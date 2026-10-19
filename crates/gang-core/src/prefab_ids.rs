//! Prefab identity (DESIGN.md §13.9, field test A1).
//!
//! Scenes and other prefabs point at the objects inside a prefab by the
//! prefab's GUID and the object's fileID (`{fileID: 4821937561, guid: …}`):
//! instance overrides, stripped copies, references. When a prefab is
//! rebuilt, for example turned into a variant, Unity keeps its GUID but gives
//! every object a new fileID. Every reference to the old ids silently stops
//! working: placed instances lose their overrides and fall back to the
//! prefab's values (the field test's Imps jumped to the origin).
//!
//! GANG matches the old objects to the new ones by their place in the
//! prefab (hierarchy path, component type and order) and points the
//! references at the new ids. What has no match is reported.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use unity_yaml::{File, FileId, Hierarchy};

use crate::repo::{Repo, split_nul};
use crate::semantic;

/// A prefab whose objects got new ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuild {
    pub path: String,
    pub guid: String,
    /// Old fileID → new fileID, for objects found again.
    pub map: BTreeMap<FileId, FileId>,
    /// Old fileIDs with no counterpart in the new version.
    pub lost: BTreeSet<FileId>,
    /// Old id → where it was in the prefab (`Imp/Model › Transform`).
    pub names: BTreeMap<FileId, String>,
}

/// Each document's place in the file: the owning object's path, the class,
/// and its order among the object's documents of that class.
fn places(file: &File) -> BTreeMap<FileId, String> {
    let hierarchy = Hierarchy::build(file);
    let mut seen: BTreeMap<(String, u32), usize> = BTreeMap::new();
    let mut places = BTreeMap::new();
    for doc in &file.docs {
        let (Some(id), Some(class)) = (doc.file_id, doc.class_id) else {
            continue;
        };
        let owner = hierarchy.owner(id).unwrap_or(id);
        let Some(path) = hierarchy.path(owner) else {
            continue;
        };
        let n = seen.entry((path.clone(), class)).or_insert(0);
        places.insert(id, format!("{path}\u{1}{class}\u{1}{n}"));
        *n += 1;
    }
    places
}

fn describe(place: &str, file: &File, id: FileId) -> String {
    let path = place.split('\u{1}').next().unwrap_or(place);
    let type_name = file
        .doc(id)
        .and_then(|d| d.type_name())
        .unwrap_or("object")
        .to_owned();
    if type_name == "GameObject" {
        path.replace('/', " › ")
    } else {
        format!("{} › {type_name}", path.replace('/', " › "))
    }
}

/// Compares two versions of a prefab. `None` when no object lost its id.
pub fn compare(path: &str, guid: &str, old: &File, new: &File) -> Option<Rebuild> {
    let old_places = places(old);
    let new_places = places(new);
    let new_ids: BTreeSet<FileId> = new.docs.iter().filter_map(|d| d.file_id).collect();
    let removed: Vec<FileId> = old_places
        .keys()
        .copied()
        .filter(|id| !new_ids.contains(id))
        .collect();
    if removed.is_empty() {
        return None;
    }
    let old_ids: BTreeSet<FileId> = old_places.keys().copied().collect();
    let mut by_place: BTreeMap<&str, FileId> = BTreeMap::new();
    for (id, place) in &new_places {
        // Only objects that are new: a kept object is not a replacement.
        if !old_ids.contains(id) {
            by_place.insert(place.as_str(), *id);
        }
    }
    let mut rebuild = Rebuild {
        path: path.to_owned(),
        guid: guid.to_owned(),
        map: BTreeMap::new(),
        lost: BTreeSet::new(),
        names: BTreeMap::new(),
    };
    for id in removed {
        let place = &old_places[&id];
        rebuild.names.insert(id, describe(place, old, id));
        match by_place.get(place.as_str()) {
            Some(new_id) => {
                rebuild.map.insert(id, *new_id);
            }
            None => {
                rebuild.lost.insert(id);
            }
        }
    }
    Some(rebuild)
}

/// What [`repoint`] did to one file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Repointed {
    pub text: String,
    /// References now pointing at the new ids.
    pub moved: usize,
    /// References to old ids that have no new counterpart.
    pub dangling: usize,
}

/// Points every `{fileID: OLD, guid: GUID, …}` in `text` at the new id.
/// Everything else stays byte for byte.
pub fn repoint(text: &str, rebuild: &Rebuild) -> Repointed {
    let mut out = Repointed {
        text: String::with_capacity(text.len()),
        ..Repointed::default()
    };
    let needle = format!("guid: {}", rebuild.guid);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else {
            break;
        };
        let inner = &rest[open + 1..close];
        out.text.push_str(&rest[..open + 1]);
        let mut replaced = None;
        if inner.contains(&needle) && !inner.contains('{') {
            if let Some(id) = inner
                .split(',')
                .map(str::trim)
                .find_map(|part| part.strip_prefix("fileID:"))
                .and_then(|v| v.trim().parse::<FileId>().ok())
            {
                if let Some(new_id) = rebuild.map.get(&id) {
                    replaced = Some(inner.replacen(
                        &format!("fileID: {id}"),
                        &format!("fileID: {new_id}"),
                        1,
                    ));
                    out.moved += 1;
                } else if rebuild.lost.contains(&id) {
                    out.dangling += 1;
                }
            }
        }
        out.text.push_str(replaced.as_deref().unwrap_or(inner));
        out.text.push('}');
        rest = &rest[close + 1..];
    }
    out.text.push_str(rest);
    out
}

/// What [`repair`] found and did.
#[derive(Debug, Default)]
pub struct Report {
    pub rebuilds: Vec<Rebuild>,
    /// `(file, references moved, references left dangling)`.
    pub files: Vec<(String, usize, usize)>,
}

impl Report {
    /// Files [`repair`] would change (or changed).
    pub fn touched(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|(_, moved, _)| *moved > 0)
            .map(|(f, _, _)| f.as_str())
            .collect()
    }
}

/// The GUID in a `.meta` file.
fn meta_guid(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("guid:"))
        .map(|g| g.trim().to_owned())
        .filter(|g| !g.is_empty())
}

/// Prefabs that changed between `base` and `head` (the working tree when
/// `None`) and gave their objects new ids, with the files in the working
/// tree that still point at the old ids. With `write`, those files are
/// rewritten to point at the new ids.
pub fn repair(repo: &Repo, base: &str, head: Option<&str>, write: bool) -> Result<Report> {
    let mut changed = match head {
        Some(head) => {
            split_nul(&repo.run(["diff", "--name-only", "-z", base, head, "--", "*.prefab"])?)
        }
        None => split_nul(&repo.run(["diff", "--name-only", "-z", base, "--", "*.prefab"])?),
    };
    changed.retain(|p| p.ends_with(".prefab"));
    let mut report = Report::default();
    for path in changed {
        let Some(old) = semantic::blob(repo, &format!("{base}:{path}"))?
            .and_then(|t| unity_yaml::parse(&t).ok())
        else {
            continue;
        };
        let new_text = match head {
            Some(head) => semantic::blob(repo, &format!("{head}:{path}"))?,
            None => semantic::worktree(repo, &path),
        };
        let Some(new) = new_text.and_then(|t| unity_yaml::parse(&t).ok()) else {
            continue;
        };
        let Some(guid) = semantic::worktree(repo, &format!("{path}.meta"))
            .or_else(|| {
                semantic::blob(repo, &format!("{base}:{path}.meta"))
                    .ok()
                    .flatten()
            })
            .and_then(|t| meta_guid(&t))
        else {
            continue;
        };
        let Some(rebuild) = compare(&path, &guid, &old, &new) else {
            continue;
        };
        let users = repo
            .try_run([
                "grep",
                "-l",
                "-z",
                "-F",
                "--untracked",
                "-e",
                &format!("guid: {guid}"),
                "--",
                "*.unity",
                "*.prefab",
                "*.asset",
            ])?
            .map(|out| split_nul(&out))
            .unwrap_or_default();
        let mut any = false;
        for user in users {
            if user == path {
                continue;
            }
            let Some(text) = semantic::worktree(repo, &user) else {
                continue;
            };
            let result = repoint(&text, &rebuild);
            if result.moved == 0 && result.dangling == 0 {
                continue;
            }
            any = true;
            if write && result.moved > 0 {
                std::fs::write(repo.root().join(&user), &result.text)?;
            }
            report.files.push((user, result.moved, result.dangling));
        }
        if any {
            report.rebuilds.push(rebuild);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefab(ids: [i64; 3]) -> File {
        let [go, transform, script] = ids;
        unity_yaml::parse(&format!(
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &{go}\nGameObject:\n  m_Component:\n  - component: {{fileID: {transform}}}\n  - component: {{fileID: {script}}}\n  m_Name: Imp\n--- !u!4 &{transform}\nTransform:\n  m_GameObject: {{fileID: {go}}}\n  m_Children: []\n  m_Father: {{fileID: 0}}\n--- !u!114 &{script}\nMonoBehaviour:\n  m_GameObject: {{fileID: {go}}}\n  speed: 3\n"
        ))
        .unwrap()
    }

    #[test]
    fn a_rebuilt_prefab_maps_by_place() {
        let rebuild = compare(
            "Imp.prefab",
            "abc",
            &prefab([1, 2, 3]),
            &prefab([10, 20, 30]),
        )
        .unwrap();
        assert_eq!(rebuild.map, BTreeMap::from([(1, 10), (2, 20), (3, 30)]));
        assert!(rebuild.lost.is_empty());
        assert_eq!(rebuild.names[&2], "Imp › Transform");
        assert!(compare("Imp.prefab", "abc", &prefab([1, 2, 3]), &prefab([1, 2, 3])).is_none());
    }

    #[test]
    fn references_follow_and_nothing_else_moves() {
        let rebuild = compare(
            "Imp.prefab",
            "abc",
            &prefab([1, 2, 3]),
            &prefab([10, 20, 30]),
        )
        .unwrap();
        let scene = "  - target: {fileID: 2, guid: abc, type: 3}\n    propertyPath: m_LocalPosition.x\n    value: 5\n  - target: {fileID: 2, guid: other, type: 3}\n  m_CorrespondingSourceObject: {fileID: 1, guid: abc, type: 3}\n  m_Father: {fileID: 2}\n";
        let result = repoint(scene, &rebuild);
        assert_eq!(result.moved, 2);
        assert_eq!(
            result.text,
            "  - target: {fileID: 20, guid: abc, type: 3}\n    propertyPath: m_LocalPosition.x\n    value: 5\n  - target: {fileID: 2, guid: other, type: 3}\n  m_CorrespondingSourceObject: {fileID: 10, guid: abc, type: 3}\n  m_Father: {fileID: 2}\n"
        );
    }
}
