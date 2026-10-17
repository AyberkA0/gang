//! Unity-aware commands and output: `gang tree`, and the semantic view in
//! `gang diff`, `gang show` and `gang log --unity` (DESIGN.md §12).

use std::ffi::OsString;

use anyhow::{Context, Result, bail};
use clap::Parser;
use gang_core::config::Config;
use gang_core::invocation::Invocation;
use gang_core::repo::{Repo, split_nul};
use gang_core::{attributes, git, semantic};
use gang_merge::diff::{self, Rules};
use gang_merge::render::{self, Context as RenderContext};
use unity_yaml::{GuidIndex, Hierarchy, ObjectKind};

use crate::ui;

/// Show the objects of a scene or prefab
#[derive(Parser)]
#[command(name = "gang tree")]
pub struct TreeArgs {
    /// A scene or prefab, optionally with an object: Level1.unity::Environment/Player
    target: String,
    /// How many levels to show
    #[arg(long)]
    depth: Option<usize>,
}

pub fn tree(invocation: &Invocation) -> Result<i32> {
    let args = TreeArgs::parse_from(
        std::iter::once(OsString::from("gang tree")).chain(invocation.args.iter().cloned()),
    );
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    let (file_part, object) = match args.target.split_once("::") {
        Some((file, object)) => (file.to_owned(), Some(object.to_owned())),
        None => (args.target.clone(), None),
    };
    let text = std::fs::read_to_string(&file_part)
        .with_context(|| format!("could not read {file_part}"))?;
    let file = unity_yaml::parse(&text).map_err(|e| anyhow::anyhow!("{file_part}: {e}"))?;
    let guids = semantic::guid_index(&repo);
    let lookup = |guid: &str| guids.name(guid);
    let hierarchy = Hierarchy::build_with(&file, &lookup);
    let ctx = RenderContext::new(&file, &file, Some(&guids));

    let roots: Vec<i64> = match &object {
        Some(path) if !path.is_empty() => vec![
            hierarchy
                .resolve(path)
                .with_context(|| format!("no object {path:?} in {file_part}"))?,
        ],
        _ => hierarchy.roots().to_vec(),
    };
    if invocation.options.json {
        let objects: Vec<serde_json::Value> = roots
            .iter()
            .flat_map(|root| hierarchy.subtree(*root))
            .filter_map(|id| hierarchy.object(id))
            .map(|o| {
                serde_json::json!({
                    "id": o.id,
                    "path": hierarchy.path(o.id),
                    "kind": if o.kind == ObjectKind::PrefabInstance { "prefab" } else { "object" },
                    "components": o.components.iter().map(|c| ctx.component_name(*c)).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&objects)?);
        return Ok(0);
    }
    for (i, root) in roots.iter().enumerate() {
        print_object(
            &hierarchy,
            &ctx,
            *root,
            "",
            i + 1 == roots.len(),
            object.is_some(),
            0,
            args.depth,
        );
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn print_object(
    h: &Hierarchy,
    ctx: &RenderContext<'_>,
    id: i64,
    prefix: &str,
    last: bool,
    top: bool,
    level: usize,
    depth: Option<usize>,
) {
    let Some(object) = h.object(id) else { return };
    let (branch, next_prefix) = if top && level == 0 {
        ("", String::new())
    } else if last {
        ("└─ ", format!("{prefix}   "))
    } else {
        ("├─ ", format!("{prefix}│  "))
    };
    let mut name = object.name.clone();
    if object.kind == ObjectKind::PrefabInstance {
        name.push_str(" (prefab)");
    }
    let scripts: Vec<String> = object
        .components
        .iter()
        .map(|c| ctx.component_name(*c))
        .filter(|n| !matches!(n.as_str(), "Transform" | "RectTransform"))
        .collect();
    let label = format!("{prefix}{branch}{name}");
    println!("{label:<44} &{:<20} {}", object.id, scripts.join(", "));
    if depth.is_some_and(|d| level + 1 >= d) {
        if !object.children.is_empty() {
            println!("{next_prefix}└─ … {} more", h.subtree(id).len() - 1);
        }
        return;
    }
    for (i, child) in object.children.iter().enumerate() {
        print_object(
            h,
            ctx,
            *child,
            &next_prefix,
            i + 1 == object.children.len(),
            false,
            level + 1,
            depth,
        );
    }
}

/// Which two versions `gang diff <args>` compares.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Side {
    Worktree,
    Index,
    Rev(String),
}

struct DiffRequest {
    old: Side,
    new: Side,
    revs: Vec<String>,
    paths: Vec<String>,
}

/// Understands the common forms of `git diff`; `None` for anything else,
/// which then runs as plain git.
fn diff_request(repo: &Repo, args: &[String]) -> Option<DiffRequest> {
    let mut cached = false;
    let mut revs = Vec::new();
    let mut paths = Vec::new();
    let mut after_separator = false;
    for arg in args {
        if after_separator {
            paths.push(arg.clone());
        } else if arg == "--" {
            after_separator = true;
        } else if arg == "--cached" || arg == "--staged" {
            cached = true;
        } else if arg.starts_with('-') {
            return None;
        } else if is_rev(repo, arg) {
            revs.push(arg.clone());
        } else {
            paths.push(arg.clone());
        }
    }
    let rev = |r: &str| Side::Rev(r.to_owned());
    let (old, new) = match (revs.as_slice(), cached) {
        ([], false) => (Side::Index, Side::Worktree),
        ([], true) => (rev("HEAD"), Side::Index),
        ([one], false) if one.contains("...") => {
            let (a, b) = one.split_once("...")?;
            let base = repo.try_run(["merge-base", a, b]).ok()??;
            (rev(base.trim()), rev(b))
        }
        ([one], false) if one.contains("..") => {
            let (a, b) = one.split_once("..")?;
            (rev(a), rev(if b.is_empty() { "HEAD" } else { b }))
        }
        ([one], false) => (rev(one), Side::Worktree),
        ([one], true) => (rev(one), Side::Index),
        ([a, b], false) => (rev(a), rev(b)),
        _ => return None,
    };
    Some(DiffRequest {
        old,
        new,
        revs,
        paths,
    })
}

fn is_rev(repo: &Repo, arg: &str) -> bool {
    let spec = if arg.contains("..") {
        arg.split("..").next().unwrap_or(arg)
    } else {
        arg
    };
    let spec = if spec.is_empty() { "HEAD" } else { spec };
    repo.try_run(["rev-parse", "-q", "--verify", &format!("{spec}^{{commit}}")])
        .ok()
        .flatten()
        .is_some()
}

fn read_side(repo: &Repo, side: &Side, path: &str) -> Result<Option<String>> {
    match side {
        Side::Worktree => Ok(semantic::worktree(repo, path)),
        Side::Index => semantic::blob(repo, &format!(":{path}")),
        Side::Rev(rev) => semantic::blob(repo, &format!("{rev}:{path}")),
    }
}

/// Semantic lines for one file between two texts.
fn file_lines(
    old: Option<&str>,
    new: Option<&str>,
    guids: &GuidIndex,
    rules: &Rules,
) -> Option<Vec<String>> {
    let empty = unity_yaml::File {
        prefix: String::new(),
        docs: Vec::new(),
    };
    let old = match old {
        Some(text) => unity_yaml::parse(text).ok()?,
        None => empty.clone(),
    };
    let new = match new {
        Some(text) => unity_yaml::parse(text).ok()?,
        None => empty,
    };
    let changes = diff::diff(&old, &new, rules);
    let ctx = RenderContext::new(&old, &new, Some(guids));
    let mut lines = render::render(&changes, &ctx, false);
    if lines.is_empty() && !changes.is_empty() {
        let noise = changes.iter().filter(|c| c.is_noise()).count();
        lines.push(format!(
            "only noise ({noise} change{}); `gang restore --noise` drops it",
            if noise == 1 { "" } else { "s" }
        ));
    }
    Some(lines)
}

/// `gang diff` on a terminal: git's diff for other files, then GANG's view
/// of Unity files. `None` when the arguments need plain git.
pub fn diff(invocation: &Invocation, repo: &Repo) -> Result<Option<i32>> {
    if invocation.options.raw || !ui::interactive() {
        return Ok(None);
    }
    let args: Vec<String> = invocation
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let Some(request) = diff_request(repo, &args) else {
        return Ok(None);
    };
    let mut name_args = vec!["diff".to_owned(), "--name-only".to_owned(), "-z".to_owned()];
    if request.old == Side::Rev("HEAD".into()) && request.new == Side::Index {
        name_args.push("--cached".into());
    }
    if let (Side::Rev(_), Side::Index) = (&request.old, &request.new) {
        if !name_args.contains(&"--cached".to_owned()) {
            name_args.push("--cached".into());
        }
    }
    name_args.extend(request.revs.iter().cloned());
    name_args.push("--".into());
    name_args.extend(request.paths.iter().cloned());
    let changed = split_nul(&repo.run(&name_args)?);
    let (unity, other): (Vec<String>, Vec<String>) = changed
        .into_iter()
        .partition(|p| attributes::is_unity_yaml(p));
    if unity.is_empty() {
        return Ok(None);
    }

    if !other.is_empty() {
        let mut git_args: Vec<OsString> = invocation.git_globals.clone();
        git_args.extend(["--no-pager".into(), "diff".into()]);
        git_args.extend(
            name_args[3..name_args.len() - request.paths.len() - 1]
                .iter()
                .map(OsString::from),
        );
        git_args.push("--".into());
        git_args.extend(other.iter().map(OsString::from));
        git::run_inherited(&git_args)?;
    }
    print_semantic(repo, &unity, |path| {
        Ok((
            read_side(repo, &request.old, path)?,
            read_side(repo, &request.new, path)?,
        ))
    })?;
    Ok(Some(0))
}

fn print_semantic(
    repo: &Repo,
    files: &[String],
    versions: impl Fn(&str) -> Result<(Option<String>, Option<String>)>,
) -> Result<()> {
    let guids = semantic::guid_index(repo);
    let rules = semantic::rules(&Config::load(repo.root()).unwrap_or_default());
    for path in files {
        let (old, new) = versions(path)?;
        println!("{}", ui::bold(path));
        match file_lines(old.as_deref(), new.as_deref(), &guids, &rules) {
            Some(lines) if lines.is_empty() => println!("  (no changes)"),
            Some(lines) => {
                for line in lines {
                    println!("  {line}");
                }
            }
            None => println!("  (cannot be read as Unity YAML; use `gang --raw diff`)"),
        }
    }
    Ok(())
}

/// `gang show [<commit>]` on a terminal.
pub fn show(invocation: &Invocation, repo: &Repo) -> Result<Option<i32>> {
    if invocation.options.raw || !ui::interactive() || invocation.args.len() > 1 {
        return Ok(None);
    }
    let rev = invocation
        .args
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_else(|| "HEAD".to_owned());
    if rev.starts_with('-') || !is_rev(repo, &rev) {
        return Ok(None);
    }
    let changed = split_nul(&repo.run([
        "diff-tree",
        "--no-commit-id",
        "--name-only",
        "-r",
        "-z",
        "--root",
        &rev,
    ])?);
    let (unity, other): (Vec<String>, Vec<String>) = changed
        .into_iter()
        .partition(|p| attributes::is_unity_yaml(p));
    if unity.is_empty() {
        return Ok(None);
    }
    let mut git_args: Vec<OsString> = invocation.git_globals.clone();
    git_args.extend(["--no-pager".into(), "show".into(), rev.clone().into()]);
    if other.is_empty() {
        git_args.push("--no-patch".into());
    } else {
        git_args.push("--".into());
        git_args.extend(other.iter().map(OsString::from));
    }
    git::run_inherited(&git_args)?;
    println!();
    let parent = repo
        .try_run(["rev-parse", "-q", "--verify", &format!("{rev}^")])?
        .map(|p| p.trim().to_owned());
    print_semantic(repo, &unity, |path| {
        let old = match &parent {
            Some(parent) => semantic::blob(repo, &format!("{parent}:{path}"))?,
            None => None,
        };
        Ok((old, semantic::blob(repo, &format!("{rev}:{path}"))?))
    })?;
    Ok(Some(0))
}

/// `gang log --unity [git log options]`: each commit with a summary of the
/// objects it changed.
pub fn log(invocation: &Invocation, repo: &Repo) -> Result<Option<i32>> {
    if !invocation.args.iter().any(|a| a == "--unity") {
        return Ok(None);
    }
    let mut args: Vec<String> = invocation
        .args
        .iter()
        .filter(|a| *a != "--unity")
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if !args.iter().any(|a| {
        a.starts_with("-n") || a.starts_with("--max-count") || a.parse::<i64>().is_ok_and(|n| n < 0)
    }) {
        args.insert(0, "-n20".to_owned());
    }
    let mut log_args = vec!["log".to_owned(), "--format=%H".to_owned()];
    log_args.extend(args);
    let commits = repo.run(&log_args)?;
    let guids = semantic::guid_index(repo);
    let rules = semantic::rules(&Config::load(repo.root()).unwrap_or_default());
    for commit in commits.lines() {
        let header = repo.run(["log", "-1", "--format=%h  %an, %ar  %s", commit])?;
        println!("{}", ui::yellow(header.trim()));
        let parent = repo
            .try_run(["rev-parse", "-q", "--verify", &format!("{commit}^")])?
            .map(|p| p.trim().to_owned());
        let changed = split_nul(&repo.run([
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            "--root",
            commit,
        ])?);
        for path in changed.iter().filter(|p| attributes::is_unity_yaml(p)) {
            let old = match &parent {
                Some(parent) => semantic::blob(repo, &format!("{parent}:{path}"))?,
                None => None,
            };
            let new = semantic::blob(repo, &format!("{commit}:{path}"))?;
            let lines =
                file_lines(old.as_deref(), new.as_deref(), &guids, &rules).unwrap_or_default();
            let name = path.rsplit('/').next().unwrap_or(path);
            println!("  {name}");
            for line in lines.iter().take(12) {
                println!("    {line}");
            }
            if lines.len() > 12 {
                println!("    … {} more", lines.len() - 12);
            }
        }
        println!();
    }
    Ok(Some(0))
}

/// `gang restore --noise [paths]`: drops changes to Unity files that are
/// only noise.
pub fn restore_noise(repo: &Repo, paths: &[String]) -> Result<i32> {
    let rules = semantic::rules(&Config::load(repo.root()).unwrap_or_default());
    let mut args = vec![
        "diff".to_owned(),
        "--name-only".to_owned(),
        "-z".to_owned(),
        "HEAD".to_owned(),
        "--".to_owned(),
    ];
    args.extend(paths.iter().cloned());
    let changed = split_nul(&repo.run(&args)?);
    let noise: Vec<String> = changed
        .into_iter()
        .filter(|p| semantic::is_noise_change(repo, p, &rules).unwrap_or(false))
        .collect();
    if noise.is_empty() {
        println!("No noise-only changes.");
        return Ok(0);
    }
    let mut restore = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
    restore.extend(noise.iter().map(String::as_str));
    repo.run(&restore)?;
    for path in &noise {
        ui::done(format!("restored {path} (noise only)"));
    }
    Ok(0)
}

/// `gang restore <file>::<object>`: undoes the changes to one object.
pub fn restore_object(
    repo: &Repo,
    prefix: &str,
    target: &str,
    source: Option<&str>,
) -> Result<i32> {
    let (file, object) = target
        .split_once("::")
        .context("expected <file>::<object>")?;
    let path = crate::wrap::repo_path(prefix, file);
    let source = source.unwrap_or("HEAD");
    let original_text = semantic::blob(repo, &format!("{source}:{path}"))?
        .with_context(|| format!("{path} is not in {source}"))?;
    let current_text =
        semantic::worktree(repo, &path).with_context(|| format!("could not read {path}"))?;
    let original =
        unity_yaml::parse(&original_text).map_err(|e| anyhow::anyhow!("{path} ({source}): {e}"))?;
    let mut current =
        unity_yaml::parse(&current_text).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
    let count = gang_merge::restore::restore_object(&mut current, &original, object)?;
    if count == 0 {
        println!("{object} in {path} has no changes.");
        return Ok(0);
    }
    std::fs::write(repo.root().join(&path), current.to_text())?;
    let unity_open = gang_core::unity::find_projects(repo.root())
        .iter()
        .any(|p| gang_core::unity::is_editor_open(&p.path(repo.root())));
    ui::done(format!(
        "Restored {} › {object} ({count} change{}). Your other changes in {path} are kept.",
        path.rsplit('/').next().unwrap_or(&path),
        if count == 1 { "" } else { "s" }
    ));
    if unity_open {
        ui::warn(format!(
            "Unity is open: let it reload {} before you keep editing.",
            path.rsplit('/').next().unwrap_or(&path)
        ));
    }
    Ok(0)
}

pub fn reject_object_paths(paths: &[String]) -> Result<()> {
    if paths.iter().filter(|p| p.contains("::")).count() > 1 {
        bail!("restore one object at a time");
    }
    Ok(())
}
