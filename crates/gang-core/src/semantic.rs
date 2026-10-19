//! Reading versions of Unity files and comparing them semantically
//! (DESIGN.md §12).

use std::fs;

use anyhow::Result;
use gang_merge::diff::{self, Rules};
use unity_yaml::GuidIndex;

use crate::config::Config;
use crate::repo::Repo;
use crate::{attributes, unity};

/// Every project's `.meta` GUIDs, paths relative to the repository root.
pub fn guid_index(repo: &Repo) -> GuidIndex {
    let mut index = GuidIndex::default();
    for project in unity::find_projects(repo.root()) {
        index.extend(GuidIndex::scan(&project.path(repo.root()), repo.root()));
    }
    index
}

/// [`guid_index`] plus the packages Unity downloaded into
/// `Library/PackageCache`, for naming things: UI and networking components
/// live in packages, and without them a diff says "MonoBehaviour" where
/// the Inspector says "Canvas Scaler". The package part is cached in
/// `.git/gang/cache/` until the package folder changes.
pub fn name_index(repo: &Repo) -> GuidIndex {
    let mut index = guid_index(repo);
    for project in unity::find_projects(repo.root()) {
        let cache = project
            .path(repo.root())
            .join("Library")
            .join("PackageCache");
        let Ok(stamp) = fs::metadata(&cache).and_then(|m| m.modified()) else {
            continue;
        };
        let stamp = stamp
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let file = repo
            .gang_dir()
            .join("cache")
            .join(format!("packages-{}.json", project.dir.replace('/', "_")));
        let cached: Option<(u64, Vec<(String, String)>)> = fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        let entries = match cached {
            Some((at, entries)) if at == stamp => entries,
            _ => {
                let scanned = GuidIndex::scan_folder(&cache, repo.root());
                let entries = scanned.entries();
                if let Some(dir) = file.parent() {
                    let _ = fs::create_dir_all(dir);
                }
                let _ = fs::write(
                    &file,
                    serde_json::to_string(&(stamp, &entries)).unwrap_or_default(),
                );
                entries
            }
        };
        index.extend(GuidIndex::from_entries(entries));
    }
    index
}

/// Comparison rules from the project's configuration.
pub fn rules(config: &Config) -> Rules {
    Rules {
        float_epsilon: config.merge.float_epsilon,
        extra_noise: config
            .merge
            .noise
            .iter()
            .filter(|rule| rule.rule == "ignore")
            .map(|rule| (rule.class.clone(), rule.path.clone()))
            .collect(),
    }
}

/// A version of a file: `HEAD:path`, `:path` (the index), `<rev>:path`.
pub fn blob(repo: &Repo, spec: &str) -> Result<Option<String>> {
    repo.try_run(["show", spec])
}

/// The file in the working tree.
pub fn worktree(repo: &Repo, path: &str) -> Option<String> {
    fs::read_to_string(repo.root().join(path)).ok()
}

/// Whether the working-tree change to a Unity file, compared with `HEAD`, is
/// only noise.
pub fn is_noise_change(repo: &Repo, path: &str, rules: &Rules) -> Result<bool> {
    if !attributes::is_unity_yaml(path) {
        return Ok(false);
    }
    let (Some(old), Some(new)) = (blob(repo, &format!("HEAD:{path}"))?, worktree(repo, path))
    else {
        return Ok(false);
    };
    Ok(diff::noise_only(&old, &new, rules))
}
