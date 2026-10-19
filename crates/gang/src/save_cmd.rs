//! `gang save`, `gang undo` and `gang hook`.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use clap::Parser;
use gang_core::guards::{self, Allow, Severity};
use gang_core::invocation::Invocation;
use gang_core::repo::Repo;
use gang_core::{git, safety, semantic, setup};

use crate::{ui, wrap};

/// Add every change and commit it, after GANG's checks
#[derive(Parser)]
#[command(name = "gang save")]
pub struct SaveArgs {
    /// Commit message; several -m become paragraphs, as in git
    #[arg(short, long)]
    message: Vec<String>,
    /// Push after committing
    #[arg(long)]
    push: bool,
    /// Only save these paths (default: everything)
    paths: Vec<String>,
}

pub fn save(invocation: &Invocation) -> Result<i32> {
    let args = SaveArgs::parse_from(
        std::iter::once(OsString::from("gang save")).chain(invocation.args.iter().cloned()),
    );
    git::ensure_supported()?;
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    setup::ensure_installed(&repo)?;

    // Staging everything would mark unanswered conflicts as resolved.
    let unmerged = crate::bang::unmerged(&repo)?;
    if !unmerged.is_empty() {
        eprintln!(
            "✗ A merge is in progress and {} file{} still {} conflicts:",
            unmerged.len(),
            if unmerged.len() == 1 { "" } else { "s" },
            if unmerged.len() == 1 { "has" } else { "have" }
        );
        for path in unmerged.iter().take(10) {
            eprintln!("    {path}");
        }
        ui::hint("Answer them first: gang bang");
        return Ok(1);
    }
    // New layers: reserve them, and move the ones a teammate took first
    // before anything is staged (DESIGN.md §14.6).
    if let Some(code) = crate::layers_cmd::before_save(&repo)? {
        return Ok(code);
    }
    // A rebuilt prefab: point the scenes' references at its new objects.
    if let Some(code) = crate::prefabs_cmd::before_save(&repo)? {
        return Ok(code);
    }
    // Files the project wants locked while they change: locked now, when
    // nobody holds them ([scenes.policy], R35).
    crate::locks_cmd::before_save(&repo);

    // A refused save must leave the index exactly as it was.
    let index_before = repo.try_run(["write-tree"])?.map(|t| t.trim().to_owned());
    let restore_index = |repo: &Repo| {
        if let Some(tree) = &index_before {
            let _ = repo.run(["read-tree", tree]);
        }
    };

    let add_args: Vec<OsString> = if args.paths.is_empty() {
        vec!["-A".into()]
    } else {
        std::iter::once(OsString::from("--"))
            .chain(args.paths.iter().map(OsString::from))
            .collect()
    };
    let code = wrap::add(invocation, &repo, &add_args)?;
    if code != 0 {
        restore_index(&repo);
        return Ok(code);
    }
    if guards::staged_changes(&repo)?.is_empty() {
        println!("Nothing to save.");
        return Ok(0);
    }

    let message = match args.message.as_slice() {
        [] if ui::interactive() => {
            print!("Message: ");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            let line = line.trim().to_owned();
            if line.is_empty() {
                restore_index(&repo);
                bail!("a message is needed to save");
            }
            line
        }
        [] => {
            restore_index(&repo);
            bail!("a message is needed: gang save -m \"…\"")
        }
        paragraphs => paragraphs.join(
            "

",
        ),
    };

    let op = safety::begin(&repo, "save", false)?;
    let dropped = drop_empty_folders(&repo)?;
    let mut commit = invocation.git_globals.clone();
    commit.extend(["commit".into(), "-q".into(), "-m".into(), message.into()]);
    let code = git::run_inherited(&commit)?;
    if code != 0 {
        restore_index(&repo);
        for folder in &dropped {
            let _ = repo.run(["checkout", "--", &format!("{folder}.meta")]);
        }
        if index_before.is_some() {
            ui::hint("Nothing was staged or committed; your changes are where they were.");
        }
        return Ok(code);
    }
    safety::finish(&repo, op)?;
    let summary = repo.run(["log", "-1", "--format=%h %s"])?;
    ui::done(format!("Saved {}", summary.trim()));

    if args.push {
        let has_upstream = repo
            .try_run(["rev-parse", "--abbrev-ref", "@{upstream}"])?
            .is_some();
        let mut push = invocation.git_globals.clone();
        push.push("push".into());
        if !has_upstream {
            push.extend(["-u".into(), "origin".into(), "HEAD".into()]);
        }
        return Ok(git::run_inherited(&push)?);
    }
    Ok(0)
}

/// The last asset of a folder deleted, the folder and its `.meta` left
/// (R31): git keeps no empty folders, so on teammates' machines Unity would
/// delete the lone `.meta`, and here it would give the folder a new one when
/// it comes back. The empty folder goes with its last asset, as when a
/// folder is deleted in Unity.
fn drop_empty_folders(repo: &Repo) -> Result<Vec<String>> {
    let mut all = Vec::new();
    for _ in 0..32 {
        let folders = guards::emptied_folders(repo)?;
        let mut dropped = false;
        for folder in folders {
            let dir = repo.root().join(&folder);
            if !is_empty_dir(&dir) {
                continue;
            }
            let _ = std::fs::remove_dir_all(&dir);
            repo.run(["rm", "-q", "--", &format!("{folder}.meta")])?;
            println!("✓ The empty folder {folder} goes too (git keeps no empty folders)");
            all.push(folder);
            dropped = true;
        }
        if !dropped {
            break;
        }
    }
    Ok(all)
}

/// A missing folder, or one with only empty folders in it.
fn is_empty_dir(dir: &std::path::Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .all(|e| e.path().is_dir() && is_empty_dir(&e.path())),
        Err(_) => true,
    }
}

/// `gang undo`: undoes the last GANG operation.
pub fn undo(invocation: &Invocation) -> Result<i32> {
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    let Some(op) = safety::last(&repo)? else {
        println!("Nothing to undo.");
        return Ok(0);
    };
    if ui::interactive() && !ui::confirm(&format!("Undo the last `{}`?", op.op), true)? {
        return Ok(1);
    }
    let message = safety::undo(&repo, &op)?;
    ui::done(format!("Undid `{}`: {message}", op.op));
    Ok(0)
}

/// `gang hook <name>`: called by the hook shims GANG installs.
pub fn hook(invocation: &Invocation) -> Result<i32> {
    let name = invocation
        .args
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.as_str() {
        "pre-commit" => pre_commit(),
        "post-commit" | "post-checkout" | "post-merge" => {
            if let Some(repo) = Repo::discover(&[])? {
                crate::presence_cmd::spawn_publish(&repo);
            }
            Ok(0)
        }
        "pre-push" => pre_push(),
        _ => Ok(0),
    }
}

fn pre_commit() -> Result<i32> {
    let Some(repo) = Repo::discover(&[])? else {
        return Ok(0);
    };
    let allow = Allow {
        unity_upgrade: std::env::var_os("GANG_ALLOW_UNITY_UPGRADE").is_some(),
    };
    let findings = guards::check_staged(&repo, &allow)?;
    let mut blocked = false;
    for finding in &findings {
        let symbol = match finding.severity {
            Severity::Block => {
                blocked = true;
                "✗"
            }
            Severity::Warn => "⚠",
        };
        eprintln!("{symbol} {}: {}", finding.path, finding.message);
        if let Some(fix) = &finding.fix {
            eprintln!("  Fix: {fix}");
        }
    }

    // Locks, reserved layer numbers and the bake policy.
    if gang_core::setup::is_gang_repo(&repo) {
        // Deleting an asset that scenes still use (R29): a warning, since
        // pointing them elsewhere may well come in the next commit.
        for orphan in gang_core::orphans::staged(&repo).unwrap_or_default() {
            let count = orphan.users.len();
            eprintln!(
                "⚠ {}: deleted, but {} still use{} it; Unity will show it as Missing there",
                orphan.asset,
                if count == 1 {
                    orphan.users[0].clone()
                } else {
                    format!(
                        "{count} files ({})",
                        orphan
                            .users
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                if count == 1 { "s" } else { "" }
            );
            eprintln!(
                "  Fix: point them at another asset in Unity, or keep it: gang restore --staged --worktree -- \"{}\" \"{}.meta\"",
                orphan.asset, orphan.asset
            );
        }
        for problem in crate::layers_cmd::pre_commit_problems(&repo) {
            blocked = true;
            eprintln!("✗ {problem}");
            eprintln!(
                "  Fix: gang save (it moves your layer, with your objects and scripts; close Unity first)"
            );
        }
        let changes = guards::staged_changes(&repo)?;
        let config = gang_core::config::Config::load(repo.root()).unwrap_or_default();
        if !config.scenes.policy.is_empty() {
            let all = gang_core::locks::read(&repo)
                .map(|t| t.locks)
                .unwrap_or_default();
            for path in gang_core::lock_check::unlocked_but_required(
                &config,
                &all,
                changes.iter().map(|c| c.path.as_str()),
            ) {
                blocked = true;
                eprintln!(
                    "✗ {path}: the project wants it locked while it changes ([scenes.policy] lock-required)"
                );
                eprintln!("  Fix: gang lock {path}  (gang save takes the lock for you)");
            }
        }
        let versions: Vec<(String, Option<String>, Option<String>)> = changes
            .iter()
            .map(|c| {
                let unity = gang_core::attributes::is_unity_yaml(&c.path);
                let old = unity
                    .then(|| {
                        semantic::blob(&repo, &format!("HEAD:{}", c.path))
                            .ok()
                            .flatten()
                    })
                    .flatten();
                let new = (unity && !c.deletes())
                    .then(|| {
                        semantic::blob(&repo, &format!(":{}", c.path))
                            .ok()
                            .flatten()
                    })
                    .flatten();
                (c.path.clone(), old, new)
            })
            .collect();
        for hit in crate::locks_cmd::check_changes(&repo, false, &versions).unwrap_or_default() {
            blocked = true;
            eprintln!("✗ {}", hit.message());
            match &hit.restore {
                Some(target) => eprintln!(
                    "  Fix: gang restore {target}  (undoes only that object), then add the file again"
                ),
                None => eprintln!(
                    "  Fix: ask {} to unlock it, or `gang unlock --force` (the team is told)",
                    hit.owner
                ),
            }
        }
        if gang_core::config::Config::load(repo.root())
            .map(|c| c.scenes.lighting == "lock")
            .unwrap_or(false)
        {
            let index: std::collections::BTreeSet<String> =
                repo.tracked_files()?.into_iter().collect();
            let me = gang_core::presence::identity(&repo)?.login;
            let all = gang_core::locks::read(&repo)
                .map(|t| t.locks)
                .unwrap_or_default();
            // Moving bake data along with its renamed scene changes nothing
            // in it (field test C4): exact renames are not bake changes.
            let renamed: std::collections::BTreeSet<String> =
                gang_core::repo::split_nul(&repo.run([
                    "diff",
                    "--cached",
                    "--name-status",
                    "-M100%",
                    "--diff-filter=R",
                    "-z",
                ])?)
                .chunks(3)
                .flat_map(|c| c.iter().skip(1).cloned().collect::<Vec<_>>())
                .collect();
            let mut scenes = std::collections::BTreeSet::new();
            for change in changes.iter().filter(|c| !renamed.contains(&c.path)) {
                if let Some(scene) = gang_core::lock_check::bake_output_scene(&index, &change.path)
                {
                    if !gang_core::lock_check::holds_bake_lock(&all, &me, &scene) {
                        scenes.insert(scene);
                    }
                }
            }
            for scene in scenes {
                blocked = true;
                eprintln!(
                    "✗ {scene}: bake outputs need a bake lock (the project's lighting policy)"
                );
                eprintln!("  Fix: gang lock {scene} --bake");
            }
        }
    }

    if blocked {
        eprintln!();
        eprintln!("Commit stopped by GANG. Fix the problems above and commit again.");
        return Ok(1);
    }
    Ok(0)
}

/// Checks everything being pushed against freshly fetched locks.
fn pre_push() -> Result<i32> {
    let Some(repo) = Repo::discover(&[])? else {
        return Ok(0);
    };
    if !gang_core::setup::is_gang_repo(&repo) {
        return Ok(0);
    }
    let mut input = String::new();
    io::Read::read_to_string(&mut io::stdin(), &mut input)?;
    let config = gang_core::config::Config::load(repo.root()).unwrap_or_default();
    let zero = |sha: &str| sha.chars().all(|c| c == '0');
    let mut versions = Vec::new();
    for line in input.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let [_, local, _, remote] = parts.as_slice() else {
            continue;
        };
        if zero(local) {
            continue;
        }
        let base = if zero(remote) || repo.try_run(["cat-file", "-e", remote])?.is_none() {
            match gang_core::presence::upstream(&repo, &config)? {
                Some(upstream) => repo
                    .try_run(["merge-base", local, &upstream])?
                    .map(|b| b.trim().to_owned()),
                None => None,
            }
        } else {
            Some((*remote).to_owned())
        };
        let Some(base) = base else { continue };
        let paths =
            gang_core::repo::split_nul(&repo.run(["diff", "--name-only", "-z", &base, local])?);
        for path in paths {
            let unity = gang_core::attributes::is_unity_yaml(&path);
            let old = unity
                .then(|| {
                    semantic::blob(&repo, &format!("{base}:{path}"))
                        .ok()
                        .flatten()
                })
                .flatten();
            let new = unity
                .then(|| {
                    semantic::blob(&repo, &format!("{local}:{path}"))
                        .ok()
                        .flatten()
                })
                .flatten();
            versions.push((path, old, new));
        }
    }
    let hits = match crate::locks_cmd::check_changes(&repo, true, &versions) {
        Ok(hits) => hits,
        Err(err) => {
            eprintln!("⚠ could not check locks: {err:#}");
            return Ok(0);
        }
    };
    if hits.is_empty() {
        return Ok(0);
    }
    for hit in &hits {
        eprintln!("✗ {}", hit.message());
    }
    eprintln!();
    eprintln!("Push stopped by GANG: it changes locked files or objects.");
    Ok(1)
}
