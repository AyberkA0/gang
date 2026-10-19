//! `gang init` and `gang clone`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gang_core::invocation::Invocation;
use gang_core::repo::Repo;
use gang_core::{git, setup, unity};

use crate::ui;

/// git init options that take their value as the next argument.
const INIT_VALUE_OPTIONS: &[&str] = &[
    "-b",
    "--initial-branch",
    "--template",
    "--separate-git-dir",
    "--object-format",
    "--ref-format",
];

/// git clone options that take their value as the next argument.
const CLONE_VALUE_OPTIONS: &[&str] = &[
    "-b",
    "--branch",
    "-o",
    "--origin",
    "-c",
    "--config",
    "--depth",
    "--reference",
    "--reference-if-able",
    "--template",
    "--separate-git-dir",
    "-u",
    "--upload-pack",
    "-j",
    "--jobs",
    "--shallow-since",
    "--shallow-exclude",
    "--server-option",
    "--bundle-uri",
    "--ref-format",
    "--filter",
];

/// Positional arguments, skipping options and their values.
fn positionals(args: &[OsString], value_options: &[&str]) -> Vec<OsString> {
    let mut result = Vec::new();
    let mut args = args.iter();
    let mut options_done = false;
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if options_done || !text.starts_with('-') || text == "-" {
            result.push(arg.clone());
        } else if text == "--" {
            options_done = true;
        } else if value_options.contains(&text.as_ref()) {
            args.next();
        }
    }
    result
}

fn has_any(args: &[OsString], options: &[&str]) -> bool {
    args.iter().any(|arg| {
        let text = arg.to_string_lossy();
        options
            .iter()
            .any(|option| text == *option || text.starts_with(&format!("{option}=")))
    })
}

/// `gang init [git init options] [directory]`.
pub fn init(invocation: &Invocation) -> Result<i32> {
    git::ensure_supported()?;
    // GANG's own options: --github [--public] [--name <name>].
    let mut github = false;
    let mut public = false;
    let mut name: Option<String> = None;
    let mut rest: Vec<OsString> = Vec::new();
    let mut iter = invocation.args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--github") => github = true,
            Some("--public") => public = true,
            Some("--name") => name = iter.next().map(|n| n.to_string_lossy().into_owned()),
            _ => rest.push(arg.clone()),
        }
    }
    let args = &rest;
    let dir = positionals(args, INIT_VALUE_OPTIONS)
        .last()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let existing = Repo::open(&dir).ok();
    let mut git_args: Vec<OsString> = invocation.git_globals.clone();
    git_args.push("init".into());
    if existing.is_none() && !has_any(args, &["-b", "--initial-branch"]) {
        git_args.extend(["-b".into(), "main".into()]);
    }
    git_args.extend(args.iter().cloned());
    let code = git::run_inherited(&git_args)?;
    if code != 0 {
        return Ok(code);
    }

    let repo = Repo::open(&dir)?;
    set_up(&repo)?;
    if github {
        crate::auth_cmd::create_github_repo(invocation, &repo, name, public)?;
    }
    println!();
    println!("Next: commit the GANG files and push.");
    ui::hint("gang add .gitignore .gitattributes .gang");
    ui::hint("gang commit -m \"Set up GANG\"");
    Ok(0)
}

/// Writes managed files and registers drivers and hooks, reporting each step.
fn set_up(repo: &Repo) -> Result<()> {
    let projects = unity::find_projects(repo.root());
    if projects.is_empty() {
        ui::warn("No Unity project found (a folder with Assets/ and ProjectSettings/).");
        ui::hint("GANG still manages ignore rules and line endings; Unity features stay off.");
    }
    for project in &projects {
        let name = if project.dir.is_empty() {
            "."
        } else {
            &project.dir
        };
        let version = project
            .editor_version
            .as_deref()
            .unwrap_or("unknown version");
        ui::done(format!("Unity project {name} ({version})"));
        check_settings(repo, project)?;
    }

    for file in setup::write_managed_files(repo, &projects)? {
        ui::done(format!("wrote {file}"));
    }
    for step in setup::install(repo)? {
        if let Some(warning) = step.strip_prefix("⚠ ") {
            ui::warn(warning);
        } else {
            ui::done(step);
        }
    }
    Ok(())
}

fn check_settings(repo: &Repo, project: &unity::UnityProject) -> Result<()> {
    let dir = project.path(repo.root());
    let settings = unity::read_settings(&dir);
    if settings.is_ok() {
        return Ok(());
    }
    ui::warn("This project's settings break merging:");
    if settings.force_text != Some(true) {
        ui::hint("Asset Serialization should be Force Text (Project Settings › Editor).");
    }
    if settings.visible_meta == Some(false) {
        ui::hint(
            "Version Control mode should be Visible Meta Files (Project Settings › Version Control).",
        );
    }
    if unity::is_editor_open(&dir) {
        ui::hint("Unity is open: change these in Unity, or close it and run `gang init` again.");
        return Ok(());
    }
    if ui::interactive() && ui::confirm("Fix them now?", true)? {
        match unity::fix_settings(&dir) {
            Ok(changed) => {
                for change in changed {
                    ui::done(change);
                }
            }
            Err(err) => ui::warn(format!("{err:#}")),
        }
    }
    Ok(())
}

/// `gang clone <url | owner/repo> [directory] [git clone options]`.
pub fn clone(invocation: &Invocation) -> Result<i32> {
    git::ensure_supported()?;
    let mut args = invocation.args.clone();
    let positional = positionals(&args, CLONE_VALUE_OPTIONS);
    let Some(source) = positional.first().cloned() else {
        // Let git print its usage.
        return run_git(invocation, "clone", &args);
    };
    let url = expand_shorthand(&source.to_string_lossy());
    if let Some(slot) = args.iter_mut().find(|arg| **arg == source) {
        *slot = OsString::from(&url);
    }
    // Partial clone (free, built into git and GitHub): large files from the
    // history are downloaded when something needs them, not up front, so a
    // project full of art does not cost every teammate every version.
    let opt_out = args.iter().any(|a| a == "--no-partial");
    args.retain(|a| a != "--no-partial");
    let chosen = args.iter().any(|a| {
        let a = a.to_string_lossy();
        a.starts_with("--filter") || a.starts_with("--depth") || a == "--mirror" || a == "--bare"
    });
    let partial = !opt_out && !chosen && is_network_url(&url);
    if partial {
        args.insert(
            0,
            OsString::from(format!("--filter=blob:limit={PARTIAL_LIMIT}")),
        );
    }
    let code = run_git(invocation, "clone", &args)?;
    if code != 0 {
        return Ok(code);
    }
    if partial {
        ui::done(format!(
            "partial clone: older versions of files over {PARTIAL_LIMIT}B download only when needed (`gang clone --no-partial` for everything)"
        ));
    }

    let dir = positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_clone_dir(&url)));
    let repo =
        Repo::open(&dir).with_context(|| format!("cloned, but cannot open {}", dir.display()))?;
    after_clone(&repo)?;
    Ok(0)
}

/// Files larger than this are left out of a clone's history until needed.
const PARTIAL_LIMIT: &str = "5m";

/// A remote that serves partial clones (GitHub, any https or ssh git host);
/// local paths and `file://` copy everything anyway.
fn is_network_url(url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("ssh://")
        || (url.contains('@') && url.contains(':') && !url.contains("://"))
}

fn run_git(invocation: &Invocation, command: &str, args: &[OsString]) -> Result<i32> {
    let mut git_args = invocation.git_globals.clone();
    git_args.push(command.into());
    git_args.extend(args.iter().cloned());
    Ok(git::run_inherited(&git_args)?)
}

fn after_clone(repo: &Repo) -> Result<()> {
    if !setup::is_gang_repo(repo) {
        ui::warn("This repository is not set up for GANG yet.");
        ui::hint("Run `gang init` inside it to set it up for the team.");
        return Ok(());
    }
    for step in setup::install(repo)? {
        if let Some(warning) = step.strip_prefix("⚠ ") {
            ui::warn(warning);
        } else {
            ui::done(step);
        }
    }
    let config = gang_core::config::Config::load(repo.root()).unwrap_or_default();
    if gang_core::config::lfs_enabled(repo.root(), &config) && git::lfs_version().is_some() {
        repo.run(["lfs", "pull"])?;
        ui::done("LFS files downloaded");
    }
    let _ = gang_core::locks::fetch(repo);
    if gang_core::presence::fetch(repo).is_ok() {
        ui::done("team presence fetched");
    }
    check_editor_versions(repo);
    Ok(())
}

fn check_editor_versions(repo: &Repo) {
    let installed = unity::installed_editors();
    for project in unity::find_projects(repo.root()) {
        let Some(version) = project.editor_version else {
            continue;
        };
        if installed.contains(&version) {
            ui::done(format!("Unity {version} is installed"));
        } else {
            ui::warn(format!(
                "The project uses Unity {version}, which is not installed here."
            ));
            if !installed.is_empty() {
                ui::hint(format!("Found: {}", installed.join(", ")));
            }
            ui::hint(format!(
                "Install {version} with Unity Hub before opening the project; another version \
                 would re-save hundreds of files."
            ));
        }
    }
}

/// `owner/repo` → `https://github.com/owner/repo.git`.
fn expand_shorthand(source: &str) -> String {
    let is_shorthand = source.split('/').count() == 2
        && !source.contains(':')
        && !source.starts_with('.')
        && !Path::new(source).exists()
        && source
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if is_shorthand {
        format!("https://github.com/{}.git", source.trim_end_matches(".git"))
    } else {
        source.to_owned()
    }
}

/// The folder git clone creates: the last path segment without `.git`.
fn default_clone_dir(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    trimmed
        .rsplit(['/', ':', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn positionals_skip_options_and_values() {
        assert_eq!(
            positionals(
                &os(&["-b", "dev", "--depth", "1", "--bare", "url", "dir"]),
                CLONE_VALUE_OPTIONS
            ),
            os(&["url", "dir"])
        );
        assert_eq!(
            positionals(&os(&["--", "-weird"]), CLONE_VALUE_OPTIONS),
            os(&["-weird"])
        );
        assert_eq!(
            positionals(
                &os(&["-q", "--initial-branch=x", "game"]),
                INIT_VALUE_OPTIONS
            ),
            os(&["game"])
        );
    }

    #[test]
    fn shorthand() {
        assert_eq!(
            expand_shorthand("AyberkA0/forest-run"),
            "https://github.com/AyberkA0/forest-run.git"
        );
        assert_eq!(
            expand_shorthand("https://github.com/a/b.git"),
            "https://github.com/a/b.git"
        );
        assert_eq!(
            expand_shorthand("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        assert_eq!(expand_shorthand("../local/repo"), "../local/repo");
    }

    #[test]
    fn partial_clone_only_over_the_network() {
        assert!(is_network_url("https://github.com/a/b.git"));
        assert!(is_network_url("git@github.com:a/b.git"));
        assert!(is_network_url("ssh://git@host/a/b.git"));
        assert!(!is_network_url("file:///tmp/remote.git"));
        assert!(!is_network_url("/tmp/remote.git"));
        assert!(!is_network_url("C:/remote.git"));
    }

    #[test]
    fn clone_dir() {
        assert_eq!(
            default_clone_dir("https://github.com/a/forest-run.git"),
            "forest-run"
        );
        assert_eq!(default_clone_dir("git@github.com:a/b.git"), "b");
        assert_eq!(default_clone_dir("/tmp/remote.git/"), "remote");
    }
}
