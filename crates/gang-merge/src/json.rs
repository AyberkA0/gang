//! 3-way merge of JSON files such as `Packages/manifest.json` and
//! assembly definitions (DESIGN.md §14.3): objects key by key, arrays item
//! by item (diff3), everything else as a value.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::merge::Choice;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonConflict {
    /// `dependencies/com.unity.inputsystem`
    pub path: String,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}

pub struct JsonMerged {
    pub text: String,
    pub conflicts: Vec<JsonConflict>,
}

/// Merges three JSON texts. Fails when one of them is not JSON.
pub fn merge_json(
    base: &str,
    ours: &str,
    theirs: &str,
    decisions: &HashMap<String, Choice>,
) -> Result<JsonMerged, serde_json::Error> {
    let parse = |text: &str| -> Result<Value, serde_json::Error> {
        if text.trim().is_empty() {
            Ok(Value::Object(Map::new()))
        } else {
            serde_json::from_str(text)
        }
    };
    let (b, o, t) = (parse(base)?, parse(ours)?, parse(theirs)?);
    let mut conflicts = Vec::new();
    let merged = merge_value("", Some(&b), Some(&o), Some(&t), decisions, &mut conflicts)
        .unwrap_or(Value::Null);
    // Written the way the file was: Unity indents assembly definitions
    // with four spaces and the package manifest with two.
    // (JSON strings hold no raw line breaks, so lines can be re-indented.)
    let indent = indent_of(ours);
    let pretty = serde_json::to_string_pretty(&merged)?;
    let mut text = String::with_capacity(pretty.len());
    for line in pretty.lines() {
        let rest = line.trim_start_matches(' ');
        let depth = (line.len() - rest.len()) / 2;
        text.push_str(&indent.repeat(depth));
        text.push_str(rest);
        text.push('\n');
    }
    Ok(JsonMerged { text, conflicts })
}

/// The indentation of the first indented line, two spaces by default.
fn indent_of(text: &str) -> String {
    text.lines()
        .map(|line| {
            let rest = line.trim_start_matches([' ', '\t']);
            &line[..line.len() - rest.len()]
        })
        .find(|indent| !indent.is_empty())
        .unwrap_or("  ")
        .to_owned()
}

/// Longest common subsequence of two lists: `(a index, b index)` pairs.
fn lcs(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut pairs = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Three-way merge of two arrays against their base (diff3, as git does
/// for lines): references added on both sides are both kept. `None` when
/// both sides changed the same stretch differently.
fn diff3(base: &[Value], ours: &[Value], theirs: &[Value]) -> Option<Vec<Value>> {
    if base.len().max(ours.len()).max(theirs.len()) > 2000 {
        return None;
    }
    let keys = |items: &[Value]| -> Vec<String> { items.iter().map(Value::to_string).collect() };
    let (base_keys, our_keys, their_keys) = (keys(base), keys(ours), keys(theirs));
    let in_ours: HashMap<usize, usize> = lcs(&base_keys, &our_keys).into_iter().collect();
    let in_theirs: HashMap<usize, usize> = lcs(&base_keys, &their_keys).into_iter().collect();
    let mut out = Vec::new();
    let (mut bi, mut oi, mut ti) = (0usize, 0usize, 0usize);
    loop {
        let stable = (bi..base.len()).find(|l| {
            in_ours.get(l).is_some_and(|&o| o >= oi) && in_theirs.get(l).is_some_and(|&t| t >= ti)
        });
        let (b_end, o_end, t_end) = match stable {
            Some(l) => (l, in_ours[&l], in_theirs[&l]),
            None => (base.len(), ours.len(), theirs.len()),
        };
        let base_part = &base_keys[bi..b_end];
        let our_part = &our_keys[oi..o_end];
        let their_part = &their_keys[ti..t_end];
        if our_part == base_part {
            out.extend_from_slice(&theirs[ti..t_end]);
        } else if their_part == base_part || our_part == their_part {
            out.extend_from_slice(&ours[oi..o_end]);
        } else if base_part.is_empty() {
            // Both added at the same place (two references at the end of
            // the list): both are kept, ours first, without duplicates.
            out.extend_from_slice(&ours[oi..o_end]);
            for (value, key) in theirs[ti..t_end].iter().zip(their_part) {
                if !our_part.contains(key) {
                    out.push(value.clone());
                }
            }
        } else {
            return None;
        }
        match stable {
            Some(_) => {
                out.push(ours[o_end].clone());
                bi = b_end + 1;
                oi = o_end + 1;
                ti = t_end + 1;
            }
            None => return Some(out),
        }
    }
}

fn merge_value(
    path: &str,
    b: Option<&Value>,
    o: Option<&Value>,
    t: Option<&Value>,
    decisions: &HashMap<String, Choice>,
    conflicts: &mut Vec<JsonConflict>,
) -> Option<Value> {
    if o == t {
        return o.cloned();
    }
    if b == o {
        return t.cloned();
    }
    if b == t {
        return o.cloned();
    }
    if let (Some(Value::Object(om)), Some(Value::Object(tm))) = (o, t) {
        let bm = b.and_then(Value::as_object);
        let mut out = Map::new();
        for (key, ov) in om {
            let child = join(path, key);
            if let Some(v) = merge_value(
                &child,
                bm.and_then(|m| m.get(key)),
                Some(ov),
                tm.get(key),
                decisions,
                conflicts,
            ) {
                out.insert(key.clone(), v);
            }
        }
        for (key, tv) in tm {
            if om.contains_key(key) {
                continue;
            }
            let child = join(path, key);
            if let Some(v) = merge_value(
                &child,
                bm.and_then(|m| m.get(key)),
                None,
                Some(tv),
                decisions,
                conflicts,
            ) {
                out.insert(key.clone(), v);
            }
        }
        return Some(Value::Object(out));
    }
    if let (Some(Value::Array(ba)), Some(Value::Array(oa)), Some(Value::Array(ta))) = (b, o, t) {
        if let Some(merged) = diff3(ba, oa, ta) {
            return Some(Value::Array(merged));
        }
    }
    let show = |v: Option<&Value>| v.map(|v| v.to_string());
    conflicts.push(JsonConflict {
        path: path.to_owned(),
        base: show(b),
        ours: show(o),
        theirs: show(t),
    });
    match decisions.get(path) {
        Some(Choice::Theirs) => t.cloned(),
        Some(Choice::Base) => b.cloned(),
        Some(Choice::Value(text)) => serde_json::from_str(text)
            .ok()
            .or_else(|| Some(Value::String(text.clone()))),
        _ => o.cloned(),
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}/{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
  "dependencies": {
    "com.unity.ugui": "2.0.0",
    "com.unity.inputsystem": "1.7.0"
  }
}
"#;

    #[test]
    fn packages_added_on_both_sides() {
        let ours = BASE.replace(
            "\"com.unity.ugui\": \"2.0.0\",",
            "\"com.unity.ugui\": \"2.0.0\",\n    \"com.unity.cinemachine\": \"3.1.0\",",
        );
        let theirs = BASE.replace(
            "\"com.unity.inputsystem\": \"1.7.0\"",
            "\"com.unity.inputsystem\": \"1.7.0\",\n    \"com.unity.addressables\": \"2.2.2\"",
        );
        let merged = merge_json(BASE, &ours, &theirs, &HashMap::new()).unwrap();
        assert!(merged.conflicts.is_empty());
        assert_eq!(
            merged.text,
            "{\n  \"dependencies\": {\n    \"com.unity.ugui\": \"2.0.0\",\n    \"com.unity.cinemachine\": \"3.1.0\",\n    \"com.unity.inputsystem\": \"1.7.0\",\n    \"com.unity.addressables\": \"2.2.2\"\n  }\n}\n"
        );
    }

    const ASMDEF: &str = r#"{
    "name": "Game",
    "references": [
        "GUID:aaaa"
    ],
    "defineConstraints": []
}
"#;

    #[test]
    fn references_added_on_both_sides_keep_both_and_the_indent() {
        let ours = ASMDEF.replace("\"GUID:aaaa\"", "\"GUID:aaaa\",\n        \"GUID:bbbb\"");
        let theirs = ASMDEF.replace("\"GUID:aaaa\"", "\"GUID:aaaa\",\n        \"GUID:cccc\"");
        let merged = merge_json(ASMDEF, &ours, &theirs, &HashMap::new()).unwrap();
        assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
        assert_eq!(
            merged.text,
            "{\n    \"name\": \"Game\",\n    \"references\": [\n        \"GUID:aaaa\",\n        \"GUID:bbbb\",\n        \"GUID:cccc\"\n    ],\n    \"defineConstraints\": []\n}\n"
        );
        // One removed, one added elsewhere: both apply.
        let base = ASMDEF.replace("\"GUID:aaaa\"", "\"GUID:aaaa\",\n        \"GUID:dddd\"");
        let ours = base.replace("\"GUID:aaaa\",\n        ", "");
        let theirs = base.replace("\"GUID:dddd\"", "\"GUID:dddd\",\n        \"GUID:eeee\"");
        let merged = merge_json(&base, &ours, &theirs, &HashMap::new()).unwrap();
        assert!(merged.conflicts.is_empty());
        assert!(
            merged
                .text
                .contains("[\n        \"GUID:dddd\",\n        \"GUID:eeee\"\n    ]"),
            "{}",
            merged.text
        );
        // The same item replaced differently on both sides still asks.
        let ours = ASMDEF.replace("GUID:aaaa", "GUID:1111");
        let theirs = ASMDEF.replace("GUID:aaaa", "GUID:2222");
        let merged = merge_json(ASMDEF, &ours, &theirs, &HashMap::new()).unwrap();
        assert_eq!(merged.conflicts.len(), 1);
        assert_eq!(merged.conflicts[0].path, "references");
    }

    #[test]
    fn same_package_two_versions() {
        let ours = BASE.replace("1.7.0", "1.8.0");
        let theirs = BASE.replace("1.7.0", "1.9.0");
        let merged = merge_json(BASE, &ours, &theirs, &HashMap::new()).unwrap();
        assert_eq!(merged.conflicts.len(), 1);
        assert_eq!(
            merged.conflicts[0].path,
            "dependencies/com.unity.inputsystem"
        );
        assert!(merged.text.contains("1.8.0"));
        let decisions = HashMap::from([(
            "dependencies/com.unity.inputsystem".to_owned(),
            Choice::Theirs,
        )]);
        assert!(
            merge_json(BASE, &ours, &theirs, &decisions)
                .unwrap()
                .text
                .contains("1.9.0")
        );
    }
}
