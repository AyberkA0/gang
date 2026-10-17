//! Parsing Unity YAML into the lossless tree.

use std::fmt;

use crate::model::{Doc, Entry, File, Item, Map, Value};
use crate::scalar::Scalar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parses a Unity YAML file (or a plain YAML file such as a `.meta`).
pub fn parse(text: &str) -> Result<File, ParseError> {
    if text.contains('\0') {
        return Err(ParseError {
            line: 1,
            message: "binary content".to_owned(),
        });
    }
    let lines = split_lines(text);
    let headers: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.indent == 0 && l.content.starts_with("--- "))
        .map(|(i, _)| i)
        .collect();

    let parser = Parser {
        text,
        lines: &lines,
    };
    if headers.is_empty() {
        let first = lines
            .iter()
            .position(|l| !l.is_blank() && !l.content.starts_with('#'));
        let prefix_end = first.map(|i| lines[i].start).unwrap_or(text.len());
        let mut i = first.unwrap_or(lines.len());
        let mut root = parser.map(&mut i, lines.len(), 0)?;
        parser.expect_end(i, lines.len())?;
        keep_trailing(&mut root, parser.source(i, lines.len()));
        return Ok(File {
            prefix: text[..prefix_end].to_owned(),
            docs: vec![Doc {
                header: None,
                class_id: None,
                file_id: None,
                stripped: false,
                root,
            }],
        });
    }

    let prefix = text[..lines[headers[0]].start].to_owned();
    let mut docs = Vec::with_capacity(headers.len());
    for (n, &header) in headers.iter().enumerate() {
        let end = headers.get(n + 1).copied().unwrap_or(lines.len());
        let line = &lines[header];
        let (class_id, file_id, stripped) = parse_header(line.content);
        let mut i = header + 1;
        let mut root = parser.map(&mut i, end, 0)?;
        parser.expect_end(i, end)?;
        keep_trailing(&mut root, parser.source(i, end));
        docs.push(Doc {
            header: Some(text[line.start..line.end].to_owned()),
            class_id,
            file_id,
            stripped,
            root,
        });
    }
    Ok(File { prefix, docs })
}

/// Blank lines at the end of a document stay with its last entry.
fn keep_trailing(root: &mut Map, text: &str) {
    if !text.is_empty() {
        if let Some(last) = root.entries_mut().last_mut() {
            last.append_raw(text);
        }
    }
}

/// `--- !u!1 &4821937561 stripped`
fn parse_header(content: &str) -> (Option<u32>, Option<i64>, bool) {
    let mut class_id = None;
    let mut file_id = None;
    let mut stripped = false;
    for word in content.split_whitespace().skip(1) {
        if let Some(class) = word.strip_prefix("!u!") {
            class_id = class.parse().ok();
        } else if let Some(id) = word.strip_prefix('&') {
            file_id = id.parse().ok();
        } else if word == "stripped" {
            stripped = true;
        }
    }
    (class_id, file_id, stripped)
}

struct Line<'a> {
    /// Byte offsets of the line in the text, line break included in `end`.
    start: usize,
    end: usize,
    indent: usize,
    /// The line without indentation and line break.
    content: &'a str,
}

impl Line<'_> {
    fn is_blank(&self) -> bool {
        self.content.trim().is_empty()
    }

    fn is_dash(&self) -> bool {
        self.content == "-" || self.content.starts_with("- ")
    }
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = text[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(text.len());
        let full = &text[start..end];
        let bare = full.trim_end_matches('\n').trim_end_matches('\r');
        let indent = bare.len() - bare.trim_start_matches(' ').len();
        lines.push(Line {
            start,
            end,
            indent,
            content: &bare[indent..],
        });
        start = end;
    }
    lines
}

struct Parser<'a> {
    text: &'a str,
    lines: &'a [Line<'a>],
}

impl<'a> Parser<'a> {
    fn error<T>(&self, i: usize, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            line: i + 1,
            message: message.into(),
        })
    }

    fn expect_end(&self, i: usize, end: usize) -> Result<(), ParseError> {
        match (i..end).find(|&j| !self.lines[j].is_blank()) {
            Some(j) => self.error(j, format!("unexpected line {:?}", self.lines[j].content)),
            None => Ok(()),
        }
    }

    /// Source text of lines `from..to`.
    fn source(&self, from: usize, to: usize) -> &'a str {
        if from >= to {
            return "";
        }
        &self.text[self.lines[from].start..self.lines[to - 1].end]
    }

    /// The first non-blank line at or after `i`, before `end`.
    fn next_content(&self, i: usize, end: usize) -> Option<usize> {
        (i..end).find(|&j| !self.lines[j].is_blank())
    }

    /// A block mapping whose keys are at `indent`.
    fn map(&self, i: &mut usize, end: usize, indent: usize) -> Result<Map, ParseError> {
        let mut entries: Vec<Entry> = Vec::new();
        while *i < end {
            let line = &self.lines[*i];
            if line.is_blank() {
                // Blank lines between entries stay with the entry before.
                match self.next_content(*i, end) {
                    Some(next) if self.lines[next].indent >= indent && !entries.is_empty() => {
                        let blanks = self.source(*i, next);
                        entries.last_mut().unwrap().append_raw(blanks);
                        *i = next;
                        continue;
                    }
                    _ => break,
                }
            }
            if line.indent < indent || (line.indent == indent && line.is_dash()) {
                break;
            }
            if line.indent > indent {
                return self.error(*i, format!("unexpected indentation in {:?}", line.content));
            }
            let start = *i;
            let entry = self.entry(i, end, indent, None)?;
            let _ = start;
            entries.push(entry);
        }
        Ok(Map::new(entries))
    }

    /// One `key: value` entry starting at line `*i`. `first` replaces the
    /// first line's text (for the first entry of a sequence item, written
    /// after `- `); its raw text is then the line as if it stood alone at
    /// `indent`.
    fn entry(
        &self,
        i: &mut usize,
        end: usize,
        indent: usize,
        first: Option<&'a str>,
    ) -> Result<Entry, ParseError> {
        let start = *i;
        let content = first.unwrap_or(self.lines[start].content);
        let Some((key, rest, explicit_space)) = split_key(content) else {
            return self.error(start, format!("expected `key: value`, found {:?}", content));
        };
        *i += 1;

        let value = if !rest.is_empty() {
            Value::Scalar(self.scalar(rest, i, end, indent))
        } else {
            match self.next_content(*i, end) {
                Some(next) if self.lines[next].indent == indent && self.lines[next].is_dash() => {
                    *i = next;
                    Value::Seq(self.seq(i, end, indent)?)
                }
                Some(next) if self.lines[next].indent > indent => {
                    *i = next;
                    let child = self.lines[next].indent;
                    if self.lines[next].is_dash() {
                        Value::Seq(self.seq(i, end, child)?)
                    } else {
                        Value::Map(self.map(i, end, child)?)
                    }
                }
                _ if explicit_space => Value::Scalar(Scalar::raw("")),
                _ => Value::Empty,
            }
        };

        let raw = match first {
            None => self.source(start, *i).to_owned(),
            Some(text) => {
                let line = &self.lines[start];
                let ending = &self.text[line.start + line.indent + line.content.len()..line.end];
                format!(
                    "{}{text}{ending}{}",
                    " ".repeat(indent),
                    self.source(start + 1, *i)
                )
            }
        };
        Ok(Entry::parsed(key.to_owned(), value, raw))
    }

    /// A scalar that starts with `first` on the current line and may continue
    /// on following lines.
    fn scalar(&self, first: &str, i: &mut usize, end: usize, indent: usize) -> Scalar {
        let mut text = first.to_owned();
        while *i < end {
            let line = &self.lines[*i];
            let open = unclosed(&text);
            let continues = if open {
                true
            } else if line.is_blank() {
                self.next_content(*i, end).is_some_and(|next| {
                    self.lines[next].indent > indent && !starts_structure(&self.lines[next], indent)
                })
            } else {
                line.indent > indent && !starts_structure(line, indent)
            };
            if !continues {
                break;
            }
            text.push('\n');
            let full = &self.text[line.start..line.end];
            text.push_str(full.trim_end_matches('\n').trim_end_matches('\r'));
            *i += 1;
        }
        Scalar::raw(text)
    }

    /// A block sequence whose dashes are at `indent`.
    fn seq(&self, i: &mut usize, end: usize, indent: usize) -> Result<Vec<Item>, ParseError> {
        self.seq_from(i, end, indent, None)
    }

    /// A block sequence; `first` replaces the first line's text (a sequence
    /// nested on a dash line: `- - item`).
    fn seq_from(
        &self,
        i: &mut usize,
        end: usize,
        indent: usize,
        mut first: Option<&'a str>,
    ) -> Result<Vec<Item>, ParseError> {
        let mut items: Vec<Item> = Vec::new();
        while *i < end {
            let line = &self.lines[*i];
            let virtual_first = first.take();
            if line.is_blank() {
                match self.next_content(*i, end) {
                    Some(next)
                        if self.lines[next].indent == indent
                            && self.lines[next].is_dash()
                            && !items.is_empty() =>
                    {
                        let blanks = self.source(*i, next);
                        items.last_mut().unwrap().append_raw(blanks);
                        *i = next;
                        continue;
                    }
                    _ => break,
                }
            }
            let content = virtual_first.unwrap_or(line.content);
            let is_dash = content == "-" || content.starts_with("- ");
            if virtual_first.is_none() && (line.indent != indent || !is_dash) {
                break;
            }
            let start = *i;
            let after = content
                .strip_prefix('-')
                .unwrap_or("")
                .strip_prefix(' ')
                .unwrap_or("");
            let value = if after == "-" || after.starts_with("- ") {
                // `- - item`: a sequence nested on the dash line.
                Value::Seq(self.seq_from(i, end, indent + 2, Some(after))?)
            } else if after.trim().is_empty() {
                *i += 1;
                match self.next_content(*i, end) {
                    Some(next) if self.lines[next].indent > indent => {
                        *i = next;
                        let child = self.lines[next].indent;
                        if self.lines[next].is_dash() {
                            Value::Seq(self.seq(i, end, child)?)
                        } else {
                            Value::Map(self.map(i, end, child)?)
                        }
                    }
                    // `- ` is an empty string; a bare `-` is empty.
                    _ if content.starts_with("- ") => Value::Scalar(Scalar::raw(after)),
                    _ => Value::Empty,
                }
            } else if looks_like_key(after) {
                let item_indent = indent + 2;
                let mut entries = vec![self.entry(i, end, item_indent, Some(after))?];
                let rest = self.map(i, end, item_indent)?;
                entries.extend(rest.entries().iter().cloned());
                Value::Map(Map::new(entries))
            } else {
                *i += 1;
                Value::Scalar(self.scalar(after, i, end, indent))
            };
            let raw = match virtual_first {
                None => self.source(start, *i).to_owned(),
                Some(text) => {
                    let ending =
                        &self.text[line.start + line.indent + line.content.len()..line.end];
                    format!(
                        "{}{text}{ending}{}",
                        " ".repeat(indent),
                        self.source(start + 1, *i)
                    )
                }
            };
            items.push(Item::parsed(value, raw));
        }
        Ok(items)
    }
}

/// A line that starts a new entry or item rather than continuing a scalar.
/// Nothing can nest under an inline value, so every deeper line continues it,
/// even one that starts with `- `.
fn starts_structure(line: &Line<'_>, indent: usize) -> bool {
    line.indent <= indent
}

/// `key: value` → `(key, value, true)`; `key:` → `(key, "", false)`.
fn split_key(content: &str) -> Option<(&str, &str, bool)> {
    if let Some(pos) = content.find(": ") {
        return Some((&content[..pos], &content[pos + 2..], true));
    }
    content.strip_suffix(':').map(|key| (key, "", false))
}

/// Whether a sequence item's text is a mapping entry (`component: {…}`)
/// rather than a scalar (`{fileID: 1}`, `'a: b'`).
fn looks_like_key(text: &str) -> bool {
    !text.starts_with(['{', '[', '\'', '"']) && (text.contains(": ") || text.ends_with(':'))
}

/// Whether a quoted scalar or flow collection is still open.
fn unclosed(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    match chars.next() {
        Some('\'') => {
            while let Some(c) = chars.next() {
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                    } else {
                        return false;
                    }
                }
            }
            true
        }
        Some('"') => {
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        chars.next();
                    }
                    '"' => return false,
                    _ => {}
                }
            }
            true
        }
        Some('{' | '[') => {
            let mut depth = 1;
            let mut quote: Option<char> = None;
            for c in chars {
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, '\'' | '"') => quote = Some(c),
                    (None, '{' | '[') => depth += 1,
                    (None, '}' | ']') => {
                        depth -= 1;
                        if depth == 0 {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENE: &str = "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!1 &4821937561
GameObject:
  m_ObjectHideFlags: 0
  m_Component:
  - component: {fileID: 4821937562}
  - component: {fileID: 4821937563}
  m_Layer: 0
  m_Name: Player
--- !u!4 &4821937562
Transform:
  m_GameObject: {fileID: 4821937561}
  m_LocalPosition: {x: 0, y: 1, z: 0}
  m_Children: []
  m_Father: {fileID: 0}
--- !u!114 &4821937563 stripped
MonoBehaviour:
  m_Script: {fileID: 11500000, guid: 4f231c4fb786f3946a6b90b886c48677, type: 3}
  m_Text: 'a long text that Unity
    wraps onto the next line'
  m_Modifications:
  - target: {fileID: 1}
    propertyPath: m_Name
    value: Enemy
    objectReference: {fileID: 0}
  - target: {fileID: 2}
    propertyPath: m_LocalPosition.x
    value: 4
    objectReference: {fileID: 0}
  m_Empty:
  m_Nested:
    deeper:
      deepest: 1
  4:3: 0
";

    /// The fixture, with the trailing space Unity writes after an empty
    /// string (editors tend to strip it from source files).
    fn scene() -> String {
        SCENE.replace("  m_Empty:\n", "  m_Empty: \n")
    }

    #[test]
    fn round_trip() {
        let file = parse(&scene()).unwrap();
        assert_eq!(file.to_text(), scene());
        let file = parse(SCENE).unwrap();
        assert_eq!(file.to_text(), SCENE);
    }

    #[test]
    fn documents_and_values() {
        let file = parse(&scene()).unwrap();
        assert_eq!(file.docs.len(), 3);
        let go = &file.docs[0];
        assert_eq!(
            (go.class_id, go.file_id, go.type_name()),
            (Some(1), Some(4821937561), Some("GameObject"))
        );
        let body = go.body().unwrap();
        assert_eq!(
            body.get("m_Name").unwrap().text().as_deref(),
            Some("Player")
        );
        let components = body.get("m_Component").unwrap().as_seq().unwrap();
        assert_eq!(components.len(), 2);
        let first = components[0].value().as_map().unwrap();
        assert_eq!(
            first.get("component").unwrap().as_ref().unwrap().file_id,
            4821937562
        );

        let mono = &file.docs[2];
        assert!(mono.stripped);
        let body = mono.body().unwrap();
        assert_eq!(
            body.get("m_Text").unwrap().text().as_deref(),
            Some("a long text that Unity wraps onto the next line")
        );
        assert_eq!(
            body.get("m_Modifications").unwrap().as_seq().unwrap().len(),
            2
        );
        assert_eq!(body.get("m_Empty"), Some(&Value::Scalar(Scalar::raw(""))));
        assert_eq!(body.get("4:3").unwrap().text().as_deref(), Some("0"));
        assert_eq!(
            file.docs[1]
                .body()
                .unwrap()
                .get("m_Children")
                .unwrap()
                .as_seq()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn changed_entries_are_written_like_unity() {
        let mut file = parse(&scene()).unwrap();
        let body = file.docs[0].body_mut().unwrap();
        *body.get_mut("m_Name").unwrap() = Value::Scalar(Scalar::plain("Hero"));
        let text = file.to_text();
        assert_eq!(text, scene().replace("m_Name: Player", "m_Name: Hero"));

        let mut file = parse(&scene()).unwrap();
        let mods = file.docs[2]
            .body_mut()
            .unwrap()
            .get_mut("m_Modifications")
            .unwrap();
        let items = mods.as_seq_mut().unwrap();
        let mut item = items[0].clone();
        item.value_mut()
            .as_map_mut()
            .unwrap()
            .set("value", Value::Scalar(Scalar::plain("Boss")));
        items.push(item);
        let text = file.to_text();
        assert!(text.contains(
            "    objectReference: {fileID: 0}\n  - target: {fileID: 1}\n    propertyPath: m_Name\n    value: Boss\n    objectReference: {fileID: 0}\n  m_Empty: \n"
        ), "{text}");
    }

    #[test]
    fn plain_yaml_meta() {
        let meta = "fileFormatVersion: 2\nguid: 0123456789abcdef0123456789abcdef\nTextureImporter:\n  spriteSheet:\n    sprites: []\n  userData: \n";
        assert!(meta.contains("userData: \n"));
        let file = parse(meta).unwrap();
        assert_eq!(file.to_text(), meta);
        assert!(file.docs[0].header.is_none());
        assert_eq!(
            file.docs[0].root.get("guid").unwrap().text().as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn nested_sequences_and_dash_continuations() {
        let text = "outline:\n- - {x: -2, y: -2}\n  - {x: -2, y: 2}\n- - {x: 1, y: 1}\nscenes:\n- path: Assets/A Long Name\n    - Part Two.unity\n  guid: abc\n\n";
        let file = parse(text).unwrap();
        assert_eq!(file.to_text(), text);
        let outline = file.docs[0].root.get("outline").unwrap().as_seq().unwrap();
        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].value().as_seq().unwrap().len(), 2);
        let scene = file.docs[0].root.get("scenes").unwrap().as_seq().unwrap()[0]
            .value()
            .as_map()
            .unwrap();
        assert_eq!(
            scene.get("path").unwrap().text().as_deref(),
            Some("Assets/A Long Name - Part Two.unity")
        );

        let mut reformatted = file.clone();
        reformatted.forget_raw();
        assert_eq!(
            reformatted.to_text(),
            text.trim_end_matches('\n').to_owned() + "\n"
        );
    }

    #[test]
    fn rejects_non_yaml() {
        assert!(parse("{\n  \"json\": true\n}\n").is_err());
        assert!(parse("abc\0def").is_err());
    }

    #[test]
    fn no_trailing_newline_and_crlf() {
        for text in ["a: 1\nb:\n  c: 2", "a: 1\r\nb:\r\n  c: 2\r\n"] {
            assert_eq!(parse(text).unwrap().to_text(), text);
        }
    }
}
