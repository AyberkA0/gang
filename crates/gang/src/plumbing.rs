//! Commands git calls: `gang merge-driver` and `gang hook`, plus
//! `gang config --project`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use gang_core::invocation::Invocation;
use gang_core::managed::{self, Kind};
use gang_core::repo::Repo;
use gang_core::{config, layers, store};

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
        "unity" | "json" => semantic_merge(kind, base, ours, theirs, path),
        other => bail!("unknown merge driver kind {other:?}"),
    }
}

/// Semantic merge of a Unity YAML or JSON file. Binary files and files that
/// do not parse get git's text merge only when it is clean: conflict
/// markers are never written into these files (DESIGN.md G2). Otherwise
/// the file keeps the current side and `gang bang` asks for the whole file.
fn semantic_merge(kind: &str, base: &str, ours: &str, theirs: &str, path: &str) -> Result<i32> {
    let read = |p: &str| fs::read(p).with_context(|| format!("could not read {p}"));
    let (b, o, t) = (read(base)?, read(ours)?, read(theirs)?);
    if [&b, &o, &t].iter().any(|bytes| bytes.contains(&0)) {
        // Binary: keep ours and report a conflict; `gang bang` asks.
        return Ok(1);
    }
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).ok();
    let (Some(b), Some(o), Some(t)) = (text(b), text(o), text(t)) else {
        return clean_text_merge(base, ours, theirs, kind);
    };
    let Some(repo) = gang_core::repo::Repo::discover(&[])? else {
        return clean_text_merge(base, ours, theirs, kind);
    };
    let config = gang_core::config::Config::load(repo.root()).unwrap_or_default();
    let rules = gang_core::semantic::rules(&config);
    let format = if kind == "json" {
        store::Format::Json
    } else {
        store::Format::Unity
    };
    let theirs_name = std::env::var("GANG_THEIRS_NAME").unwrap_or_else(|_| other_side_name(&repo));
    // During a rebase git's "ours" is the upstream commit and "theirs" the
    // user's own commit being replayed. GANG's "mine" is always the user's
    // side, so the versions swap; the result still goes to git's <ours>.
    let rebasing = repo.rebasing();
    let (b, mine, upstream) = if rebasing {
        // A layer the pass moved before this rebase: the user's commits are
        // replayed with it at its new number (DESIGN.md §14.2).
        let replay = |text: String| layers::replay_moves(&repo, path, &text).unwrap_or(text);
        (replay(b), replay(t), o)
    } else {
        (b, o, t)
    };
    match store::driver_merge(
        &repo,
        path,
        format,
        &b,
        &mine,
        &upstream,
        &rules,
        &theirs_name,
    ) {
        Some((merged, conflicts)) => {
            fs::write(ours, merged)?;
            Ok(if conflicts { 1 } else { 0 })
        }
        None => clean_text_merge(base, ours, theirs, kind),
    }
}

/// git's line merge, kept only when it has no conflicts and its result is
/// still a valid file when the inputs were. Otherwise `<ours>` is left as
/// it is (a valid file) and the merge reports a conflict.
fn clean_text_merge(base: &str, ours: &str, theirs: &str, kind: &str) -> Result<i32> {
    let output = std::process::Command::new("git")
        .args([
            "merge-file",
            "-p",
            "-L",
            "ours",
            "-L",
            "base",
            "-L",
            "theirs",
        ])
        .args([ours, base, theirs])
        .output()?;
    if output.status.code() != Some(0) {
        return Ok(1);
    }
    let merged = output.stdout;
    let parses = |bytes: &[u8]| match std::str::from_utf8(bytes) {
        Ok(text) if kind == "json" => serde_json::from_str::<serde_json::Value>(text).is_ok(),
        Ok(text) => unity_yaml::parse(text).is_ok(),
        Err(_) => false,
    };
    let ours_parsed = fs::read(ours).map(|b| parses(&b)).unwrap_or(false);
    if ours_parsed && !parses(&merged) {
        return Ok(1);
    }
    fs::write(ours, merged)?;
    Ok(0)
}

/// A name for the other side of the merge: the merged branch's author, or
/// during a rebase the author of the upstream commit it replays onto.
fn other_side_name(repo: &gang_core::repo::Repo) -> String {
    let onto = repo.rebase_onto();
    let heads: Vec<&str> = match &onto {
        Some(onto) => vec![onto.as_str()],
        None => vec!["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"],
    };
    for head in heads {
        if let Ok(Some(name)) = repo.try_run(["log", "-1", "--format=%an", head]) {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_owned();
            }
        }
    }
    "the other side".to_owned()
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
