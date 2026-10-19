//! Wrapped git commands: git runs as usual, with GANG's checks around it
//! (DESIGN.md §8.3, PLAN.md P4).

use std::collections::BTreeSet;
use std::ffi::OsString;

use anyhow::Result;
use gang_core::commands::Layer;
use gang_core::config::Config;
use gang_core::invocation::Invocation;
use gang_core::presence::{self, Notice};
use gang_core::repo::Repo;
use gang_core::{clock, git, guards, safety, setup, unity};

use crate::ui;

/// Runs a wrapped git command.
pub fn run(invocation: &Invocation, name: &str, layers: &[Layer]) -> Result<i32> {
    git::ensure_supported()?;
    let repo = Repo::discover(&invocation.git_globals)?;
    if let Some(repo) = &repo {
        setup::ensure_installed(repo)?;
        if layers.contains(&Layer::Editor) && changes_files(name, &invocation.args) {
            if let Some(code) = editor_guard(invocation, repo)? {
                return Ok(code);
            }
        }
    }
    match (name, repo) {
        ("add", Some(repo)) => add(invocation, &repo, &invocation.args),
        ("rm", Some(repo)) => rm(invocation, &repo),
        ("mv", Some(repo)) => mv(invocation, &repo),
        ("restore", Some(repo)) => restore(invocation, &repo, false),
        ("checkout", Some(repo)) => restore(invocation, &repo, true),
        ("reset", Some(repo)) => reset(invocation, &repo),
        ("clean", Some(repo)) => clean(invocation, &repo),
        ("push", _) => push(invocation),
        ("status", repo) => crate::presence_cmd::status(invocation, repo.as_ref()),
        ("diff", Some(repo)) => match crate::semantic_cmd::diff(invocation, &repo)? {
            Some(code) => Ok(code),
            None => git_command(invocation, &invocation.args),
        },
        ("show", Some(repo)) => match crate::semantic_cmd::show(invocation, &repo)? {
            Some(code) => Ok(code),
            None => git_command(invocation, &invocation.args),
        },
        ("log", Some(repo)) => match crate::semantic_cmd::log(invocation, &repo)? {
            Some(code) => Ok(code),
            None => git_command(invocation, &invocation.args),
        },
        ("fetch", Some(repo)) => {
            let code = git_command(invocation, &invocation.args)?;
            if code == 0 && setup::is_gang_repo(&repo) {
                let _ = gang_core::presence::fetch(&repo);
                let _ = gang_core::locks::fetch(&repo);
            }
            Ok(code)
        }
        ("filter-branch" | "filter-repo", repo) => rewrite_history(invocation, repo.as_ref()),
        ("merge" | "rebase" | "cherry-pick" | "revert" | "stash" | "pull", Some(repo)) => {
            let aborting = invocation
                .args
                .iter()
                .any(|a| a == "--abort" || a == "--quit" || a == "--skip");
            if !aborting && setup::is_gang_repo(&repo) {
                if let Some(side) = one_side_for_all(&strings(&invocation.args)) {
                    eprintln!(
                        "✗ GANG asks every conflict on its own; `-X {side}` would settle them all for one side."
                    );
                    ui::hint(format!(
                        "Run it without `-X {side}` and answer the questions with `gang bang`. (Plain git still does it: `gang git {name} -X {side} …`.)"
                    ));
                    return Ok(1);
                }
                let auto_remap = Config::load(repo.root())
                    .map(|c| c.layers.auto_remap)
                    .unwrap_or(true);
                if auto_remap {
                    layer_pass(invocation, &repo, name)?;
                }
            }
            let before = repo
                .try_run(["rev-parse", "-q", "--verify", "HEAD"])?
                .map(|h| h.trim().to_owned());
            let code = git_command(invocation, &invocation.args)?;
            if aborting {
                gang_core::store::clear_all(&repo)?;
                return Ok(code);
            }
            // A clean merge gets the same reference checks as `gang sync`
            // (rebuilt prefabs, shared GUIDs, deleted assets in use).
            if code == 0
                && matches!(name, "merge" | "pull" | "rebase")
                && setup::is_gang_repo(&repo)
            {
                let after = repo
                    .try_run(["rev-parse", "-q", "--verify", "HEAD"])?
                    .map(|h| h.trim().to_owned());
                if let Some(before) = before.filter(|b| after.as_ref() != Some(b)) {
                    if let Err(err) = crate::prefabs_cmd::after_merge(&repo, &before) {
                        ui::warn(format!(
                            "could not check references after the merge: {err:#}"
                        ));
                    }
                }
            }
            if name == "pull" && code == 0 && setup::is_gang_repo(&repo) {
                let _ = gang_core::presence::fetch(&repo);
                let _ = gang_core::locks::fetch(&repo);
            }
            offer_bang(&repo, code)
        }
        _ => git_command(invocation, &invocation.args),
    }
}

/// The layer pass (DESIGN.md §14.2) before a command that brings in other
/// commits: `merge`, `pull` (merge or rebase), `rebase` and `cherry-pick`.
/// Without it a layer clash only reaches `gang bang` as a question about a
/// name, and the other side's objects silently change layer (L3).
fn layer_pass(invocation: &Invocation, repo: &Repo, name: &str) -> Result<()> {
    let args = strings(&invocation.args);
    let resuming = ["--continue", "--edit-todo", "--show-current-patch"];
    if has(&args, &resuming) {
        return Ok(());
    }
    match name {
        "merge" => {
            let revs = positionals(
                &args,
                &[
                    "-m",
                    "-F",
                    "-s",
                    "-X",
                    "--strategy",
                    "--strategy-option",
                    "--file",
                ],
            );
            if let [rev] = revs.as_slice() {
                if !has(&args, &["--no-commit", "--squash"]) {
                    crate::sync_cmd::pre_merge(repo, rev, false, invocation.options.ignore_editor)?;
                }
            }
        }
        "rebase" => {
            // `--onto` and `--root` replay onto something else than the
            // upstream, and a second positional switches branches first.
            if has(&args, &["--onto", "--root"]) || args.iter().any(|a| a.starts_with("--onto=")) {
                return Ok(());
            }
            let revs = positionals(
                &args,
                &[
                    "-s",
                    "-X",
                    "--strategy",
                    "--strategy-option",
                    "-x",
                    "--exec",
                ],
            );
            let upstream = match revs.as_slice() {
                [rev] => rev.clone(),
                [] => match configured_upstream(repo)? {
                    Some(rev) => rev,
                    None => return Ok(()),
                },
                _ => return Ok(()),
            };
            crate::sync_cmd::pre_merge(repo, &upstream, true, invocation.options.ignore_editor)?;
        }
        "pull" => {
            if has(&args, &["--ff-only"]) {
                return Ok(());
            }
            let Some(upstream) = fetch_for_pull(invocation, repo, &args)? else {
                return Ok(());
            };
            crate::sync_cmd::pre_merge(
                repo,
                &upstream,
                pull_rebases(repo, &args)?,
                invocation.options.ignore_editor,
            )?;
        }
        "cherry-pick" => {
            if has(&args, &["-n", "--no-commit"]) {
                return Ok(());
            }
            let commits = positionals(
                &args,
                &[
                    "-m",
                    "--mainline",
                    "-s",
                    "-X",
                    "--strategy",
                    "--strategy-option",
                ],
            );
            for commit in commits {
                // A range picks up to its last commit.
                let tip = commit.rsplit("..").next().unwrap_or(&commit).to_owned();
                if !tip.is_empty() {
                    crate::sync_cmd::pre_merge(
                        repo,
                        &tip,
                        false,
                        invocation.options.ignore_editor,
                    )?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The current branch's upstream (`origin/main`), if it has one.
fn configured_upstream(repo: &Repo) -> Result<Option<String>> {
    Ok(repo
        .try_run(["rev-parse", "--abbrev-ref", "@{upstream}"])?
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty()))
}

/// Fetches what `git pull <args>` would merge, and names it: the branch's
/// upstream for a plain pull, `FETCH_HEAD` otherwise.
fn fetch_for_pull(invocation: &Invocation, repo: &Repo, args: &[String]) -> Result<Option<String>> {
    let refs = positionals(
        args,
        &[
            "-s",
            "-X",
            "--strategy",
            "--strategy-option",
            "-j",
            "--jobs",
            "--depth",
            "--upload-pack",
        ],
    );
    let mut fetch: Vec<OsString> = invocation.git_globals.clone();
    fetch.extend(["fetch".into(), "-q".into()]);
    fetch.extend(refs.iter().map(OsString::from));
    let output = std::process::Command::new("git")
        .args(&fetch)
        .current_dir(repo.root())
        .output()?;
    if !output.status.success() {
        // Let the pull itself report the problem.
        return Ok(None);
    }
    if refs.is_empty() {
        if let Some(upstream) = configured_upstream(repo)? {
            return Ok(Some(upstream));
        }
    }
    Ok(repo
        .try_run(["rev-parse", "-q", "--verify", "FETCH_HEAD^{commit}"])?
        .map(|_| "FETCH_HEAD".to_owned()))
}

/// Whether `git pull <args>` rebases: `--rebase`, or the configuration.
fn pull_rebases(repo: &Repo, args: &[String]) -> Result<bool> {
    let from_value = |value: &str| !matches!(value, "false" | "no" | "off" | "0");
    for arg in args.iter().rev() {
        match arg.as_str() {
            "-r" | "--rebase" => return Ok(true),
            "--no-rebase" => return Ok(false),
            _ => {
                if let Some(value) = arg.strip_prefix("--rebase=") {
                    return Ok(from_value(value));
                }
            }
        }
    }
    let branch = repo
        .try_run(["symbolic-ref", "--short", "-q", "HEAD"])?
        .map(|b| b.trim().to_owned())
        .unwrap_or_default();
    for key in [format!("branch.{branch}.rebase"), "pull.rebase".to_owned()] {
        if let Some(value) = repo.config_get(&key)? {
            return Ok(from_value(value.trim()));
        }
    }
    Ok(false)
}

/// After a command that merges: offer `gang bang` when conflicts are left.
pub fn offer_bang(repo: &Repo, code: i32) -> Result<i32> {
    if code == 0 {
        return Ok(0);
    }
    let open = crate::bang::open_questions(repo)?;
    if open == 0 {
        return Ok(code);
    }
    let plural = if open == 1 { "" } else { "s" };
    if !ui::interactive() {
        eprintln!("{open} conflict{plural} found. Run `gang bang` to resolve them.");
        return Ok(code);
    }
    println!();
    if ui::confirm(
        &format!("{open} conflict{plural} found. Resolve them now?"),
        true,
    )? {
        return crate::bang::resolve(repo, &crate::bang::Args::default());
    }
    println!("Run `gang bang` when you are ready.");
    Ok(code)
}

/// Runs the invocation's command with `args` instead of its own arguments.
fn git_command(invocation: &Invocation, args: &[OsString]) -> Result<i32> {
    let mut all = invocation.git_globals.clone();
    all.extend(invocation.command.clone());
    all.extend(args.iter().cloned());
    Ok(git::run_inherited(&all)?)
}

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// `-X ours` / `-X theirs` (git settles every text conflict for one side):
/// the side it names.
fn one_side_for_all(args: &[String]) -> Option<&str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let value = if arg == "-X" || arg == "--strategy-option" {
            iter.next().map(String::as_str)
        } else {
            arg.strip_prefix("--strategy-option=")
                .or_else(|| arg.strip_prefix("-X"))
        };
        if let Some(side @ ("ours" | "theirs")) = value {
            return Some(side);
        }
    }
    None
}

fn has(args: &[String], options: &[&str]) -> bool {
    args.iter().any(|a| options.contains(&a.as_str()))
}

/// Positional arguments, skipping options and the values of `value_options`.
fn positionals(args: &[String], value_options: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    let mut iter = args.iter();
    let mut options_done = false;
    while let Some(arg) = iter.next() {
        if options_done || !arg.starts_with('-') || arg == "-" {
            result.push(arg.clone());
        } else if arg == "--" {
            options_done = true;
        } else if value_options.contains(&arg.as_str()) {
            iter.next();
        }
    }
    result
}

/// Whether the command can rewrite files in the working tree.
fn changes_files(name: &str, args: &[OsString]) -> bool {
    let args = strings(args);
    let first = positionals(&args, &[]).into_iter().next();
    match name {
        "switch" => {
            !(has(&args, &["-c", "-C", "--create", "--force-create"])
                && positionals(&args, &["-c", "-C", "--create", "--force-create"]).is_empty())
        }
        "checkout" => !(has(&args, &["-b", "-B"]) && positionals(&args, &["-b", "-B"]).is_empty()),
        "stash" => !matches!(first.as_deref(), Some("list" | "show" | "create" | "store")),
        "bisect" => !matches!(
            first.as_deref(),
            Some("log" | "visualize" | "view" | "terms" | "help")
        ),
        "reset" => has(&args, &["--hard", "--merge", "--keep"]),
        // Restoring one object only warns (DESIGN.md §18.3).
        "restore" if args.iter().any(|a| a.contains("::")) => false,
        "restore" => !has(&args, &["--staged", "-S"]) || has(&args, &["--worktree", "-W"]),
        _ => true,
    }
}

/// Stops a command while Unity has a project open. `Some(exit code)` when
/// the command must not run.
fn editor_guard(invocation: &Invocation, repo: &Repo) -> Result<Option<i32>> {
    let open: Vec<String> = unity::find_projects(repo.root())
        .into_iter()
        .filter(|p| unity::is_editor_open(&p.path(repo.root())))
        .map(|p| {
            if p.dir.is_empty() {
                ".".to_owned()
            } else {
                p.dir
            }
        })
        .collect();
    if open.is_empty() {
        return Ok(None);
    }
    let name = invocation.command_name().unwrap_or("this command");
    let warn_only = invocation.options.ignore_editor
        || Config::load(repo.root())
            .map(|c| c.sync.editor_open == "warn")
            .unwrap_or(false);
    if warn_only {
        ui::warn(format!(
            "Unity is open ({}). Let it reload changed scenes before you save in Unity.",
            open.join(", ")
        ));
        return Ok(None);
    }
    eprintln!(
        "✗ Unity is open ({}), and `gang {name}` can change scenes and prefabs on disk.",
        open.join(", ")
    );
    ui::hint("Saving in Unity afterwards would overwrite them with the old version.");
    ui::hint("Save your work and close Unity, then run it again.");
    ui::hint(format!(
        "(Use `gang --ignore-editor {name} …` to run it anyway.)"
    ));
    Ok(Some(1))
}

/// The current directory relative to the repository root (`Assets/Scenes/`).
fn prefix(invocation: &Invocation) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(&invocation.git_globals)
        .args(["rev-parse", "--show-prefix"])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `path` as typed in the current directory → path from the repository root.
pub fn repo_path(prefix: &str, path: &str) -> String {
    let joined = format!("{prefix}{}", path.replace('\\', "/"));
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn staged_paths(repo: &Repo) -> Result<BTreeSet<String>> {
    Ok(guards::staged_changes(repo)?
        .into_iter()
        .map(|c| format!("{}{}", c.status, c.path))
        .collect())
}

/// `gang add`: git add, then the `.meta` partners of what was added, without
/// OS junk.
pub fn add(invocation: &Invocation, repo: &Repo, args: &[OsString]) -> Result<i32> {
    let before = staged_paths(repo)?;
    let mut all = invocation.git_globals.clone();
    all.push("add".into());
    all.extend(args.iter().cloned());
    let code = git::run_inherited(&all)?;
    if code != 0 {
        return Ok(code);
    }
    complete_pairs(repo, &before)?;
    Ok(0)
}

/// Stages missing `.meta` partners and unstages junk after an add.
fn complete_pairs(repo: &Repo, before: &BTreeSet<String>) -> Result<()> {
    let projects = unity::find_projects(repo.root());
    let index: BTreeSet<String> = repo.tracked_files()?.into_iter().collect();
    let changes = guards::staged_changes(repo)?;

    let junk: Vec<&str> = changes
        .iter()
        .filter(|c| c.adds() && guards::is_junk(&c.path))
        .map(|c| c.path.as_str())
        .collect();
    if !junk.is_empty() {
        let mut args = vec!["rm", "-q", "--cached", "--"];
        args.extend(&junk);
        repo.run(&args)?;
        for path in &junk {
            ui::warn(format!("left out {path} (OS junk file)"));
        }
    }

    let mut partners: BTreeSet<String> = BTreeSet::new();
    for change in changes.iter().filter(|c| !guards::is_junk(&c.path)) {
        if let Some(partner) = guards::meta_partner(&projects, &change.path) {
            let on_disk = repo.root().join(&partner);
            if on_disk.is_file() || (!on_disk.exists() && index.contains(&partner)) {
                partners.insert(partner);
            }
        }
        if change.adds() {
            for meta in guards::folder_metas(&projects, &change.path) {
                if repo.root().join(&meta).is_file() {
                    partners.insert(meta);
                }
            }
        }
    }
    if partners.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "--"];
    args.extend(partners.iter().map(String::as_str));
    if let Err(err) = repo.run(&args) {
        ui::warn(format!("could not add .meta partners: {err:#}"));
        return Ok(());
    }
    let after = staged_paths(repo)?;
    for entry in after.difference(before) {
        let path = &entry[1..];
        if partners.contains(path) {
            ui::done(format!("also added {path}"));
        }
    }
    Ok(())
}

fn deletions(repo: &Repo) -> Result<BTreeSet<String>> {
    Ok(guards::staged_changes(repo)?
        .into_iter()
        .filter(|c| c.deletes())
        .map(|c| c.path)
        .collect())
}

/// `gang rm`: git rm, then the `.meta` files of what was removed.
fn rm(invocation: &Invocation, repo: &Repo) -> Result<i32> {
    let before = deletions(repo)?;
    let code = git_command(invocation, &invocation.args)?;
    if code != 0 {
        return Ok(code);
    }
    let removed: Vec<String> = deletions(repo)?.difference(&before).cloned().collect();
    let projects = unity::find_projects(repo.root());
    let mut index: BTreeSet<String> = repo.tracked_files()?.into_iter().collect();

    let mut metas: BTreeSet<String> = BTreeSet::new();
    for path in removed.iter().filter(|p| !p.ends_with(".meta")) {
        if let Some(meta) = guards::meta_partner(&projects, path) {
            if index.remove(&meta) {
                metas.insert(meta);
            }
        }
    }
    // Folders left empty lose their .meta too, innermost first.
    for path in &removed {
        for folder_meta in guards::folder_metas(&projects, path) {
            let folder = folder_meta.trim_end_matches(".meta");
            let prefix = format!("{folder}/");
            let has_children = index
                .range(prefix.clone()..)
                .next()
                .is_some_and(|p| p.starts_with(&prefix));
            if !has_children && index.remove(&folder_meta) {
                metas.insert(folder_meta);
            }
        }
    }
    if metas.is_empty() {
        return Ok(0);
    }
    let args = strings(&invocation.args);
    let mut rm_args = vec!["rm", "-q"];
    if has(&args, &["--cached"]) {
        rm_args.push("--cached");
    }
    if has(&args, &["-f", "--force"]) {
        rm_args.push("-f");
    }
    rm_args.push("--");
    rm_args.extend(metas.iter().map(String::as_str));
    repo.run(&rm_args)?;
    for meta in metas {
        ui::done(format!("also removed {meta}"));
    }
    Ok(0)
}

/// `gang mv`: git mv, moving `.meta` files along, and case-only renames that
/// work on case-insensitive file systems.
fn mv(invocation: &Invocation, repo: &Repo) -> Result<i32> {
    let args = strings(&invocation.args);
    let paths = positionals(&args, &[]);
    if paths.len() < 2 || has(&args, &["-n", "--dry-run"]) {
        return git_command(invocation, &invocation.args);
    }
    let prefix = prefix(invocation)?;
    let (sources, destination) = paths.split_at(paths.len() - 1);
    let destination = repo_path(&prefix, &destination[0]);
    let sources: Vec<String> = sources.iter().map(|s| repo_path(&prefix, s)).collect();
    let into_dir = repo.root().join(&destination).is_dir()
        && !(sources.len() == 1 && sources[0].eq_ignore_ascii_case(&destination));
    let index: BTreeSet<String> = repo.tracked_files()?.into_iter().collect();
    let projects = unity::find_projects(repo.root());
    let force: Vec<&str> = if has(&args, &["-f", "--force"]) {
        vec!["-f"]
    } else {
        vec![]
    };

    let mut moves: Vec<(String, String)> = Vec::new();
    for source in &sources {
        let target = if into_dir {
            let name = source.rsplit('/').next().unwrap_or(source);
            format!("{destination}/{name}")
        } else {
            destination.clone()
        };
        moves.push((source.clone(), target.clone()));
        let meta = format!("{source}.meta");
        let is_asset =
            !source.ends_with(".meta") && guards::meta_partner(&projects, source).is_some();
        if is_asset && index.contains(&meta) && !sources.contains(&meta) {
            moves.push((meta, format!("{target}.meta")));
        }
        // A scene's lighting and NavMesh data live in a folder named after
        // it; Unity only finds them there (field test C4).
        if let (Some(old_stem), Some(new_stem)) =
            (source.strip_suffix(".unity"), target.strip_suffix(".unity"))
        {
            let folder_meta = format!("{old_stem}.meta");
            let tracked_inside = index.iter().any(|p| p.starts_with(&format!("{old_stem}/")));
            if old_stem != new_stem
                && tracked_inside
                && !sources.iter().any(|s| s == old_stem)
                && !repo.root().join(new_stem).exists()
            {
                moves.push((old_stem.to_owned(), new_stem.to_owned()));
                if index.contains(&folder_meta) {
                    moves.push((folder_meta, format!("{new_stem}.meta")));
                }
            }
        }
    }

    // Folders that do not exist yet are created, with their .meta, as
    // Unity would (field test C6).
    for (from, to) in moves.clone() {
        if from.ends_with(".meta") {
            continue;
        }
        let Some(project) = projects.iter().find(|p| to.starts_with(&p.join("Assets/"))) else {
            continue;
        };
        let assets = project.join("Assets");
        let mut missing = Vec::new();
        let mut dir = to.rsplit_once('/').map(|(d, _)| d.to_owned());
        while let Some(current) = dir {
            if current.len() <= assets.len() || repo.root().join(&current).is_dir() {
                break;
            }
            missing.push(current.clone());
            dir = current.rsplit_once('/').map(|(d, _)| d.to_owned());
        }
        for folder in missing.into_iter().rev() {
            std::fs::create_dir_all(repo.root().join(&folder))?;
            let meta = format!("{folder}.meta");
            std::fs::write(
                repo.root().join(&meta),
                format!(
                    "fileFormatVersion: 2
guid: {}
folderAsset: yes
DefaultImporter:
  externalObjects: {{}}
  userData: 
  assetBundleName: 
  assetBundleVariant: 
",
                    new_guid()
                ),
            )?;
            repo.run(["add", "--", &meta])?;
            ui::done(format!("created {folder} (with its .meta)"));
        }
    }

    for (from, to) in &moves {
        let temp = format!("{to}.gang-rename");
        let mut command = vec!["mv"];
        command.extend(&force);
        if from != to && from.eq_ignore_ascii_case(to) {
            // A case-only rename: go through a temporary name.
            let mut first = command.clone();
            first.extend(["--", from.as_str(), temp.as_str()]);
            repo.run(&first)?;
            command.extend(["--", temp.as_str(), to.as_str()]);
        } else {
            command.extend(["--", from.as_str(), to.as_str()]);
        }
        let output = repo.output(&command)?;
        if !output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            return Ok(output.status.code().unwrap_or(1));
        }
        if has(&args, &["-v", "--verbose"]) || from.ends_with(".meta") && !sources.contains(from) {
            ui::done(format!("{from} → {to}"));
        }
        if from.ends_with(".unity") && file_name(from) != file_name(to) {
            for place in crate::doctor_cmd::scene_name_references(repo, from) {
                ui::warn(format!("{place} still loads the scene by its old name"));
            }
        }
        if from.ends_with(".unity") && from != to {
            for settings in build_settings_with(repo, &projects, from, to)? {
                ui::done(format!("{settings}: the scene list now points at {to}"));
            }
        }
    }
    Ok(0)
}

/// Points Build Settings' scene list at a moved scene; returns the files
/// changed (staged).
fn build_settings_with(
    repo: &Repo,
    projects: &[unity::UnityProject],
    from: &str,
    to: &str,
) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for project in projects {
        let path = project.join("ProjectSettings/EditorBuildSettings.asset");
        let Ok(text) = std::fs::read_to_string(repo.root().join(&path)) else {
            continue;
        };
        let scene_from = from
            .strip_prefix(&format!("{}/", project.dir))
            .unwrap_or(from);
        let needle = format!(
            "path: {scene_from}
"
        );
        if !text.contains(&needle) {
            continue;
        }
        let scene_to = to.strip_prefix(&format!("{}/", project.dir)).unwrap_or(to);
        let updated = text.replace(
            &needle,
            &format!(
                "path: {scene_to}
"
            ),
        );
        std::fs::write(repo.root().join(&path), updated)?;
        repo.run(["add", "--", &path])?;
        changed.push(path);
    }
    Ok(changed)
}

/// A new random GUID for a `.meta` file.
fn new_guid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for round in 0..2u64 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(round);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        hasher.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `gang restore` / `gang checkout -- <paths>`: restores `.meta` files with
/// their assets, and keeps a backup of the working tree.
fn restore(invocation: &Invocation, repo: &Repo, checkout: bool) -> Result<i32> {
    let args = strings(&invocation.args);
    // `--ours` / `--theirs` during a merge would settle every conflict in
    // the named files at once; GANG asks them one by one (gang bang).
    if has(&args, &["--ours", "--theirs", "-2", "-3"]) {
        let unmerged = crate::bang::unmerged(repo)?;
        if !unmerged.is_empty() {
            let open = crate::bang::open_questions(repo).unwrap_or(0);
            eprintln!(
                "✗ GANG asks every conflict on its own: {open} {} open. Answer {} with `gang bang`.",
                if open == 1 {
                    "question is"
                } else {
                    "questions are"
                },
                if open == 1 { "it" } else { "them one by one" }
            );
            ui::hint(
                "(Plain git still takes one side for whole files: `gang git checkout --theirs -- <file>`.)",
            );
            return Ok(1);
        }
    }
    if !checkout {
        let paths = positionals(&args, &["-s", "--source", "--pathspec-from-file"]);
        if has(&args, &["--noise"]) {
            let prefix = prefix(invocation)?;
            let paths: Vec<String> = paths.iter().map(|p| repo_path(&prefix, p)).collect();
            return crate::semantic_cmd::restore_noise(repo, &paths);
        }
        if let Some(target) = paths.iter().find(|p| p.contains("::")) {
            crate::semantic_cmd::reject_object_paths(&paths)?;
            let source = args
                .iter()
                .position(|a| a == "-s" || a == "--source")
                .and_then(|i| args.get(i + 1).cloned())
                .or_else(|| {
                    args.iter()
                        .find_map(|a| a.strip_prefix("--source=").map(str::to_owned))
                });
            let op = safety::begin(repo, "restore", true)?;
            let code = crate::semantic_cmd::restore_object(
                repo,
                &prefix(invocation)?,
                target,
                source.as_deref(),
            )?;
            safety::finish(repo, op)?;
            return Ok(code);
        }
    }
    let paths: Vec<String> = if checkout {
        match args.iter().position(|a| a == "--") {
            Some(pos) => args[pos + 1..].to_vec(),
            None => return git_command(invocation, &invocation.args),
        }
    } else {
        positionals(&args, &["-s", "--source", "--pathspec-from-file"])
    };
    let prefix = prefix(invocation)?;
    let index: BTreeSet<String> = repo.tracked_files()?.into_iter().collect();
    let projects = unity::find_projects(repo.root());

    let mut extra: Vec<OsString> = Vec::new();
    for path in &paths {
        let full = repo_path(&prefix, path);
        if full.ends_with(".meta") {
            continue;
        }
        if let Some(meta) = guards::meta_partner(&projects, &full) {
            let listed = paths.iter().any(|p| repo_path(&prefix, p) == meta);
            if index.contains(&meta) && !listed {
                extra.push(format!(":(top){meta}").into());
            }
        }
    }
    let mut all_args = invocation.args.clone();
    if !extra.is_empty() {
        if !args.iter().any(|a| a == "--") {
            // Paths after options; add ours after an explicit separator.
            all_args.retain(|a| !paths.iter().any(|p| a.to_string_lossy() == *p));
            all_args.push("--".into());
            all_args.extend(paths.iter().map(OsString::from));
        }
        all_args.extend(extra.iter().cloned());
    }

    let touches_worktree = changes_files("restore", &invocation.args) || checkout;
    let op = if touches_worktree {
        Some(safety::begin(
            repo,
            if checkout { "checkout" } else { "restore" },
            true,
        )?)
    } else {
        None
    };
    let code = git_command(invocation, &all_args)?;
    if let Some(op) = op {
        if code == 0 {
            safety::finish(repo, op)?;
        }
    }
    Ok(code)
}

/// `gang reset`: asks before `--hard` and keeps a backup.
fn reset(invocation: &Invocation, repo: &Repo) -> Result<i32> {
    let args = strings(&invocation.args);
    if !has(&args, &["--hard", "--merge", "--keep"]) {
        return git_command(invocation, &invocation.args);
    }
    let changed = repo.run(["status", "--porcelain", "--untracked-files=no"])?;
    let count = changed.lines().count();
    if count > 0 && ui::interactive() {
        let question = format!(
            "Discard changes to {count} file{}? A backup is kept: `gang undo` brings them back.",
            if count == 1 { "" } else { "s" }
        );
        if !ui::confirm(&question, true)? {
            return Ok(1);
        }
    }
    let op = safety::begin(repo, "reset", true)?;
    let code = git_command(invocation, &invocation.args)?;
    if code == 0 {
        safety::finish(repo, op)?;
    }
    Ok(code)
}

/// `gang clean`: never deletes Unity's `Library/` unless asked, and shows
/// what will go first.
fn clean(invocation: &Invocation, repo: &Repo) -> Result<i32> {
    let mut args: Vec<OsString> = invocation.args.clone();
    let include_library = args.iter().any(|a| a == "--include-library");
    args.retain(|a| a != "--include-library");
    if !include_library {
        args.extend(["-e".into(), "[Ll]ibrary/".into()]);
    }
    let text = strings(&args);
    let dry_run = has(&text, &["-n", "--dry-run", "-i", "--interactive"]);
    let forced = text
        .iter()
        .any(|a| a == "--force" || (a.starts_with('-') && !a.starts_with("--") && a.contains('f')));
    if forced && !dry_run && ui::interactive() {
        let mut preview = vec!["clean".to_owned(), "-n".to_owned()];
        preview.extend(text.iter().cloned());
        let listing = repo.run(&preview)?;
        if listing.trim().is_empty() {
            println!("Nothing to clean.");
            return Ok(0);
        }
        print!("{listing}");
        if !ui::confirm("Delete these?", false)? {
            return Ok(1);
        }
    }
    let op = if forced && !dry_run {
        Some(safety::begin(repo, "clean", true)?)
    } else {
        None
    };
    let code = git_command(invocation, &args)?;
    if let Some(op) = op {
        if code == 0 {
            safety::finish(repo, op)?;
        }
    }
    Ok(code)
}

/// `gang push`: `--force` becomes `--force-with-lease`, and force-pushing
/// the default branch asks first.
fn push(invocation: &Invocation) -> Result<i32> {
    let mut forced = false;
    let has_lease = invocation
        .args
        .iter()
        .any(|a| a.to_string_lossy().starts_with("--force-with-lease"));
    let args: Vec<OsString> = invocation
        .args
        .iter()
        .map(|arg| {
            let text = arg.to_string_lossy();
            if (text == "--force" || text == "-f") && !has_lease {
                forced = true;
                OsString::from("--force-with-lease")
            } else {
                if text.starts_with("--force") || text == "-f" {
                    forced = true;
                }
                arg.clone()
            }
        })
        .collect();
    if forced && ui::interactive() {
        let text = strings(&args);
        let refs = positionals(
            &text,
            &["-o", "--push-option", "--repo", "--receive-pack", "--exec"],
        );
        let targets_default = refs.iter().skip(1).any(|r| {
            let dest = r.rsplit(':').next().unwrap_or(r);
            matches!(
                dest.trim_start_matches('+')
                    .trim_start_matches("refs/heads/"),
                "main" | "master"
            )
        }) || (refs.len() <= 1 && current_branch_is_default());
        if targets_default
            && !ui::confirm(
                "Force-push the default branch? Teammates' work on it can be lost.",
                false,
            )?
        {
            return Ok(1);
        }
    }
    let repo = Repo::discover(&invocation.git_globals)?;
    let pushed: Vec<String> = repo
        .as_ref()
        .and_then(|r| {
            r.try_run(["diff", "--name-only", "-z", "@{upstream}", "HEAD"])
                .ok()
                .flatten()
        })
        .map(|names| gang_core::repo::split_nul(&names))
        .unwrap_or_default();
    let code = git_command(invocation, &args)?;
    if code == 0 {
        if let Some(repo) = repo.filter(setup::is_gang_repo) {
            if let Err(err) = crate::locks_cmd::after_push(&repo, &pushed) {
                ui::warn(format!("could not update locks: {err:#}"));
            }
        }
    }
    Ok(code)
}

fn current_branch_is_default() -> bool {
    std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .map(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "main" | "master"))
        .unwrap_or(false)
}

/// `gang filter-branch` / `gang filter-repo`: history rewrites ask twice,
/// and a successful one is announced to the team (DESIGN.md §8.5).
fn rewrite_history(invocation: &Invocation, repo: Option<&Repo>) -> Result<i32> {
    if ui::interactive() {
        println!("This rewrites history. Everyone on the team will have to re-clone or reset.");
        if !ui::confirm("Rewrite history?", false)? || !ui::confirm("Really?", false)? {
            return Ok(1);
        }
    }
    let code = git_command(invocation, &invocation.args)?;
    if let Some(repo) = repo.filter(|r| code == 0 && setup::is_gang_repo(r)) {
        let branch = repo
            .try_run(["symbolic-ref", "--short", "-q", "HEAD"])?
            .map(|b| b.trim().to_owned())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "HEAD".to_owned());
        presence::add_notice(
            repo,
            Notice {
                ts: clock::format(clock::now()),
                kind: "history_rewrite".into(),
                target: branch,
                from: None,
            },
        )?;
        crate::presence_cmd::spawn_publish(repo);
        println!("The team has been told that history was rewritten.");
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn which_commands_change_files() {
        assert!(!changes_files("switch", &os(&["-c", "topic"])));
        assert!(changes_files(
            "switch",
            &os(&["-c", "topic", "origin/main"])
        ));
        assert!(changes_files("switch", &os(&["main"])));
        assert!(!changes_files("checkout", &os(&["-b", "topic"])));
        assert!(changes_files("checkout", &os(&["main"])));
        assert!(!changes_files("stash", &os(&["list"])));
        assert!(changes_files("stash", &os(&[])));
        assert!(changes_files("stash", &os(&["pop"])));
        assert!(!changes_files("reset", &os(&["HEAD~1"])));
        assert!(changes_files("reset", &os(&["--hard"])));
        assert!(!changes_files("restore", &os(&["--staged", "a.txt"])));
        assert!(changes_files("restore", &os(&["a.txt"])));
    }

    #[test]
    fn repo_paths() {
        assert_eq!(repo_path("", "Assets/a.png"), "Assets/a.png");
        assert_eq!(repo_path("Assets/Art/", "a.png"), "Assets/Art/a.png");
        assert_eq!(repo_path("Assets/Art/", "../b.png"), "Assets/b.png");
        assert_eq!(
            repo_path("Assets/", "./Scenes\\x.unity"),
            "Assets/Scenes/x.unity"
        );
    }
}
