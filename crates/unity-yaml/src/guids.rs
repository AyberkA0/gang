//! GUID → asset path, from `.meta` files (DESIGN.md §11.3).

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct GuidIndex {
    paths: HashMap<String, String>,
    duplicates: Vec<(String, Vec<String>)>,
}

impl GuidIndex {
    /// Scans `Assets/` and `Packages/` of a Unity project. Paths are stored
    /// relative to `base` with `/` separators.
    pub fn scan(project_dir: &Path, base: &Path) -> GuidIndex {
        let mut index = GuidIndex::default();
        let mut seen: HashMap<String, Vec<String>> = HashMap::new();
        for folder in ["Assets", "Packages"] {
            walk(&project_dir.join(folder), &mut |meta| {
                if let Some(guid) = read_guid(meta) {
                    let asset = meta.with_extension("");
                    let relative = asset
                        .strip_prefix(base)
                        .unwrap_or(&asset)
                        .to_string_lossy()
                        .replace('\\', "/");
                    seen.entry(guid).or_default().push(relative);
                }
            });
        }
        for (guid, mut paths) in seen {
            paths.sort();
            if paths.len() > 1 {
                index.duplicates.push((guid.clone(), paths.clone()));
            }
            index.paths.insert(guid, paths.remove(0));
        }
        index
    }

    /// Scans every `.meta` below a folder (for example one package in
    /// `Library/PackageCache`).
    pub fn scan_folder(folder: &Path, base: &Path) -> GuidIndex {
        let mut index = GuidIndex::default();
        walk(folder, &mut |meta| {
            if let Some(guid) = read_guid(meta) {
                let asset = meta.with_extension("");
                let relative = asset
                    .strip_prefix(base)
                    .unwrap_or(&asset)
                    .to_string_lossy()
                    .replace('\\', "/");
                index.paths.entry(guid).or_insert(relative);
            }
        });
        index
    }

    /// Every `(guid, path)`.
    pub fn entries(&self) -> Vec<(String, String)> {
        let mut entries: Vec<(String, String)> = self
            .paths
            .iter()
            .map(|(g, p)| (g.clone(), p.clone()))
            .collect();
        entries.sort();
        entries
    }

    /// An index from `(guid, path)` pairs.
    pub fn from_entries(entries: Vec<(String, String)>) -> GuidIndex {
        GuidIndex {
            paths: entries.into_iter().collect(),
            duplicates: Vec::new(),
        }
    }

    /// Adds another project's assets.
    pub fn extend(&mut self, other: GuidIndex) {
        self.duplicates.extend(other.duplicates);
        for (guid, path) in other.paths {
            self.paths.entry(guid).or_insert(path);
        }
    }

    /// The asset with this GUID.
    pub fn path(&self, guid: &str) -> Option<&str> {
        self.paths.get(guid).map(String::as_str)
    }

    /// The asset's file name without extension: a script's class name, a
    /// prefab's name.
    pub fn name(&self, guid: &str) -> Option<String> {
        let path = self.path(guid)?;
        let file = path.rsplit('/').next().unwrap_or(path);
        Some(
            file.rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(file)
                .to_owned(),
        )
    }

    /// GUIDs used by more than one asset.
    pub fn duplicates(&self) -> &[(String, Vec<String>)] {
        &self.duplicates
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

fn walk(dir: &Path, found: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&path, found);
        } else if path.extension().is_some_and(|e| e == "meta") {
            found(&path);
        }
    }
}

fn read_guid(meta: &Path) -> Option<String> {
    let file = fs::File::open(meta).ok()?;
    BufReader::new(file)
        .lines()
        .take(5)
        .map_while(Result::ok)
        .find_map(|line| line.strip_prefix("guid: ").map(|g| g.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_metas() {
        let dir = tempfile_dir();
        let write = |path: &str, text: &str| {
            let full = dir.join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        };
        write(
            "game/Assets/Scripts/Player.cs.meta",
            "fileFormatVersion: 2\nguid: aaa\n",
        );
        write(
            "game/Assets/Prefabs/Bullet.prefab.meta",
            "fileFormatVersion: 2\nguid: bbb\n",
        );
        write(
            "game/Assets/Copy.prefab.meta",
            "fileFormatVersion: 2\nguid: bbb\n",
        );
        let index = GuidIndex::scan(&dir.join("game"), &dir);
        assert_eq!(index.path("aaa"), Some("game/Assets/Scripts/Player.cs"));
        assert_eq!(index.name("aaa").as_deref(), Some("Player"));
        assert_eq!(index.duplicates().len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("guids-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
