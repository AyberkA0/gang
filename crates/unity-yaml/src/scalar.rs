//! Scalar values: plain, quoted, and flow mappings like `{x: 0, y: 1}`.

/// A value written on the same line as its key (or dash), exactly as it
/// appears in the file, including continuation lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scalar {
    raw: String,
}

impl Scalar {
    /// A scalar from its exact text. Continuation lines must include their
    /// indentation and be separated by `\n`.
    pub fn raw(text: impl Into<String>) -> Scalar {
        Scalar { raw: text.into() }
    }

    /// A scalar for `value`, quoted the way Unity would when needed.
    pub fn plain(value: &str) -> Scalar {
        if needs_quotes(value) {
            Scalar::raw(format!("'{}'", value.replace('\'', "''")))
        } else {
            Scalar::raw(value)
        }
    }

    /// A flow mapping, `{a: 1, b: 2}`.
    pub fn flow(entries: &[(String, String)]) -> Scalar {
        let body = entries
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", ");
        Scalar::raw(format!("{{{body}}}"))
    }

    /// The text as written.
    pub fn as_raw(&self) -> &str {
        &self.raw
    }

    /// The value: quotes removed, escapes and line folding applied.
    pub fn value(&self) -> String {
        let text = self.raw.trim();
        if let Some(inner) = text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
            return fold(inner).replace("''", "'");
        }
        if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            return unescape_double(inner);
        }
        fold(text)
    }

    /// Entries of a flow mapping (`{fileID: 0}` → `[("fileID", "0")]`), or
    /// `None` when the scalar is not one.
    pub fn flow_entries(&self) -> Option<Vec<(String, String)>> {
        let text = fold(self.raw.trim());
        let inner = text.strip_prefix('{')?.strip_suffix('}')?;
        if inner.trim().is_empty() {
            return Some(Vec::new());
        }
        split_top_level(inner)
            .into_iter()
            .map(|part| {
                let (key, value) = part.split_once(':')?;
                Some((key.trim().to_owned(), value.trim().to_owned()))
            })
            .collect()
    }

    /// One field of a flow mapping.
    pub fn flow_get(&self, key: &str) -> Option<String> {
        self.flow_entries()?
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// Joins continuation lines the way YAML folds plain and single-quoted
/// scalars: a line break becomes a space, an empty line a line break.
fn fold(text: &str) -> String {
    if !text.contains('\n') {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut pending_breaks = 0;
    for (i, line) in text.split('\n').enumerate() {
        let line = line.trim();
        if i == 0 {
            out.push_str(line);
            continue;
        }
        if line.is_empty() {
            pending_breaks += 1;
            continue;
        }
        if pending_breaks > 0 {
            out.push_str(&"\n".repeat(pending_breaks));
            pending_breaks = 0;
        } else {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

fn unescape_double(inner: &str) -> String {
    // Escaped line breaks join lines without a space.
    let mut joined = String::new();
    let mut lines = inner.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line = if joined.is_empty() {
            line
        } else {
            line.trim_start()
        };
        if lines.peek().is_some() {
            if let Some(stripped) = line.strip_suffix('\\') {
                joined.push_str(stripped);
            } else {
                joined.push_str(line.trim_end());
                joined.push(' ');
            }
        } else {
            joined.push_str(line);
        }
    }
    let mut out = String::new();
    let mut chars = joined.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some(' ') => out.push(' '),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(c);
                }
            }
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(c);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits `a: 1, b: {c: 2}` at top-level commas.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '{' | '[') => depth += 1,
            (None, '}' | ']') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn needs_quotes(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let first = value.chars().next().unwrap();
    value.starts_with(' ')
        || value.ends_with(' ')
        || "{[&*!|>'\"%@`#,?-".contains(first)
            && !(first == '-'
                && value[1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit()))
        || value.contains(": ")
        || value.contains(" #")
        || value.contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(Scalar::raw("Player").value(), "Player");
        assert_eq!(Scalar::raw("'It''s'").value(), "It's");
        assert_eq!(Scalar::raw("\"a\\tb\\u00e7\"").value(), "a\tbç");
        assert_eq!(
            Scalar::raw("long text\n      continues here").value(),
            "long text continues here"
        );
        assert_eq!(
            Scalar::raw("'para one\n\n    para two'").value(),
            "para one\npara two"
        );
    }

    #[test]
    fn flow_mappings() {
        let s = Scalar::raw("{fileID: 11500000, guid: abc, type: 3}");
        assert_eq!(s.flow_get("guid").as_deref(), Some("abc"));
        assert_eq!(s.flow_entries().unwrap().len(), 3);
        assert_eq!(Scalar::raw("{}").flow_entries(), Some(vec![]));
        assert_eq!(Scalar::raw("Player").flow_entries(), None);
        let built = Scalar::flow(&[("x".into(), "0".into()), ("y".into(), "1.5".into())]);
        assert_eq!(built.as_raw(), "{x: 0, y: 1.5}");
    }

    #[test]
    fn quoting() {
        assert_eq!(Scalar::plain("Player").as_raw(), "Player");
        assert_eq!(Scalar::plain("-1.5").as_raw(), "-1.5");
        assert_eq!(Scalar::plain("a: b").as_raw(), "'a: b'");
        assert_eq!(Scalar::plain("{x}").as_raw(), "'{x}'");
        assert_eq!(Scalar::plain("it's: x").as_raw(), "'it''s: x'");
    }
}
