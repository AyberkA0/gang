//! `gang save`, `gang undo` and `gang hook`.

use std::ffi::OsString;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use clap::Parser;
use gang_core::guards::{self, Allow, Severity};
use gang_core::invocation::Invocation;
use gang_core::repo::Repo;
use gang_core::{git, safety, setup};

use crate::{ui, wrap};

/// Add every change and commit it, after GANG's checks
#[derive(Parser)]
#[command(name = "gang save")]
pub struct SaveArgs {
    /// Commit message
    #[arg(short, long)]
    message: Option<String>,
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

    let add_args: Vec<OsString> = if args.paths.is_empty() {
        vec!["-A".into()]
    } else {
        std::iter::once(OsString::from("--"))
            .chain(args.paths.iter().map(OsString::from))
            .collect()
    };
    let code = wrap::add(invocation, &repo, &add_args)?;
    if code != 0 {
        return Ok(code);
    }
    if guards::staged_changes(&repo)?.is_empty() {
        println!("Nothing to save.");
        return Ok(0);
    }

    let message = match args.message {
        Some(message) => message,
        None if ui::interactive() => {
            print!("Message: ");
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            let line = line.trim().to_owned();
            if line.is_empty() {
                bail!("a message is needed to save");
            }
            line
        }
        None => bail!("a message is needed: gang save -m \"…\""),
    };

    let op = safety::begin(&repo, "save", false)?;
    let mut commit = invocation.git_globals.clone();
    commit.extend(["commit".into(), "-q".into(), "-m".into(), message.into()]);
    let code = git::run_inherited(&commit)?;
    if code != 0 {
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
        // pre-push (locks, P9), post-checkout/commit/merge (presence, P5).
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
    if blocked {
        eprintln!();
        eprintln!("Commit stopped by GANG. Fix the problems above and commit again.");
        return Ok(1);
    }
    Ok(0)
}
