//! `gang who`, background presence publishing, and team notes in
//! `gang status` (DESIGN.md §9).

use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use gang_core::config::Config;
use gang_core::invocation::Invocation;
use gang_core::presence::{self, Presence, State};
use gang_core::repo::Repo;
use gang_core::{attributes, clock, setup};

use crate::ui;

/// Commands that never change anything worth publishing.
const READ_ONLY: &[&str] = &[
    "help",
    "version",
    "who",
    "log",
    "show",
    "diff",
    "blame",
    "grep",
    "shortlog",
    "describe",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "cat-file",
    "ls-remote",
    "presence-publish",
    "hook",
    "merge-driver",
    "credential",
    "auth",
];

/// Publishing is off in tests and when `GANG_NO_PRESENCE` is set.
fn enabled() -> bool {
    std::env::var_os("GANG_NO_PRESENCE").is_none()
}

/// After a command: publish this user's presence in the background, so the
/// command itself never waits for the network.
pub fn after_command(invocation: &Invocation) {
    let Some(name) = invocation.command_name() else {
        return;
    };
    if READ_ONLY.contains(&name) || !enabled() {
        return;
    }
    if let Ok(Some(repo)) = Repo::discover(&invocation.git_globals) {
        spawn_publish(&repo);
    }
}

/// Starts `gang presence-publish` detached from the terminal, at most once
/// every two seconds per clone.
pub fn spawn_publish(repo: &Repo) {
    if !enabled() || !setup::is_gang_repo(repo) {
        return;
    }
    let marker = repo.gang_dir().join("presence.spawned");
    let recent = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < Duration::from_secs(2));
    if recent {
        return;
    }
    let _ = std::fs::create_dir_all(repo.gang_dir());
    let _ = std::fs::write(&marker, "");
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    command
        .arg("presence-publish")
        .current_dir(repo.root())
        // Its git calls must not take index.lock from the user's next command.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    let _ = command.spawn();
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Its own process group: Ctrl+C in the terminal does not stop it.
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(DETACH_FLAGS);
}

/// A hidden console of its own, in its own process group (Ctrl+C in the
/// terminal does not stop it). Not `DETACHED_PROCESS`: a process without any
/// console makes Windows open a new console window for every `git` it runs.
#[cfg(windows)]
const DETACH_FLAGS: u32 = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}

/// `gang presence-publish [--force]` (hidden): publishes now.
pub fn publish(invocation: &Invocation) -> Result<i32> {
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    if !setup::is_gang_repo(&repo) {
        return Ok(0);
    }
    let config = Config::load(repo.root())?;
    let force = invocation.args.iter().any(|a| a == "--force");
    match presence::publish(&repo, &config, force)? {
        presence::Published::Pushed => println!("published"),
        presence::Published::Unchanged => println!("unchanged"),
        presence::Published::NoRemote => println!("no origin remote"),
    }
    Ok(0)
}

/// `gang who [path]`: who is changing what.
pub fn who(invocation: &Invocation) -> Result<i32> {
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    let config = Config::load(repo.root())?;
    if let Err(err) = presence::poll(&repo) {
        ui::warn(format!("could not reach the team: {err:#}"));
    }
    let me = presence::identity(&repo)?.login;
    let mine = presence::current(&repo, &config, None)?;
    let mut team: Vec<Presence> = presence::team(&repo)?
        .into_iter()
        .filter(|p| p.user != me)
        .collect();
    team.insert(0, mine);

    let filter = invocation
        .args
        .first()
        .map(|a| a.to_string_lossy().replace('\\', "/"));
    if invocation.options.json {
        println!("{}", serde_json::to_string_pretty(&team)?);
        return Ok(0);
    }
    let now = clock::now();
    let offline_after = presence::offline_after(&config);

    if let Some(path) = filter {
        let mut any = false;
        for person in &team {
            for (file, flag) in &person.files {
                if file == &path || file.starts_with(&format!("{}/", path.trim_end_matches('/'))) {
                    any = true;
                    let since = clock::parse(&flag.since)
                        .map(|t| clock::ago(now - t))
                        .unwrap_or_default();
                    let who = if person.user == me {
                        format!("{} (you)", person.user)
                    } else {
                        person.user.clone()
                    };
                    println!("{who:<20} {:<9} {file}  {since}", flag.state.label());
                }
            }
        }
        if !any {
            println!("Nobody is changing {path}.");
        }
        return Ok(0);
    }

    for person in &team {
        let you = person.user == me;
        let online = you || person.is_online(now, offline_after);
        let dot = if online { "●" } else { "○" };
        let name = if you {
            format!("{} (you)", person.user)
        } else {
            person.user.clone()
        };
        let branch = person.branch.as_deref().unwrap_or("-");
        let mut summary = summarize(person);
        if !online {
            summary = format!("offline {} · {summary}", clock::ago(person.age(now)));
        }
        if person.editor_open {
            summary.push_str(" · Unity open");
        }
        println!("{dot} {name:<22} {:<8} {branch:<16} {summary}", person.os);
        for (file, flag) in person.files.iter().take(8) {
            println!("    {:<9} {file}", flag.state.label());
        }
        if person.files.len() > 8 {
            println!("    … and {} more", person.files.len() - 8);
        }
    }
    if team.len() == 1 {
        println!();
        println!(
            "No teammates have published yet. They appear once they use gang in this repository."
        );
    }
    Ok(0)
}

fn summarize(person: &Presence) -> String {
    let changed = person
        .files
        .values()
        .filter(|f| f.state != State::Stashed)
        .count();
    let mut parts = Vec::new();
    if changed > 0 {
        parts.push(format!(
            "{changed} file{} changed",
            if changed == 1 { "" } else { "s" }
        ));
    }
    if person.ahead > 0 {
        parts.push(format!(
            "{} unpushed commit{}",
            person.ahead,
            if person.ahead == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        "clean".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Lines for `gang status`: teammates changing the same files as you.
pub fn status_notes(repo: &Repo) -> Result<Vec<String>> {
    if !setup::is_gang_repo(repo) {
        return Ok(Vec::new());
    }
    let config = Config::load(repo.root())?;
    // Refresh at most once a minute; status must stay fast.
    let stamp = repo.gang_dir().join("presence.polled");
    let stale = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age > Duration::from_secs(60));
    if stale {
        let _ = presence::poll(repo);
        let _ = gang_core::locks::fetch(repo);
        let _ = std::fs::create_dir_all(repo.gang_dir());
        let _ = std::fs::write(&stamp, "");
    }
    let me = presence::identity(repo)?.login;
    let mine = presence::compute_flags(repo, presence::upstream(repo, &config)?.as_deref())?;
    let now = clock::now();
    let mut notes = Vec::new();
    for person in presence::team(repo)?.into_iter().filter(|p| p.user != me) {
        for (file, flag) in &person.files {
            if !mine.contains_key(file) || flag.state == State::Stashed {
                continue;
            }
            let since = clock::parse(&flag.since)
                .map(|t| clock::ago(now - t))
                .unwrap_or_default();
            if attributes::is_binary_path(file) {
                notes.push(format!(
                    "✗ {} is also changing {file} ({}, {since}). It cannot be merged: talk before you push, or lock it.",
                    person.user,
                    flag.state.label()
                ));
            } else if attributes::is_unity_yaml(file) {
                notes.push(format!(
                    "⚠ {} is also changing {file} ({}, {since}). It merges automatically unless you both change the same property.",
                    person.user,
                    flag.state.label()
                ));
            } else {
                notes.push(format!(
                    "⚠ {} is also changing {file} ({}, {since}).",
                    person.user,
                    flag.state.label()
                ));
            }
        }
    }
    Ok(notes)
}

/// Your changes that touch someone else's lock.
fn lock_notes(repo: &Repo) -> Result<Vec<String>> {
    if !setup::is_gang_repo(repo) {
        return Ok(Vec::new());
    }
    let changed = gang_core::repo::split_nul(&repo.run(["diff", "--name-only", "-z", "HEAD"])?);
    let versions: Vec<(String, Option<String>, Option<String>)> = changed
        .into_iter()
        .map(|path| {
            let unity = attributes::is_unity_yaml(&path);
            let old = unity
                .then(|| {
                    gang_core::semantic::blob(repo, &format!("HEAD:{path}"))
                        .ok()
                        .flatten()
                })
                .flatten();
            let new = unity
                .then(|| gang_core::semantic::worktree(repo, &path))
                .flatten();
            (path, old, new)
        })
        .collect();
    let mut notes = Vec::new();
    for hit in crate::locks_cmd::check_changes(repo, false, &versions)? {
        notes.push(format!("⚠ {}", hit.message()));
        if let Some(target) = hit.restore {
            notes.push(format!("  Undo only that object: gang restore {target}"));
        }
    }
    Ok(notes)
}

/// Unity files whose changes are only noise.
fn noise_notes(repo: &Repo) -> Result<Vec<String>> {
    let rules = gang_core::semantic::rules(&Config::load(repo.root()).unwrap_or_default());
    let changed = gang_core::repo::split_nul(&repo.run(["diff", "--name-only", "-z", "HEAD"])?);
    let noise: Vec<String> = changed
        .into_iter()
        .filter(|p| attributes::is_unity_yaml(p))
        .take(200)
        .filter(|p| gang_core::semantic::is_noise_change(repo, p, &rules).unwrap_or(false))
        .collect();
    let mut notes: Vec<String> = noise.iter().map(|p| format!("{p}: only noise")).collect();
    if !notes.is_empty() {
        notes.push("Run `gang restore --noise` to drop noise-only changes.".to_owned());
    }
    Ok(notes)
}

/// `gang status`: git's status, then team notes when on a terminal.
pub fn status(invocation: &Invocation, repo: Option<&Repo>) -> Result<i32> {
    let mut args: Vec<OsString> = invocation.git_globals.clone();
    args.push("status".into());
    args.extend(invocation.args.iter().cloned());
    let code = gang_core::git::run_inherited(&args)?;
    if code != 0 || invocation.options.raw || !ui::interactive() {
        return Ok(code);
    }
    if let Some(repo) = repo {
        let noise = noise_notes(repo).unwrap_or_default();
        if !noise.is_empty() {
            println!();
            println!("Unity:");
            for note in noise {
                println!("  {note}");
            }
        }
        let locked = lock_notes(repo).unwrap_or_default();
        if !locked.is_empty() {
            println!();
            println!("Locks:");
            for note in locked {
                println!("  {note}");
            }
        }
        let notes = status_notes(repo).unwrap_or_default();
        if !notes.is_empty() {
            println!();
            println!("Team:");
            for note in notes {
                println!("  {note}");
            }
        }
    }
    Ok(code)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn background_publisher_gets_a_hidden_console() {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        assert_eq!(DETACH_FLAGS & DETACHED_PROCESS, 0);
        assert_ne!(DETACH_FLAGS & CREATE_NO_WINDOW, 0);
    }
}
