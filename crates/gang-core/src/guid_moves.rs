//! Assets whose GUID changed in a merge (DESIGN.md §14.5): when both sides
//! added the same asset, the merge keeps the GUID already shared, and the
//! references our side made to its own GUID are pointed at the shared one.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::repo::{Repo, split_nul};
use crate::semantic;

pub(crate) fn guid_of(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("guid:"))
        .map(|g| g.trim().to_owned())
        .filter(|g| !g.is_empty())
}

/// `(asset, old GUID, new GUID, files rewritten)`.
pub type Moved = Vec<(String, String, String, Vec<String>)>;

/// `.meta` files whose GUID differs between `before` and `HEAD`, where the
/// old GUID is gone from the project: references to it in the working tree
/// are rewritten to the new GUID.
pub fn repair(repo: &Repo, before: &str) -> Result<Moved> {
    let metas: Vec<String> = split_nul(&repo.run([
        "diff",
        "--name-only",
        "-z",
        "--diff-filter=M",
        before,
        "HEAD",
        "--",
        "*.meta",
    ])?);
    let mut moves: BTreeMap<String, (String, String)> = BTreeMap::new();
    for meta in metas {
        let old = semantic::blob(repo, &format!("{before}:{meta}"))?.and_then(|t| guid_of(&t));
        let new = semantic::blob(repo, &format!("HEAD:{meta}"))?.and_then(|t| guid_of(&t));
        if let (Some(old), Some(new)) = (old, new) {
            if old != new {
                moves.insert(old, (meta, new));
            }
        }
    }
    if moves.is_empty() {
        return Ok(Vec::new());
    }
    let index = semantic::guid_index(repo);
    let mut done = Vec::new();
    for (old, (meta, new)) in moves {
        // Still used by another asset: those references are not ours to move.
        if index.path(&old).is_some() {
            continue;
        }
        let users = repo
            .try_run([
                "grep",
                "-l",
                "-z",
                "-F",
                "--untracked",
                "-e",
                &format!("guid: {old}"),
                "--",
                "*.unity",
                "*.prefab",
                "*.asset",
                "*.mat",
                "*.controller",
                "*.anim",
                "*.overrideController",
                "*.playable",
                "*.spriteatlas",
                "*.asmdef",
                "*.asmref",
            ])?
            .map(|out| split_nul(&out))
            .unwrap_or_default();
        let mut files = Vec::new();
        for user in users {
            let Some(text) = semantic::worktree(repo, &user) else {
                continue;
            };
            let rewritten = text.replace(&format!("guid: {old}"), &format!("guid: {new}"));
            if rewritten != text {
                std::fs::write(repo.root().join(&user), rewritten)?;
                files.push(user);
            }
        }
        let asset = meta.strip_suffix(".meta").unwrap_or(&meta).to_owned();
        done.push((asset, old, new, files));
    }
    Ok(done)
}
