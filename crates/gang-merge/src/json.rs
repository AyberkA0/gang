//! 3-way merge of JSON files such as `Packages/manifest.json`
//! (DESIGN.md §14.3): objects key by key, everything else as a value.

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
    let mut text = serde_json::to_string_pretty(&merged)?;
    text.push('\n');
    Ok(JsonMerged { text, conflicts })
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
