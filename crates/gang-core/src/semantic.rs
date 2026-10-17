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
