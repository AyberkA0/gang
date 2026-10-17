//! Commands git calls: `gang merge-driver` and `gang hook`, plus
//! `gang config --project`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use gang_core::invocation::Invocation;
use gang_core::managed::{self, Kind};
use gang_core::repo::Repo;
use gang_core::{config, git};

/// `gang merge-driver <unity|ignore> <base> <ours> <theirs> [path]`: git's
/// merge driver. Writes the result to `<ours>`; exits non-zero on conflicts.
pub fn merge_driver(invocation: &Invocation) -> Result<i32> {
    let args: Vec<String> = invocation
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let [kind, base, ours, theirs, rest @ ..] = args.as_slice() else {
        bail!("usage: gang merge-driver <unity|ignore> <base> <ours> <theirs> [path]");
    };
    let path = rest.first().map(String::as_str).unwrap_or("");
    match kind.as_str() {
        "ignore" => {
            let kind = if path.ends_with(".gitattributes") {
                Kind::Attributes
            } else {
                Kind::Ignore
            };
            let read =
                |p: &str| fs::read_to_string(p).with_context(|| format!("could not read {p}"));
            let merged = managed::merge(&read(base)?, &read(ours)?, &read(theirs)?, kind);
            fs::write(ours, merged)?;
            Ok(0)
        }
        // Until the semantic merge lands (PLAN.md P7), merge as text like git.
        "unity" => text_merge(base, ours, theirs),
        other => bail!("unknown merge driver kind {other:?}"),
    }
}

fn text_merge(base: &str, ours: &str, theirs: &str) -> Result<i32> {
    let code = git::run_inherited([
        "merge-file",
        "-L",
        "ours",
        "-L",
        "base",
        "-L",
        "theirs",
        ours,
        base,
        theirs,
    ])?;
    Ok(code.clamp(0, 1))
}

/// `gang config --project <key> [<value>]`; any other `gang config` is git's.
pub fn config(invocation: &Invocation) -> Result<Option<i32>> {
    let Some(first) = invocation.args.first() else {
        return Ok(None);
    };
    if first != "--project" {
        return Ok(None);
    }
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    let path = repo.root().join(config::PATH);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("{} not found; run `gang init` first", config::PATH))?;
    let rest: Vec<String> = invocation.args[1..]
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    match rest.as_slice() {
        [key] => match config::get(&text, key)? {
            Some(value) => {
                println!("{value}");
                Ok(Some(0))
            }
            None => Ok(Some(1)),
        },
        [key, value] => {
            write(&path, &config::set(&text, key, value)?)?;
            Ok(Some(0))
        }
        _ => bail!("usage: gang config --project <key> [<value>]"),
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}
