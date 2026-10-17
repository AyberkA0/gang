//! Whether a change touches someone else's lock (DESIGN.md §10.6–§10.7).

use std::collections::BTreeSet;

use anyhow::Result;
use gang_merge::diff::{self, DocChange, Rules};
use unity_yaml::{FileId, Hierarchy};

use crate::config::{Config, parse_duration};
use crate::locks::{self, Lock, LockKind, Target};
use crate::repo::Repo;
use crate::{clock, presence};

/// A change that hits a lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub path: String,
    /// What was changed: the file, or `Player › Transform › Position`.
    pub what: String,
    /// What is locked: `Assets/Art/Hero.psd`, `Level1.unity › Player`.
    pub locked: String,
    pub owner: String,
    pub reason: Option<String>,
    pub since: String,
    /// For object locks: `<file>::<object path>` to restore just that object.
    pub restore: Option<String>,
}

impl Blocked {
    pub fn message(&self) -> String {
        let reason = self
            .reason
            .as_deref()
            .map(|r| format!(" ({r:?})"))
            .unwrap_or_default();
        let since = clock::parse(&self.since)
            .map(|t| format!(", {}", clock::ago(clock::now() - t)))
            .unwrap_or_default();
        if self.what == self.path {
            format!("{} is locked by {}{reason}{since}", self.locked, self.owner)
        } else {
            format!(
                "{} is locked by {}{reason}{since} — you changed {}",
                self.locked, self.owner, self.what
            )
        }
    }
}

/// Other people's locks that are in force.
pub struct Checker {
    me: String,
    locks: Vec<Lock>,
}

impl Checker {
    /// Loads the lock table (fetching it first when `fetch` is set) and the
    /// team's presence for expiry.
    pub fn load(repo: &Repo, fetch: bool) -> Result<Checker> {
        if fetch {
            locks::fetch(repo)?;
            let _ = presence::fetch(repo);
        }
        let me = presence::identity(repo)?.login;
        let config = Config::load(repo.root()).unwrap_or_default();
        let ttl = parse_duration(&config.locks.auto_ttl)
            .unwrap_or(std::time::Duration::from_secs(8 * 3600));
        let team = presence::team(repo).unwrap_or_default();
        let locks = locks::read(repo)?
            .locks
            .into_iter()
            .filter(|l| l.owner != me && !locks::expired(l, &team, ttl))
            .collect();
        Ok(Checker { me, locks })
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }

    /// Checks one changed file, given its old and new text (for Unity files).
    pub fn check(&self, path: &str, old: Option<&str>, new: Option<&str>) -> Vec<Blocked> {
        let mut blocked = Vec::new();
        for lock in &self.locks {
            if let Some(target) = lock.targets().find(|t| t.covers_file(path)) {
                blocked.push(self.blocked(lock, path, path.to_owned(), target, None));
                continue;
            }
            let object_targets: Vec<&Target> = lock
                .targets()
                .filter(|t| t.unity_file() == Some(path))
                .collect();
            if object_targets.is_empty() {
                continue;
            }
            if let Some((what, target)) = touched_object(path, old, new, &object_targets) {
                let restore = match target {
                    Target::Object { path: object, .. } => Some(format!("{path}::{object}")),
                    _ => None,
                };
                blocked.push(self.blocked(lock, path, what, target, restore));
            }
        }
        blocked
    }

    fn blocked(
        &self,
        lock: &Lock,
        path: &str,
        what: String,
        target: &Target,
        restore: Option<String>,
    ) -> Blocked {
        Blocked {
            path: path.to_owned(),
            what,
            locked: if lock.kind == LockKind::Bake {
                format!("{} (bake)", target.label())
            } else {
                target.label()
            },
            owner: lock.owner.clone(),
            reason: lock.reason.clone(),
            since: lock.since.clone(),
            restore,
        }
    }
}

/// The first change in `path` that falls inside a locked object or document.
fn touched_object<'a>(
    path: &str,
    old: Option<&str>,
    new: Option<&str>,
    targets: &[&'a Target],
) -> Option<(String, &'a Target)> {
    let parse = |t: Option<&str>| t.and_then(|t| unity_yaml::parse(t).ok());
    let empty = unity_yaml::File {
        prefix: String::new(),
        docs: Vec::new(),
    };
    let old = parse(old).unwrap_or_else(|| empty.clone());
    let new = parse(new).unwrap_or(empty);
    let changes = diff::diff(&old, &new, &Rules::default());
    let changes: Vec<&DocChange> = changes.iter().filter(|c| !c.is_noise()).collect();
    if changes.is_empty() {
        return None;
    }
    let (old_h, new_h) = (Hierarchy::build(&old), Hierarchy::build(&new));
    for target in targets {
        let locked: BTreeSet<FileId> = match target {
            Target::Object { file_id, .. } => {
                let mut docs = new_h.subtree_docs(*file_id);
                docs.extend(old_h.subtree_docs(*file_id));
                docs
            }
            Target::Document { file_id, .. } => BTreeSet::from([*file_id]),
            _ => continue,
        };
        // Moving a child into or out of a locked object changes its children.
        for change in &changes {
            if !locked.contains(&change.id()) {
                continue;
            }
            let owner = new_h
                .owner(change.id())
                .or_else(|| old_h.owner(change.id()));
            let object = owner
                .and_then(|o| new_h.path(o).or_else(|| old_h.path(o)))
                .map(|p| p.replace('/', " › "))
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_owned());
            let what = match change {
                DocChange::Changed { props, id } => {
                    let prop = props
                        .iter()
                        .find(|p| !p.noise)
                        .map(|p| diff::label(&p.path));
                    let component = new
                        .doc(*id)
                        .or_else(|| old.doc(*id))
                        .and_then(|d| d.type_name())
                        .unwrap_or("")
                        .to_owned();
                    let is_object = new_h.object(*id).is_some() || old_h.object(*id).is_some();
                    match (prop, is_object) {
                        (Some(prop), true) => format!("{object} › {prop}"),
                        (Some(prop), false) => format!("{object} › {component} › {prop}"),
                        (None, _) => object,
                    }
                }
                DocChange::Added(_) => format!("something under {object}"),
                DocChange::Removed(_) => format!("something under {object} (removed)"),
            };
            return Some((what, *target));
        }
    }
    None
}

/// Bake outputs (lightmaps, LightingData, reflection probes, NavMesh data)
/// live in a folder named after their scene. Returns the scene for one.
pub fn bake_output_scene(index: &BTreeSet<String>, path: &str) -> Option<String> {
    let (folder, name) = path.rsplit_once('/')?;
    let name = name.strip_suffix(".meta").unwrap_or(name);
    let is_output = name == "LightingData.asset"
        || name.starts_with("Lightmap-")
        || name.starts_with("ReflectionProbe-")
        || (name.starts_with("NavMesh") && name.ends_with(".asset"));
    if !is_output {
        return None;
    }
    let scene = format!("{folder}.unity");
    index.contains(&scene).then_some(scene)
}

/// The folder a scene's bake outputs go to.
pub fn bake_folder(scene: &str) -> String {
    format!("{}/", scene.strip_suffix(".unity").unwrap_or(scene))
}

/// Whether `me` holds the bake lock for `scene`.
pub fn holds_bake_lock(all: &[Lock], me: &str, scene: &str) -> bool {
    let folder = bake_folder(scene);
    all.iter().any(|l| {
        l.owner == me
            && l.kind == LockKind::Bake
            && l.root
                == Target::Folder {
                    path: folder.clone(),
                }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bake_outputs() {
        let index = BTreeSet::from(["Assets/Scenes/Level1.unity".to_owned()]);
        assert_eq!(
            bake_output_scene(&index, "Assets/Scenes/Level1/LightingData.asset").as_deref(),
            Some("Assets/Scenes/Level1.unity")
        );
        assert_eq!(
            bake_output_scene(&index, "Assets/Scenes/Level1/Lightmap-0_comp_light.exr").as_deref(),
            Some("Assets/Scenes/Level1.unity")
        );
        assert_eq!(
            bake_output_scene(&index, "Assets/Scenes/Level1/Notes.txt"),
            None
        );
        assert_eq!(
            bake_output_scene(&index, "Assets/Scenes/Level2/LightingData.asset"),
            None
        );
    }
}
