//! `gang ignore`: the managed `.gitignore` (DESIGN.md §17.1).

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gang_core::invocation::Invocation;
use gang_core::managed::{Kind, ManagedFile};
use gang_core::repo::{Repo, split_nul};
use gang_core::{ignore, setup, unity};

use crate::ui;

/// Manage ignore rules shared by the team
#[derive(Parser)]
#[command(name = "gang ignore")]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add rules (normalized for every OS)
    Add {
        /// Rules, as in .gitignore
        #[arg(required = true)]
        rules: Vec<String>,
        /// Only for you: write to .git/info/exclude instead of .gitignore
        #[arg(long)]
        local: bool,
    },
    /// Regenerate the managed rules in .gitignore and .gitattributes
    Update,
    /// Show whether, and why, paths are ignored
    Check {
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Find ignore problems: tracked junk, missing .meta rules, line endings
    Doctor {
        /// Fix what can be fixed safely
        #[arg(long)]
        fix: bool,
    },
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = Args::parse_from(
        std::iter::once(OsString::from("gang ignore")).chain(invocation.args.iter().cloned()),
    );
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    match args.command {
        Command::Add { rules, local } => add(&repo, &rules, local),
        Command::Update => update(&repo),
        Command::Check { paths } => check(&repo, &paths),
        Command::Doctor { fix } => doctor(&repo, fix),
    }
}

fn single_project(repo: &Repo) -> Option<unity::UnityProject> {
    let mut projects = unity::find_projects(repo.root());
    (projects.len() == 1).then(|| projects.remove(0))
}

fn add(repo: &Repo, rules: &[String], local: bool) -> Result<i32> {
    let project = single_project(repo);
    let normalized: Vec<String> = rules
        .iter()
        .flat_map(|rule| ignore::normalize_rule(rule, project.as_ref()))
        .collect();
    if normalized.is_empty() {
        bail!("no rules to add");
    }

    if local {
        let path = repo.git_dir().join("info").join("exclude");
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        let mut added = Vec::new();
        for rule in &normalized {
            if !text.lines().any(|line| line.trim() == rule) {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(rule);
                text.push('\n');
                added.push(rule);
            }
        }
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, text)?;
        for rule in added {
            ui::done(format!("{rule}  (only for you, in .git/info/exclude)"));
        }
        return Ok(0);
    }

    let path = repo.root().join(".gitignore");
    let mut file = ManagedFile::parse(&fs::read_to_string(&path).unwrap_or_default());
    for rule in &normalized {
        if file.rules.insert(rule.clone()) {
            ui::done(rule);
        } else {
            println!("· {rule} (already there)");
        }
    }
    fs::write(&path, file.render(Kind::Ignore))?;
    if !setup::is_gang_repo(repo) || file.version.is_none() {
        ui::hint("Run `gang init` to add the managed rules for Unity and every OS.");
    }
    Ok(0)
}

fn update(repo: &Repo) -> Result<i32> {
    let projects = unity::find_projects(repo.root());
    let changed = setup::write_managed_files(repo, &projects)?;
    if changed.is_empty() {
        ui::done("Managed rules are up to date");
    }
    for file in changed {
        ui::done(format!("updated {file}"));
    }
    Ok(0)
}

fn check(repo: &Repo, paths: &[String]) -> Result<i32> {
    let mut child = repo
        .command()
        .args(["check-ignore", "-v", "-n", "--no-index", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run git check-ignore")?;
    {
        let mut stdin = child.stdin.take().context("no stdin")?;
        for path in paths {
            stdin.write_all(path.as_bytes())?;
            stdin.write_all(b"\0")?;
        }
    }
    let output = child.wait_with_output()?;
    // Exit 1 just means "nothing ignored"; 128 is a real error.
    if output.status.code() == Some(128) {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let fields = split_fields(&String::from_utf8_lossy(&output.stdout));
    for chunk in fields.chunks(4) {
        let [source, line, pattern, path] = chunk else {
            continue;
        };
        if pattern.is_empty() {
            println!("{path}: not ignored");
        } else if pattern.starts_with('!') {
            println!("{path}: not ignored, re-included by {pattern} ({source}:{line})");
        } else {
            println!("{path}: ignored by {pattern} ({source}:{line})");
        }
    }
    Ok(0)
}

/// `-z` output keeps empty fields, unlike [`split_nul`].
fn split_fields(text: &str) -> Vec<String> {
    let mut fields: Vec<String> = text.split('\0').map(str::to_owned).collect();
    if fields.last().is_some_and(String::is_empty) {
        fields.pop();
    }
    fields
}

fn doctor(repo: &Repo, fix: bool) -> Result<i32> {
    let mut problems = 0;
    let path = repo.root().join(".gitignore");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let file = ManagedFile::parse(&text);

    if file.version.is_none() {
        problems += 1;
        ui::warn(".gitignore has no GANG-managed rules.");
        ui::hint("Fix: gang init (or gang ignore update)");
    } else if file.version < Some(ignore::VERSION) {
        problems += 1;
        ui::warn("The managed rules in .gitignore are from an older GANG.");
        ui::hint("Fix: gang ignore update");
    }
    if text.contains('\r') {
        problems += 1;
        ui::warn(".gitignore has Windows line endings; its rules do not match on macOS or Linux.");
        ui::hint("Fix: gang ignore update");
    }
    let project = single_project(repo);
    for rule in &file.rules {
        let normalized = ignore::normalize_rule(rule, project.as_ref());
        let missing: Vec<&String> = normalized
            .iter()
            .filter(|r| !file.rules.contains(*r))
            .collect();
        if !missing.is_empty() {
            problems += 1;
            ui::warn(format!(
                "Rule {rule} does not match the same files on every OS or misses its .meta."
            ));
            ui::hint(format!("Should be: {}", normalized.join("  ")));
        }
    }

    let tracked_ignored = split_nul(&repo.run(["ls-files", "-ci", "--exclude-standard", "-z"])?);
    if !tracked_ignored.is_empty() {
        problems += 1;
        ui::warn(format!(
            "{} tracked files match ignore rules:",
            tracked_ignored.len()
        ));
        for file in tracked_ignored.iter().take(10) {
            ui::hint(file);
        }
        if tracked_ignored.len() > 10 {
            ui::hint(format!("… and {} more", tracked_ignored.len() - 10));
        }
        ui::hint("Fix: gang ignore doctor --fix (stops tracking them; the files stay on disk)");
    }

    if !fix {
        if problems == 0 {
            ui::done("No ignore problems found");
        }
        return Ok(if problems == 0 { 0 } else { 1 });
    }

    let projects = unity::find_projects(repo.root());
    let mut rules = ManagedFile::parse(&text);
    rules.rules = rules
        .rules
        .iter()
        .flat_map(|rule| ignore::normalize_rule(rule, project.as_ref()))
        .collect();
    fs::write(&path, rules.render(Kind::Ignore))?;
    for changed in setup::write_managed_files(repo, &projects)? {
        ui::done(format!("updated {changed}"));
    }
    if !tracked_ignored.is_empty() {
        let mut args = vec!["rm", "-r", "-q", "--cached", "--"];
        args.extend(tracked_ignored.iter().map(String::as_str));
        repo.run(&args)?;
        ui::done(format!(
            "stopped tracking {} files (commit to apply)",
            tracked_ignored.len()
        ));
    }
    Ok(0)
}
