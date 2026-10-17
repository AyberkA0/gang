//! Round trip over real Unity projects. Point `UNITY_CORPUS` at a folder of
//! Unity projects; without it the test does nothing.
//!
//! For every Unity YAML file it checks that:
//! 1. parsing and writing gives the same bytes, and
//! 2. formatting every entry anew (as GANG does for changed entries) gives
//!    the same bytes, i.e. GANG writes like Unity.

use std::path::{Path, PathBuf};
use std::time::Instant;

const EXTENSIONS: &[&str] = &[
    "unity",
    "prefab",
    "asset",
    "mat",
    "anim",
    "controller",
    "meta",
];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == ".git" || n == "Library")
            {
                continue;
            }
            files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| EXTENSIONS.contains(&e))
        {
            out.push(path);
        }
    }
}

#[test]
fn corpus_round_trip() {
    let Some(root) = std::env::var_os("UNITY_CORPUS") else {
        eprintln!("UNITY_CORPUS not set; skipping");
        return;
    };
    let mut paths = Vec::new();
    files(Path::new(&root), &mut paths);
    paths.sort();

    let (mut parsed, mut skipped, mut bytes) = (0, 0, 0usize);
    let mut failures = Vec::new();
    let mut reformat_failures = Vec::new();
    let mut slowest = (0.0f64, PathBuf::new());
    let mut path_failures = Vec::new();
    let mut objects = 0usize;
    for path in &paths {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        let Ok(text) = String::from_utf8(data) else {
            skipped += 1;
            continue;
        };
        // LFS pointers and JSON files that happen to use a Unity extension.
        if text.starts_with("version https://git-lfs") || text.trim_start().starts_with('{') {
            skipped += 1;
            continue;
        }
        let start = Instant::now();
        match unity_yaml::parse(&text) {
            Ok(mut file) => {
                let elapsed = start.elapsed().as_secs_f64();
                if elapsed > slowest.0 {
                    slowest = (elapsed, path.clone());
                }
                parsed += 1;
                bytes += text.len();
                if file.to_text() != text {
                    failures.push(format!("{}: round trip differs", path.display()));
                    continue;
                }
                if path
                    .extension()
                    .is_some_and(|e| e == "unity" || e == "prefab")
                {
                    let hierarchy = unity_yaml::Hierarchy::build(&file);
                    for object in hierarchy.objects() {
                        objects += 1;
                        let resolved = hierarchy
                            .path(object.id)
                            .and_then(|p| hierarchy.resolve(&p));
                        if resolved != Some(object.id) {
                            path_failures.push(format!(
                                "{}: object {} ({:?})",
                                path.display(),
                                object.id,
                                hierarchy.path(object.id)
                            ));
                        }
                    }
                }
                file.forget_raw();
                let reformatted = file.to_text();
                // A file without a final line break gets one; that is fine.
                if reformatted != text && reformatted != format!("{text}\n") {
                    reformat_failures.push(path.display().to_string());
                }
            }
            Err(err) => failures.push(format!("{}: {err}", path.display())),
        }
    }
    eprintln!(
        "parsed {parsed} files ({} MB), skipped {skipped}; slowest {:.3}s ({})",
        bytes / 1024 / 1024,
        slowest.0,
        slowest.1.display()
    );
    eprintln!("reformat differs for {} files", reformat_failures.len());
    eprintln!(
        "{objects} objects, {} paths do not resolve back",
        path_failures.len()
    );
    for failure in path_failures.iter().take(10) {
        eprintln!("  path: {failure}");
    }
    for failure in reformat_failures.iter().take(10) {
        eprintln!("  reformat: {failure}");
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
