//! `gang sync`: save, pull, merge, answer, push (DESIGN.md §18.1).

use std::collections::BTreeSet;
use std::ffi::OsString;

use anyhow::{Context, Result};
use clap::Parser;
use gang_core::config::Config;
use gang_core::invocation::Invocation;
use gang_core::repo::{Repo, split_nul};
use gang_core::{attributes, git, presence, semantic, setup, unity};

use crate::ui;

/// Save your work, merge the team's, and push
#[derive(Parser)]
#[command(name = "gang sync")]
pub struct Args {
    /// Rebase onto the team's work instead of merging
    #[arg(long)]
    rebase: bool,
    /// Commit message for uncommitted changes (asked when missing)
    #[arg(short, long)]
    message: Option<String>,
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = Args::parse_from(
        std::iter::once(OsString::from("gang sync")).chain(invocation.args.iter().cloned()),
    );
    git::ensure_supported()?;
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    setup::ensure_installed(&repo)?;
    let config = Config::load(repo.root())?;

    if !crate::bang::unmerged(&repo)?.is_empty() {
        eprintln!("✗ A merge is still in progress. Run `gang bang` to finish it first.");
        return Ok(1);
    }
    // Every conflict answered, but the merge commit was never made: finish
    // that merge rather than starting a new save (field test A9).
    if repo.git_dir().join("MERGE_HEAD").exists() {
        let code = git::run_inherited(["commit", "--no-edit", "-q"])?;
        if code != 0 {
            return Ok(code);
        }
        ui::done("Finished the merge that was left open");
    }

    // 1. Uncommitted work. New files nobody tracks yet do not stand in the
    // way of a merge; they are only saved when a message says so.
    let untracked = if args.message.is_some() {
        "--untracked-files=normal"
    } else {
        "--untracked-files=no"
    };
    let dirty = !repo
        .run(["status", "--porcelain", untracked])?
        .trim()
        .is_empty();
    if dirty {
        let save = args.message.is_some()
            || (ui::interactive()
                && ui::confirm("You have uncommitted changes. Save them first?", true)?);
        if !save {
            eprintln!("✗ Save or stash your changes first (gang save -m \"…\").");
            return Ok(1);
        }
        let mut save_args: Vec<OsString> = Vec::new();
        if let Some(message) = &args.message {
            save_args.extend(["-m".into(), message.into()]);
        }
        let save = Invocation {
            args: save_args,
            command: Some("save".into()),
            ..invocation.clone()
        };
        let code = crate::save_cmd::save(&save)?;
        if code != 0 {
            return Ok(code);
        }
    }

    if !presence::has_origin(&repo)? {
        eprintln!("✗ This repository has no `origin` remote to sync with.");
        return Ok(1);
    }

    for attempt in 0..3 {
        // 2. Fetch.
        let code = git::run_inherited([
            "-C",
            &repo.root().to_string_lossy(),
            "fetch",
            "-q",
            "origin",
        ])?;
        if code != 0 {
            return Ok(code);
        }
        let _ = presence::fetch(&repo);
        let _ = gang_core::locks::fetch(&repo);
        let upstream = presence::upstream(&repo, &config)?;

        // 3. Merge what came in.
        if let Some(upstream) = &upstream {
            let incoming = count(&repo, &format!("HEAD..{upstream}"))?;
            if incoming > 0 {
                if let Some(code) = editor_guard(invocation, &repo, upstream, &config)? {
                    return Ok(code);
                }
                let rebase = args.rebase || config.sync.strategy == "rebase";
                if config.layers.auto_remap {
                    pre_merge(&repo, upstream, rebase)?;
                }
                let code = merge(invocation, &repo, upstream, rebase, incoming)?;
                if code != 0 {
                    return Ok(code);
                }
            }
        }

        // 4. Push what is ours.
        let outgoing = match &upstream {
            Some(upstream) => count(&repo, &format!("{upstream}..HEAD"))?,
            None => 1,
        };
        if outgoing == 0 {
            ui::done(format!(
                "Up to date with {}",
                upstream.as_deref().unwrap_or("origin")
            ));
            return Ok(0);
        }
        let has_upstream = repo
            .try_run(["rev-parse", "--abbrev-ref", "@{upstream}"])?
            .is_some();
        let pushed: Vec<String> = match &upstream {
            Some(upstream) => {
                split_nul(&repo.run(["diff", "--name-only", "-z", upstream, "HEAD"])?)
            }
            None => Vec::new(),
        };
        let mut push = vec!["push", "-q"];
        if !has_upstream {
            push.extend(["-u", "origin", "HEAD"]);
        }
        let output = repo.output(&push)?;
        if output.status.success() {
            ui::done(format!(
                "Pushed {outgoing} commit{}",
                if outgoing == 1 { "" } else { "s" }
            ));
            if let Err(err) = crate::locks_cmd::after_push(&repo, &pushed) {
                ui::warn(format!("could not update locks: {err:#}"));
            }
            return Ok(0);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let rejected = stderr.contains("rejected")
            || stderr.contains("fetch first")
            || stderr.contains("non-fast-forward");
        if !rejected || attempt == 2 {
            eprint!("{stderr}");
            push_failed(&stderr, outgoing);
            return Ok(output.status.code().unwrap_or(1));
        }
        println!("Someone pushed in the meantime; merging their work too…");
    }
    Ok(1)
}

/// What a failed push means for the user, after git's own message.
fn push_failed(stderr: &str, outgoing: usize) {
    eprintln!();
    eprintln!(
        "✗ Push failed. Your {outgoing} commit{} {} safe in this clone; nothing was lost.",
        if outgoing == 1 { "" } else { "s" },
        if outgoing == 1 { "is" } else { "are" }
    );
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("lfs")
        && ["budget", "quota", "exceeded"]
            .iter()
            .any(|w| lower.contains(w))
    {
        ui::hint(
            "GitHub refused the Git LFS upload: the LFS storage or bandwidth quota is used up.",
        );
        ui::hint("Either raise the LFS budget on GitHub, or stop using LFS for this project:");
        ui::hint("set [lfs] enabled = false in .gang/config.toml and run `gang ignore update`.");
        ui::hint("Commits that already contain LFS files still need them moved out of LFS first.");
    } else {
        ui::hint("Fix the problem above, then run `gang sync` again.");
    }
}

/// Passes that run before merging, rebasing onto or cherry-picking
/// `upstream` (DESIGN.md §14.2): the layer remap.
pub fn pre_merge(repo: &Repo, upstream: &str, rebase: bool) -> Result<()> {
    gang_core::layers::forget_rebase(repo);
    for warning in gang_core::layers::removed_in_use(repo, upstream).unwrap_or_default() {
        ui::warn(warning);
    }
    let Some(report) = gang_core::layers::run(repo, upstream)? else {
        return Ok(());
    };
    if rebase {
        gang_core::layers::remember_for_rebase(repo, upstream, &report)?;
    }
    for m in &report.moves {
        if m.follow {
            println!("Layer moved: {}.", m.taken_by);
            println!(
                "  → Moving your new uses of layer {} to layer {}:",
                m.from, m.to
            );
        } else {
            println!("Layer conflict: {}.", m.taken_by);
            println!(
                "  → Moving {:?} to layer {} and updating what your commits used it for:",
                m.name, m.to
            );
        }
    }
    for line in &report.updated {
        println!("    ✓ {line}");
    }
    for line in &report.review {
        println!("    ⚠ {line} — please review");
    }
    if report.review.is_empty() {
        println!("    Nothing needs review.");
    }
    let subject = repo.run(["log", "-1", "--format=%s"])?;
    ui::done(format!("Committed \"{}\"", subject.trim()));
    Ok(())
}

fn count(repo: &Repo, range: &str) -> Result<usize> {
    Ok(repo
        .run(["rev-list", "--count", range])?
        .trim()
        .parse()
        .unwrap_or(0))
}

/// Stops when Unity is open and the incoming work changes scenes or prefabs.
fn editor_guard(
    invocation: &Invocation,
    repo: &Repo,
    upstream: &str,
    config: &Config,
) -> Result<Option<i32>> {
    let open: Vec<String> = unity::find_projects(repo.root())
        .into_iter()
        .filter(|p| unity::is_editor_open(&p.path(repo.root())))
        .map(|p| p.dir)
        .collect();
    if open.is_empty() {
        return Ok(None);
    }
    let changed =
        split_nul(&repo.run(["diff", "--name-only", "-z", &format!("HEAD...{upstream}")])?);
    let scenes: Vec<&String> = changed
        .iter()
        .filter(|p| p.ends_with(".unity") || p.ends_with(".prefab"))
        .collect();
    if scenes.is_empty() {
        return Ok(None);
    }
    if invocation.options.ignore_editor || config.sync.editor_open == "warn" {
        ui::warn("Unity is open: let it reload the changed scenes before you save in Unity.");
        return Ok(None);
    }
    eprintln!(
        "✗ Unity is open, and this sync would change {} scene/prefab file{}:",
        scenes.len(),
        if scenes.len() == 1 { "" } else { "s" }
    );
    for scene in scenes.iter().take(10) {
        eprintln!("    {scene}");
    }
    ui::hint("Save your work and close Unity, then run `gang sync` again.");
    ui::hint("(Use `gang --ignore-editor sync` to sync anyway.)");
    Ok(Some(1))
}

/// Merges `upstream` (not a rebase) the way `gang sync` does: the merge
/// driver, `gang bang` on a terminal, the summary and the repairs.
pub fn merge_from(
    invocation: &Invocation,
    repo: &Repo,
    upstream: &str,
    incoming: usize,
    _label: &str,
) -> Result<i32> {
    merge(invocation, repo, upstream, false, incoming)
}

fn merge(
    invocation: &Invocation,
    repo: &Repo,
    upstream: &str,
    rebase: bool,
    incoming: usize,
) -> Result<i32> {
    let authors: BTreeSet<String> = repo
        .run(["log", "--format=%an", &format!("HEAD..{upstream}")])?
        .lines()
        .map(str::to_owned)
        .collect();
    let names = authors.into_iter().collect::<Vec<_>>().join(", ");
    println!(
        "Pulling {incoming} commit{} from {}…",
        if incoming == 1 { "" } else { "s" },
        if names.is_empty() {
            upstream.to_owned()
        } else {
            names.clone()
        }
    );
    let before = repo.run(["rev-parse", "HEAD"])?.trim().to_owned();
    let mut command = std::process::Command::new("git");
    command.current_dir(repo.root()).env(
        "GANG_THEIRS_NAME",
        if names.contains(',') || names.is_empty() {
            "theirs".to_owned()
        } else {
            names.clone()
        },
    );
    if rebase {
        command.args(["rebase", "-q", upstream]);
    } else {
        command.args(["merge", "--no-edit", "-q", upstream]);
    }
    let status = command.status()?;
    let code = git::exit_code(status);
    if code != 0 {
        let code = crate::wrap::offer_bang(repo, code)?;
        if code != 0 || !crate::bang::unmerged(repo)?.is_empty() {
            return Ok(if code == 0 { 1 } else { code });
        }
    }
    let _ = invocation;
    summarize(repo, &before, &names);
    warn_about_my_locks(repo, &before);
    if let Err(err) = crate::prefabs_cmd::after_merge(repo, &before) {
        ui::warn(format!("could not check rebuilt prefabs: {err:#}"));
    }
    Ok(0)
}

/// Incoming changes that touched what I have locked (someone bypassed GANG).
fn warn_about_my_locks(repo: &Repo, before: &str) {
    let Ok(me) = presence::identity(repo).map(|i| i.login) else {
        return;
    };
    let Ok(table) = gang_core::locks::read(repo) else {
        return;
    };
    let mine: Vec<&gang_core::locks::Lock> = table.locks.iter().filter(|l| l.owner == me).collect();
    if mine.is_empty() {
        return;
    }
    let Ok(changed) = repo.run(["diff", "--name-only", "-z", before, "HEAD"]) else {
        return;
    };
    for path in split_nul(&changed) {
        if let Some(lock) = mine
            .iter()
            .find(|l| l.targets().any(|t| t.covers_file(&path)))
        {
            ui::warn(format!(
                "Incoming changes touched {path}, which you have locked ({}).",
                lock.root.label()
            ));
        }
    }
}

/// "Merged automatically from mehmet:" with what changed in Unity files.
fn summarize(repo: &Repo, before: &str, names: &str) {
    let Ok(changed) = repo.run(["diff", "--name-only", "-z", before, "HEAD"]) else {
        return;
    };
    let unity: Vec<String> = split_nul(&changed)
        .into_iter()
        .filter(|p| attributes::is_unity_yaml(p))
        .collect();
    if unity.is_empty() {
        return;
    }
    let guids = semantic::name_index(repo);
    let rules = semantic::rules(&Config::load(repo.root()).unwrap_or_default());
    println!(
        "Merged automatically{}:",
        if names.is_empty() {
            String::new()
        } else {
            format!(" from {names}")
        }
    );
    for path in unity {
        let old = semantic::blob(repo, &format!("{before}:{path}"))
            .ok()
            .flatten();
        let new = semantic::blob(repo, &format!("HEAD:{path}")).ok().flatten();
        let parse = |t: Option<String>| t.and_then(|t| unity_yaml::parse(&t).ok());
        let (Some(old), Some(new)) = (parse(old), parse(new)) else {
            println!("  {path}");
            continue;
        };
        let changes = gang_merge::diff::diff(&old, &new, &rules);
        let mut ctx = gang_merge::render::Context::new(&old, &new, Some(&guids));
        ctx.layer_names = gang_core::layers::names_for(repo, &path);
        let lines = gang_merge::render::render(&changes, &ctx, false);
        // Whose work it was (field test E8).
        let authors: Vec<String> = repo
            .run([
                "log",
                "--format=%an",
                &format!("{before}..HEAD"),
                "--no-merges",
                "--",
                &path,
            ])
            .map(|out| {
                let mut seen: Vec<String> = Vec::new();
                for name in out.lines() {
                    if !seen.iter().any(|s| s == name) {
                        seen.push(name.to_owned());
                    }
                }
                seen
            })
            .unwrap_or_default();
        let by = if authors.is_empty() {
            String::new()
        } else {
            format!(" ({})", authors.join(", "))
        };
        println!(
            "  {}{by}: {} change{}",
            path.rsplit('/').next().unwrap_or(&path),
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        );
        for line in lines.iter().take(6) {
            println!("    {line}");
        }
        if lines.len() > 6 {
            println!("    … {} more", lines.len() - 6);
        }
    }
}
