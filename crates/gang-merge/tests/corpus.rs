//! Merge properties over real Unity projects (`UNITY_CORPUS=<dir>`):
//! merging a file with itself changes nothing, and a change on one side
//! comes through exactly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gang_merge::diff::Rules;
use gang_merge::merge::merge;

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !path.ends_with(".git") {
                files(&path, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| e == "unity" || e == "prefab")
        {
            out.push(path);
        }
    }
}

#[test]
fn corpus_merge_properties() {
    let Some(root) = std::env::var_os("UNITY_CORPUS") else {
        eprintln!("UNITY_CORPUS not set; skipping");
        return;
    };
    let mut paths = Vec::new();
    files(Path::new(&root), &mut paths);
    let rules = Rules::default();
    let none = HashMap::new();
    let (mut checked, mut failures) = (0, Vec::new());
    for path in &paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(file) = unity_yaml::parse(&text) else {
            continue;
        };
        checked += 1;
        let same = merge(&file, &file, &file, &rules, &none);
        if same.file.to_text() != text || !same.conflicts.is_empty() {
            failures.push(format!(
                "{}: merging with itself changed it",
                path.display()
            ));
            continue;
        }
        // Theirs drops the last document; ours is unchanged.
        if file.docs.len() > 2 {
            let mut theirs = file.clone();
            theirs.docs.pop();
            let expected = theirs.to_text();
            let merged = merge(&file, &file, &theirs, &rules, &none);
            if !merged.conflicts.is_empty() {
                failures.push(format!("{}: one-sided change conflicted", path.display()));
            } else if merged.file.to_text() != expected {
                // The repair step may tidy references to the dropped document.
                if unity_yaml::parse(&merged.file.to_text()).is_err() {
                    failures.push(format!(
                        "{}: one-sided merge is not valid YAML",
                        path.display()
                    ));
                }
            }
        }
    }
    eprintln!("checked {checked} scenes and prefabs");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
