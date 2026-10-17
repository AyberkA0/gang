mod help;
mod ignore_cmd;
mod panel;
mod plumbing;
mod presence_cmd;
mod save_cmd;
mod semantic_cmd;
mod setup_cmd;
mod ui;
mod version;
mod wrap;

use anyhow::Result;
use gang_core::commands;
use gang_core::git::{self, GitError};
use gang_core::invocation::Invocation;

fn main() {
    let invocation = Invocation::parse(std::env::args_os().skip(1));
    let code = match run(&invocation) {
        Ok(code) => code,
        Err(err) => {
            report(&err);
            1
        }
    };
    presence_cmd::after_command(&invocation);
    std::process::exit(code);
}

fn run(invocation: &Invocation) -> Result<i32> {
    if invocation.options.help {
        help::print_main();
        return Ok(0);
    }
    if invocation.options.version {
        println!("gang {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if invocation.command.is_none() {
        if invocation.git_globals.is_empty() {
            if ui::interactive() {
                if let Some(repo) = gang_core::repo::Repo::discover(&[])? {
                    if gang_core::setup::is_gang_repo(&repo) {
                        return panel::run(repo);
                    }
                }
            }
            help::print_main();
            return Ok(0);
        }
        return passthrough(invocation);
    }

    match invocation.command_name() {
        Some("git") => plain_git(invocation),
        Some("version") => version::run(invocation),
        Some("help") => help::run(invocation),
        Some("init") => setup_cmd::init(invocation),
        Some("clone") => setup_cmd::clone(invocation),
        Some("ignore") => ignore_cmd::run(invocation),
        Some("merge-driver") => plumbing::merge_driver(invocation),
        Some("hook") => save_cmd::hook(invocation),
        Some("save") => save_cmd::save(invocation),
        Some("undo") => save_cmd::undo(invocation),
        Some("who") => presence_cmd::who(invocation),
        Some("tree") => semantic_cmd::tree(invocation),
        Some("presence-publish") => presence_cmd::publish(invocation),
        Some("config") => match plumbing::config(invocation)? {
            Some(code) => Ok(code),
            None => wrapped(invocation, "config", &[]),
        },
        Some(name) => match commands::wrapped_layers(name) {
            Some(layers) => wrapped(invocation, name, layers),
            None => passthrough(invocation),
        },
        None => passthrough(invocation),
    }
}

/// A git command GANG wraps (DESIGN.md §8.3).
fn wrapped(invocation: &Invocation, name: &str, layers: &[commands::Layer]) -> Result<i32> {
    debug(|| format!("wrapped `{name}`, layers {layers:?}"));
    wrap::run(invocation, name, layers)
}

/// A command GANG does not know: git runs it unchanged.
fn passthrough(invocation: &Invocation) -> Result<i32> {
    debug(|| "passed to git".to_owned());
    Ok(git::run_inherited(invocation.git_args())?)
}

/// `gang git <args>`: plain git, without any GANG layers.
fn plain_git(invocation: &Invocation) -> Result<i32> {
    let args = invocation.git_globals.iter().chain(&invocation.args);
    Ok(git::run_inherited(args)?)
}

/// Prints a routing trace to stderr when `GANG_DEBUG` is set.
fn debug(message: impl FnOnce() -> String) {
    if std::env::var_os("GANG_DEBUG").is_some() {
        eprintln!("gang: {}", message());
    }
}

fn report(err: &anyhow::Error) {
    eprintln!("✗ {err}");
    if let Some(hint) = err.downcast_ref::<GitError>().and_then(hint) {
        eprintln!("  {hint}");
    }
}

fn hint(err: &GitError) -> Option<&'static str> {
    match err {
        GitError::NotFound => Some(install_git_hint()),
        GitError::TooOld { .. } => {
            Some("Update git from https://git-scm.com/downloads and try again.")
        }
        GitError::UnknownVersion(_) | GitError::Io(_) => None,
    }
}

fn install_git_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "GANG runs git under the hood. Install it with `xcode-select --install` or `brew install git`."
    } else if cfg!(windows) {
        "GANG runs git under the hood. Install it with `winget install --id Git.Git -e` or from https://git-scm.com/downloads."
    } else {
        "GANG runs git under the hood. Install it with your package manager, e.g. `sudo apt install git`."
    }
}
