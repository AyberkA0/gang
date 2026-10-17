//! C# source helpers: which parts are code (not comments or strings), the
//! layer-number rewrite used by the layer remap (DESIGN.md §14.2), and the
//! type index used by lock levels (DESIGN.md §10.3).

use std::collections::{BTreeMap, BTreeSet};

/// For every byte of `source`, whether it is code (not inside a comment, a
/// string or a character literal).
pub fn code_mask(source: &str) -> Vec<bool> {
    let bytes = source.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (len, kind) = if rest.starts_with(b"//") {
            (
                rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len()),
                false,
            )
        } else if rest.starts_with(b"/*") {
            (
                rest.windows(2)
                    .skip(2)
                    .position(|w| w == b"*/")
                    .map(|p| p + 4)
                    .unwrap_or(rest.len()),
                false,
            )
        } else if rest.starts_with(b"@\"") || rest.starts_with(b"$@\"") || rest.starts_with(b"@$\"")
        {
            // Verbatim string: "" escapes a quote.
            let start = rest.iter().position(|b| *b == b'"').unwrap() + 1;
            let mut j = start;
            while j < rest.len() {
                if rest[j] == b'"' {
                    if rest.get(j + 1) == Some(&b'"') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            ((j + 1).min(rest.len()), false)
        } else if rest[0] == b'"' || rest.starts_with(b"$\"") || rest[0] == b'\'' {
            let quote = if rest[0] == b'$' { b'"' } else { rest[0] };
            let start = if rest[0] == b'$' { 2 } else { 1 };
            let mut j = start;
            while j < rest.len() && rest[j] != quote && rest[j] != b'\n' {
                if rest[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            ((j + 1).min(rest.len()), false)
        } else {
            (1, true)
        };
        if !kind {
            for m in mask.iter_mut().skip(i).take(len) {
                *m = false;
            }
        }
        i += len.max(1);
    }
    mask
}

/// The result of rewriting layer numbers in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRewrite {
    pub text: String,
    /// 1-based lines that were changed.
    pub changed: Vec<usize>,
    /// 1-based lines that mention a layer and contain the number, but in a
    /// form GANG does not recognize. A person should look at them.
    pub review: Vec<usize>,
}

/// Rewrites the layer number `from` to `to` on the given 1-based lines, where
/// it is recognizably a layer: `.layer = 8`, `.layer == 8`, `1 << 8`, or an
/// assignment to a name containing "layer" (`const int UI3DLayer = 8;`).
pub fn rewrite_layer_number(
    source: &str,
    lines: &BTreeSet<usize>,
    from: u32,
    to: u32,
) -> LayerRewrite {
    let mask = code_mask(source);
    let from_text = from.to_string();
    let mut out = String::with_capacity(source.len());
    let mut changed = Vec::new();
    let mut review = Vec::new();
    let mut offset = 0;
    for (n, line) in source.split_inclusive('\n').enumerate() {
        let number = n + 1;
        if !lines.contains(&number) {
            out.push_str(line);
            offset += line.len();
            continue;
        }
        let mut new_line = String::with_capacity(line.len());
        let mut last = 0;
        let mut touched = false;
        let mut suspicious = false;
        for (pos, _) in line.match_indices(&from_text) {
            let absolute = offset + pos;
            if !mask.get(absolute).copied().unwrap_or(false)
                || !is_whole_number(line, pos, from_text.len())
            {
                continue;
            }
            if is_layer_context(&line[..pos]) {
                new_line.push_str(&line[last..pos]);
                new_line.push_str(&to.to_string());
                last = pos + from_text.len();
                touched = true;
            } else if line.to_ascii_lowercase().contains("layer") {
                suspicious = true;
            }
        }
        new_line.push_str(&line[last..]);
        out.push_str(&new_line);
        if touched {
            changed.push(number);
        }
        if suspicious {
            review.push(number);
        }
        offset += line.len();
    }
    LayerRewrite {
        text: out,
        changed,
        review,
    }
}

fn is_whole_number(line: &str, pos: usize, len: usize) -> bool {
    let before = line[..pos].chars().next_back();
    let after = line[pos + len..].chars().next();
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
    !ident(before) && !ident(after)
}

/// Whether the text before a number makes it a layer number.
fn is_layer_context(before: &str) -> bool {
    let trimmed = before.trim_end();
    if let Some(rest) = trimmed.strip_suffix("<<") {
        return rest.trim_end().ends_with('1');
    }
    let operator = ["==", "!=", "="].iter().find(|op| trimmed.ends_with(*op));
    let Some(op) = operator else { return false };
    let target = trimmed[..trimmed.len() - op.len()].trim_end();
    let target = target.trim_end_matches(['!', '<', '>', '=']).trim_end();
    let identifier: String = target
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    identifier.to_ascii_lowercase().contains("layer")
}

/// Types each script declares, and the type names it mentions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeIndex {
    /// Script path → types it declares.
    pub declares: BTreeMap<String, BTreeSet<String>>,
    /// Script path → identifiers it uses.
    pub mentions: BTreeMap<String, BTreeSet<String>>,
}

impl TypeIndex {
    pub fn add(&mut self, path: &str, source: &str) {
        let (declared, mentioned) = scan_types(source);
        self.declares.insert(path.to_owned(), declared);
        self.mentions.insert(path.to_owned(), mentioned);
    }

    /// Scripts that declare a type `path` mentions, and scripts that mention a
    /// type `path` declares. One hop, both directions.
    pub fn neighbors(&self, path: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let empty = BTreeSet::new();
        let mine = self.declares.get(path).unwrap_or(&empty);
        let used = self.mentions.get(path).unwrap_or(&empty);
        for (other, declared) in &self.declares {
            if other != path && declared.iter().any(|t| used.contains(t)) {
                result.insert(other.clone());
            }
        }
        for (other, mentioned) in &self.mentions {
            if other != path && mine.iter().any(|t| mentioned.contains(t)) {
                result.insert(other.clone());
            }
        }
        result
    }

    /// Whether the script at `user` mentions a type declared by `script`.
    pub fn uses(&self, user: &str, script: &str) -> bool {
        let (Some(mentioned), Some(declared)) =
            (self.mentions.get(user), self.declares.get(script))
        else {
            return false;
        };
        user != script && declared.iter().any(|t| mentioned.contains(t))
    }
}

const DECLARING: &[&str] = &["class", "struct", "interface", "enum", "record"];

/// Declared type names, and every identifier used in code.
pub fn scan_types(source: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mask = code_mask(source);
    let code: String = source
        .char_indices()
        .map(|(i, c)| {
            if mask.get(i).copied().unwrap_or(true) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut declared = BTreeSet::new();
    let mut mentioned = BTreeSet::new();
    let mut previous = String::new();
    let mut word = String::new();
    for c in code.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            if !word.chars().next().unwrap().is_ascii_digit() {
                if DECLARING.contains(&previous.as_str()) {
                    declared.insert(word.clone());
                } else {
                    mentioned.insert(word.clone());
                }
            }
            previous = std::mem::take(&mut word);
        }
    }
    for name in &declared {
        mentioned.remove(name);
    }
    (declared, mentioned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_comments_and_strings() {
        let source = "int a = 8; // 8 in a comment\nvar s = \"8\"; /* 8 */ var t = @\"a\"\"8\";\n";
        let mask = code_mask(source);
        let code: String = source
            .char_indices()
            .filter(|(i, _)| mask[*i])
            .map(|(_, c)| c)
            .collect();
        assert_eq!(code.matches('8').count(), 1);
    }

    #[test]
    fn rewrites_recognized_layer_numbers_on_given_lines() {
        let source = "\
gameObject.layer = 8;
if (other.layer == 8) {}
int mask = 1 << 8;
const int UI3DLayer = 8;
SetLayerRecursively(go, 8);
var count = 8;
int mask2 = 1 << 18;
// layer 8 in a comment
";
        let all: BTreeSet<usize> = (1..=8).collect();
        let result = rewrite_layer_number(source, &all, 8, 9);
        assert_eq!(result.changed, vec![1, 2, 3, 4]);
        assert_eq!(result.review, vec![5]);
        assert!(result.text.starts_with("gameObject.layer = 9;\nif (other.layer == 9) {}\nint mask = 1 << 9;\nconst int UI3DLayer = 9;\nSetLayerRecursively(go, 8);\nvar count = 8;\nint mask2 = 1 << 18;"));

        let only_first: BTreeSet<usize> = [1].into();
        let result = rewrite_layer_number(source, &only_first, 8, 9);
        assert_eq!(result.changed, vec![1]);
        assert!(result.text.contains("other.layer == 8"));
    }

    #[test]
    fn type_index() {
        let mut index = TypeIndex::default();
        index.add(
            "Player.cs",
            "public class Player : MonoBehaviour { PlayerHealth health; }",
        );
        index.add(
            "PlayerHealth.cs",
            "public class PlayerHealth : MonoBehaviour { public event System.Action onDamaged; }",
        );
        index.add(
            "Enemy.cs",
            "class Enemy { void Hit(PlayerHealth h) { /* Player */ } string s = \"Player\"; }",
        );
        index.add("Bullet.cs", "class Bullet {}");
        assert_eq!(
            index.neighbors("PlayerHealth.cs"),
            BTreeSet::from(["Enemy.cs".to_owned(), "Player.cs".to_owned()])
        );
        assert_eq!(
            index.neighbors("Player.cs"),
            BTreeSet::from(["PlayerHealth.cs".to_owned()])
        );
        assert!(index.uses("Enemy.cs", "PlayerHealth.cs"));
        assert!(
            !index.uses("Enemy.cs", "Player.cs"),
            "mentions in comments and strings do not count"
        );
        assert!(index.neighbors("Bullet.cs").is_empty());
    }
}
