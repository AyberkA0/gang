//! Assets that are gone while scenes, prefabs or materials still use them
//! (HARDENING.md R29). git merges "alice deleted Red.mat" and "bob put
//! Red.mat on a new object" without a conflict, since the two changes are in
//! different files; Unity then shows the reference as Missing. GANG finds
//! such pairs when a commit deletes an asset and when a merge brings the two
//! sides together, and can bring the asset back from before the deletion.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::attributes::UNITY_YAML;
use crate::guid_moves::guid_of;
use crate::repo::{Repo, split_nul};
use crate::semantic;

/// An asset gone from the project that files still reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    /// Where it was, before it was deleted.
    pub asset: String,
    pub guid: String,
    /// A commit that still has it: where to bring it back from.
    pub from: String,
    /// The commit that deleted it (short hash, author), when known.
    pub deleted_in: Option<(String, String)>,
    /// Our side deleted it (the other side uses it).
    pub ours: bool,
    /// Files that reference it.
    pub users: Vec<String>,
}

impl Orphan {
    pub fn name(&self) -> &str {
        self.asset.rsplit('/').next().unwrap_or(&self.asset)
    }
}

fn pathspecs() -> Vec<String> {
    UNITY_YAML.iter().map(|e| format!("*.{e}")).collect()
}

/// `.meta` files deleted between `from` and `to` (`None`: the index), as
/// GUID → asset path.
fn deleted(repo: &Repo, from: &str, to: Option<&str>) -> Result<BTreeMap<String, String>> {
    let mut args = vec![
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        "--diff-filter=D",
    ];
    match to {
        Some(to) => args.extend([from, to]),
        None => args.extend(["--cached", from]),
    }
    args.extend(["--", "*.meta"]);
    let mut out = BTreeMap::new();
    for meta in split_nul(&repo.run(args)?) {
        if let Some(guid) =
            semantic::blob(repo, &format!("{from}:{meta}"))?.and_then(|t| guid_of(&t))
        {
            let asset = meta.strip_suffix(".meta").unwrap_or(&meta).to_owned();
            out.insert(guid, asset);
        }
    }
    Ok(out)
}

/// For each GUID, the files at `rev` (`None`: the index) whose text has
/// `guid: <GUID>`, within `pathspecs`.
fn grep(
    repo: &Repo,
    rev: Option<&str>,
    guids: &[&String],
    pathspecs: &[String],
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if guids.is_empty() {
        return Ok(found);
    }
    let patterns: Vec<String> = guids.iter().map(|g| format!("guid: {g}")).collect();
    let mut args: Vec<&str> = vec!["grep", "-I", "-z", "-o", "-F"];
    if rev.is_none() {
        args.push("--cached");
    }
    for pattern in &patterns {
        args.extend(["-e", pattern]);
    }
    if let Some(rev) = rev {
        args.push(rev);
    }
    args.push("--");
    args.extend(pathspecs.iter().map(String::as_str));
    let output = repo.try_run(args)?.unwrap_or_default();
    // `[<rev>:]path NUL match NEWLINE`
    for line in output.lines() {
        let Some((path, text)) = line.split_once('\0') else {
            continue;
        };
        let path = match rev {
            Some(rev) => path
                .strip_prefix(rev)
                .and_then(|p| p.strip_prefix(':'))
                .unwrap_or(path),
            None => path,
        };
        if let Some(guid) = text.strip_prefix("guid: ") {
            found
                .entry(guid.trim().to_owned())
                .or_default()
                .insert(path.to_owned());
        }
    }
    Ok(found)
}

/// GUIDs among `guids` that some `.meta` at `rev` (`None`: the index) still
/// has: assets moved or added back, not gone.
fn still_there(repo: &Repo, rev: Option<&str>, guids: &[&String]) -> Result<BTreeSet<String>> {
    Ok(grep(repo, rev, guids, &["*.meta".to_owned()])?
        .into_keys()
        .collect())
}

fn deletion(repo: &Repo, range: &str, asset: &str) -> Option<(String, String)> {
    let out = repo
        .try_run([
            "log",
            "-1",
            "--format=%h%x1f%an",
            "--diff-filter=D",
            // A small .meta deleted next to a new one looks like a rename.
            "--no-renames",
            range,
            "--",
            &format!("{asset}.meta"),
        ])
        .ok()
        .flatten()?;
    let (hash, author) = out.trim().split_once('\u{1f}')?;
    Some((hash.to_owned(), author.to_owned()))
}

/// The commit being made deletes assets that staged files still use.
pub fn staged(repo: &Repo) -> Result<Vec<Orphan>> {
    if repo
        .try_run(["rev-parse", "-q", "--verify", "HEAD"])?
        .is_none()
    {
        return Ok(Vec::new());
    }
    let gone = deleted(repo, "HEAD", None)?;
    if gone.is_empty() {
        return Ok(Vec::new());
    }
    let guids: Vec<&String> = gone.keys().collect();
    let kept = still_there(repo, None, &guids)?;
    let guids: Vec<&String> = guids.into_iter().filter(|g| !kept.contains(*g)).collect();
    let users = grep(repo, None, &guids, &pathspecs())?;
    Ok(guids
        .into_iter()
        .filter_map(|guid| {
            let users = users.get(guid)?;
            Some(Orphan {
                asset: gone[guid].clone(),
                guid: guid.clone(),
                from: "HEAD".to_owned(),
                deleted_in: None,
                ours: true,
                users: users.iter().cloned().collect(),
            })
        })
        .collect())
}

/// After a merge or rebase that brought the commits `before`..HEAD into our
/// work: assets one side deleted that files the other side changed still
/// use. A fast-forward has no work of ours, so nothing to find.
pub fn after_merge(repo: &Repo, before: &str) -> Result<Vec<Orphan>> {
    let rev = |spec: &str| -> Result<Option<String>> {
        Ok(repo
            .try_run(["rev-parse", "-q", "--verify", &format!("{spec}^{{commit}}")])?
            .map(|s| s.trim().to_owned()))
    };
    let (Some(before), Some(head)) = (rev(before)?, rev("HEAD")?) else {
        return Ok(Vec::new());
    };
    let base = match (rev("HEAD^1")?, rev("HEAD^2")?) {
        (Some(first), Some(second)) if first == before => {
            repo.try_run(["merge-base", &first, &second])?
        }
        _ => repo.try_run(["merge-base", &before, &head])?,
    };
    let Some(base) = base.map(|b| b.trim().to_owned()) else {
        return Ok(Vec::new());
    };
    if base == before || before == head {
        return Ok(Vec::new());
    }
    let ours_gone = deleted(repo, &base, Some(&before))?;
    let gone = deleted(repo, &base, Some(&head))?;
    if gone.is_empty() {
        return Ok(Vec::new());
    }
    let guids: Vec<&String> = gone.keys().collect();
    let kept = still_there(repo, Some(&head), &guids)?;
    let guids: Vec<&String> = guids.into_iter().filter(|g| !kept.contains(*g)).collect();
    let users = grep(repo, Some(&head), &guids, &pathspecs())?;
    let changed = |from: &str, to: &str| -> Result<BTreeSet<String>> {
        Ok(
            split_nul(&repo.run(["diff", "--name-only", "-z", "--no-renames", from, to])?)
                .into_iter()
                .collect(),
        )
    };
    let ours_changed = changed(&base, &before)?;
    let theirs_changed = changed(&before, &head)?;
    let mut found = Vec::new();
    for guid in guids {
        let Some(all) = users.get(guid) else { continue };
        let ours = ours_gone.contains_key(guid);
        let other_side = if ours { &theirs_changed } else { &ours_changed };
        let users: Vec<String> = all
            .iter()
            .filter(|u| other_side.contains(*u))
            .cloned()
            .collect();
        if users.is_empty() {
            continue;
        }
        let asset = gone[guid].clone();
        let range = format!("{base}..{}", if ours { &before } else { &head });
        found.push(Orphan {
            deleted_in: deletion(repo, &range, &asset),
            asset,
            guid: guid.clone(),
            from: base.clone(),
            ours,
            users,
        });
    }
    Ok(found)
}

/// Brings `orphan` back into the working tree and the index from the commit
/// that still has it: the asset, its `.meta`, the `.meta` of folders that are
/// gone too, and assets it uses that are gone the same way (a material's
/// texture). Returns the paths brought back.
pub fn bring_back(repo: &Repo, orphan: &Orphan) -> Result<Vec<String>> {
    let mut todo = vec![orphan.asset.clone()];
    let mut done: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(asset) = todo.pop() {
        if !seen.insert(asset.clone()) {
            continue;
        }
        let meta = format!("{asset}.meta");
        let mut paths = vec![meta.clone()];
        let is_file = repo
            .try_run(["cat-file", "-t", &format!("{}:{asset}", orphan.from)])?
            .is_some_and(|t| t.trim() == "blob");
        if is_file {
            paths.push(asset.clone());
        }
        // Folders that went with it.
        let mut folder = asset.as_str();
        while let Some((parent, _)) = folder.rsplit_once('/') {
            folder = parent;
            let folder_meta = format!("{parent}.meta");
            if repo.root().join(&folder_meta).exists()
                || semantic::blob(repo, &format!("{}:{folder_meta}", orphan.from))?.is_none()
            {
                continue;
            }
            paths.push(folder_meta);
        }
        let mut args = vec!["checkout".to_owned(), orphan.from.clone(), "--".to_owned()];
        args.extend(paths.iter().cloned());
        repo.run(&args)?;
        if !is_file {
            std::fs::create_dir_all(repo.root().join(&asset))?;
        }
        // What the brought-back asset uses that is gone as well.
        if is_file && crate::attributes::is_unity_yaml(&asset) && seen.len() < 100 {
            if let Some(text) = semantic::worktree(repo, &asset) {
                let used: BTreeSet<String> = text
                    .match_indices("guid: ")
                    .filter_map(|(i, _)| text.get(i + 6..i + 38))
                    .filter(|g| g.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_owned)
                    .collect();
                let used: Vec<&String> = used.iter().collect();
                let present = still_there(repo, None, &used)?;
                let missing: Vec<&String> =
                    used.into_iter().filter(|g| !present.contains(*g)).collect();
                for (_, dependency) in at(repo, &orphan.from, &missing)? {
                    todo.push(dependency);
                }
            }
        }
        done.extend(paths);
    }
    done.sort();
    done.dedup();
    Ok(done)
}

/// The assets whose `.meta` at `rev` has one of `guids`: GUID → asset.
fn at(repo: &Repo, rev: &str, guids: &[&String]) -> Result<BTreeMap<String, String>> {
    Ok(grep(repo, Some(rev), guids, &["*.meta".to_owned()])?
        .into_iter()
        .filter_map(|(guid, metas)| {
            let meta = metas.into_iter().next()?;
            Some((guid, meta.strip_suffix(".meta").unwrap_or(&meta).to_owned()))
        })
        .collect())
}

/// The commit that deleted the asset with `guid`, searched in history:
/// `(asset, short hash, author)`.
pub fn deleted_asset(repo: &Repo, guid: &str) -> Option<(String, String, String)> {
    let out = repo
        .try_run([
            "log",
            "-1",
            "--diff-filter=D",
            "--no-renames",
            "--format=%h%x1f%an",
            "--name-only",
            "-S",
            &format!("guid: {guid}"),
            "--",
            "*.meta",
        ])
        .ok()
        .flatten()?;
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let (hash, author) = lines.next()?.split_once('\u{1f}')?;
    let meta = lines.next()?.trim();
    Some((
        meta.strip_suffix(".meta").unwrap_or(meta).to_owned(),
        hash.to_owned(),
        author.to_owned(),
    ))
}
