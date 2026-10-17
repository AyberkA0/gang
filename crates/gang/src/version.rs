use std::ffi::OsString;

use anyhow::Result;
use clap::Parser;
use gang_core::git::{self, GitError, MIN_GIT};
use gang_core::invocation::Invocation;
use serde_json::json;

/// Show GANG, git and git-lfs versions
#[derive(Parser)]
#[command(name = "gang version")]
pub struct Args {
    /// Machine-readable output
    #[arg(long)]
    json: bool,
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = Args::parse_from(
        std::iter::once(OsString::from("gang version")).chain(invocation.args.iter().cloned()),
    );
    let gang = env!("CARGO_PKG_VERSION");
    let git = git::git_version();
    let lfs = git::lfs_version();

    if args.json || invocation.options.json {
        let report = json!({
            "gang": gang,
            "git": git.as_ref().ok().map(ToString::to_string),
            "git_lfs": lfs.map(|version| version.to_string()),
        });
        println!("{report}");
        return Ok(0);
    }

    println!("gang     {gang}");
    match git {
        Ok(version) if version < MIN_GIT => {
            println!("git      {version} (GANG needs {MIN_GIT} or newer)")
        }
        Ok(version) => println!("git      {version}"),
        Err(GitError::NotFound) => println!("git      not found"),
        Err(err) => println!("git      unknown ({err})"),
    }
    match lfs {
        Some(version) => println!("git-lfs  {version}"),
        None => println!("git-lfs  not found"),
    }
    Ok(0)
}
