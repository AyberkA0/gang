//! Renamed serialized fields (DESIGN.md §13.10, thought experiment TE7).
//!
//! A script field renamed with `[FormerlySerializedAs("speed")] float
//! moveSpeed;` makes Unity write `moveSpeed:` into the scenes of whoever
//! renamed it, while a teammate's scenes still say `speed:`. Merged key by
//! key, the teammate's change to `speed` would be lost or turn into a
//! meaningless question. The merge driver reads the renames from both
//! sides' scripts and writes the old key under its new name in all three
//! versions before merging.

use std::collections::BTreeMap;

use unity_yaml::{Entry, File, Value};

/// `(old name, new name)` pairs declared in a C# source.
pub fn declared(source: &str) -> Vec<(String, String)> {
    let mut renames = Vec::new();
    let needle = "FormerlySerializedAs";
    let mut rest = source;
    while let Some(at) = rest.find(needle) {
        let after = &rest[at + needle.len()..];
        rest = after;
        // ("old")
        let Some(open) = after.find('"') else { break };
        let before_quote = &after[..open];
        if !before_quote.trim_start().starts_with('(') {
            continue;
        }
        let Some(close) = after[open + 1..].find('"') else {
            break;
        };
        let old = after[open + 1..open + 1 + close].to_owned();
        // The field the attribute belongs to: the last identifier before
        // the first `;` or `=` (and before any `{` of a property).
        let tail = &after[open + 1 + close..];
        let end = tail.find([';', '=', '{']).unwrap_or(tail.len());
        let declaration = &tail[..end];
        let declaration = declaration.rsplit(']').next().unwrap_or(declaration);
        let new = declaration
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .rfind(|w| !w.is_empty())
            .map(str::to_owned);
        if let Some(new) = new {
            if new != old && !old.is_empty() {
                renames.push((old, new));
            }
        }
    }
    renames
}

/// Writes old keys under their new names in every MonoBehaviour whose
/// script is in `renames` (by script GUID). Returns whether anything moved.
pub fn apply(file: &mut File, renames: &BTreeMap<String, Vec<(String, String)>>) -> bool {
    let mut changed = false;
    for doc in &mut file.docs {
        if doc.type_name() != Some("MonoBehaviour") {
            continue;
        }
        let Some(body) = doc.body_mut() else { continue };
        let Some(guid) = body
            .get("m_Script")
            .and_then(Value::as_ref)
            .and_then(|r| r.guid)
        else {
            continue;
        };
        let Some(pairs) = renames.get(&guid) else {
            continue;
        };
        for (old, new) in pairs {
            if body.get(new).is_some() {
                continue;
            }
            let Some(index) = body.entries().iter().position(|e| &e.key == old) else {
                continue;
            };
            let value = body.entries()[index].value().clone();
            body.entries_mut()[index] = Entry::new(new.clone(), value);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_renames_in_every_shape() {
        let source = r#"
using UnityEngine;
using UnityEngine.Serialization;
public class Enemy : MonoBehaviour
{
    [FormerlySerializedAs("speed")] public float moveSpeed = 3;
    [SerializeField, FormerlySerializedAs("hp")]
    int health;
    [UnityEngine.Serialization.FormerlySerializedAs( "target" )]
    [SerializeField] private Transform chaseTarget;
    // not a rename: FormerlySerializedAs is mentioned in a comment
}
"#;
        assert_eq!(
            declared(source),
            vec![
                ("speed".to_owned(), "moveSpeed".to_owned()),
                ("hp".to_owned(), "health".to_owned()),
                ("target".to_owned(), "chaseTarget".to_owned()),
            ]
        );
    }

    #[test]
    fn old_keys_take_their_new_names() {
        let text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!114 &1\nMonoBehaviour:\n  m_Script: {fileID: 11500000, guid: abc, type: 3}\n  speed: 5\n  other: 1\n";
        let mut file = unity_yaml::parse(text).unwrap();
        let renames = BTreeMap::from([(
            "abc".to_owned(),
            vec![("speed".to_owned(), "moveSpeed".to_owned())],
        )]);
        assert!(apply(&mut file, &renames));
        assert_eq!(
            file.to_text(),
            text.replace("  speed: 5\n", "  moveSpeed: 5\n")
        );
    }
}
