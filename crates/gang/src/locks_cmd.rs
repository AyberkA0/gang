//! `gang lock`, `gang unlock`, `gang locks` (DESIGN.md §10).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use anyhow::{Context, Result, bail};
use clap::Parser;
use gang_core::config::{Config, parse_duration};
use gang_core::invocation::Invocation;
use gang_core::lock_check::{self, bake_folder};
use gang_core::lock_levels::{Expander, Resolve, resolve_target};
use gang_core::locks::{self, Derived, Lock, LockKind, Target};
use gang_core::presence::{self, Notice, State};
use gang_core::repo::Repo;
use gang_core::{attributes, clock, setup};

use crate::ui;

/// Lock files or objects so nobody else changes them
#[derive(Parser, Default)]
#[command(name = "gang lock")]
pub struct LockArgs {
    /// Files, folders/, "globs", or objects: Level1.unity::Environment/Player
    targets: Vec<String>,
    /// 0: only the target; 1: + its scripts; 2: + what interacts with it
    #[arg(long)]
    level: Option<u8>,
    /// Why, for the team
    #[arg(short, long)]
    message: Option<String>,
    /// Lock a scene's lighting and NavMesh bake data
    #[arg(long)]
    bake: bool,
    /// Lock what is free when some targets are locked by others
    #[arg(long)]
    partial: bool,
    /// Show what would be locked
    #[arg(long)]
    dry_run: bool,
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    /// Lock your own unpushed changes
    #[arg(long)]
    auto: bool,
    /// With --auto: only these kinds (binary, unity, code, other)
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
    /// With --auto: keep locking files as you change them (while the panel runs)
    #[arg(long)]
    watch: bool,
    /// With --auto: stop --watch
    #[arg(long)]
    off: bool,
    /// Recompute what one of your locks covers
    #[arg(long, value_name = "TARGET|ID")]
    refresh: Option<String>,
    #[arg(long, hide = true)]
    quiet: bool,
}

/// Release locks
#[derive(Parser)]
#[command(name = "gang unlock")]
pub struct UnlockArgs {
    /// What you locked (or a lock id from `gang locks`)
    targets: Vec<String>,
    /// Release a scene's bake lock
    #[arg(long)]
    bake: bool,
    /// Release all your locks
    #[arg(long)]
    all: bool,
    /// Release someone else's lock (the team is told)
    #[arg(long)]
    force: bool,
    /// Release even files with changes that are not pushed yet
    #[arg(long, short)]
    yes: bool,
}

/// Show the team's locks
#[derive(Parser)]
#[command(name = "gang locks")]
pub struct LocksArgs {
    /// Also show what each lock covers
    #[arg(long)]
    tree: bool,
}

fn parse<T: Parser>(name: &str, invocation: &Invocation) -> T {
    T::parse_from(std::iter::once(OsString::from(name)).chain(invocation.args.iter().cloned()))
}

fn context(invocation: &Invocation) -> Result<(Repo, Config, String)> {
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    setup::ensure_installed(&repo)?;
    let config = Config::load(repo.root())?;
    let me = presence::identity(&repo)?.login;
    Ok((repo, config, me))
}

fn prefix(invocation: &Invocation) -> String {
    std::process::Command::new("git")
        .args(&invocation.git_globals)
        .args(["rev-parse", "--show-prefix"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub fn lock(invocation: &Invocation) -> Result<i32> {
    let args: LockArgs = parse("gang lock", invocation);
    let (repo, config, me) = context(invocation)?;
    if args.auto {
        return auto(&repo, &config, &me, &args);
    }
    if let Some(target) = &args.refresh {
        return refresh(&repo, &me, &prefix(invocation), target);
    }
    if args.targets.is_empty() {
        bail!("say what to lock: gang lock <file | folder/ | file::object>");
    }
    let level = args.level.unwrap_or(config.locks.default_level).min(2);
    let mut expander = Expander::new(&repo);
    let prefix = prefix(invocation);

    let mut new_locks = Vec::new();
    for text in &args.targets {
        let root = match resolve_target(&repo, &mut expander, &prefix, text)? {
            Resolve::Found(target) => target,
            Resolve::Ambiguous(options) => pick(text, &options)?,
        };
        let (root, derived, kind, level) = if args.bake {
            let Target::File { path } = &root else {
                bail!("--bake needs a scene (.unity)")
            };
            if !path.ends_with(".unity") {
                bail!("--bake needs a scene (.unity)");
            }
            let (folder, derived) = bake_targets(&mut expander, path);
            (folder, derived, LockKind::Bake, 0)
        } else {
            let derived = expander.expand(&root, level)?;
            (root, derived, LockKind::Manual, level)
        };
        if (!derived.is_empty() || kind == LockKind::Bake) && !args.quiet {
            preview(&root, &derived, level, kind, &mut expander);
        }
        new_locks.push(Lock {
            id: locks::new_id(),
            owner: me.clone(),
            github_id: presence::identity(&repo)?.github_id,
            since: clock::format(clock::now()),
            reason: args.message.clone(),
            kind,
            level,
            root,
            derived,
        });
    }

    let total: usize = new_locks.iter().map(|l| 1 + l.derived.len()).sum();
    if args.dry_run {
        println!("(dry run: nothing locked)");
        return Ok(0);
    }
    if total > config.locks.confirm_over && !args.yes {
        if !ui::interactive() {
            bail!("this would lock {total} targets; add --yes to confirm");
        }
        if !ui::confirm(&format!("Lock all {total} targets?"), true)? {
            return Ok(1);
        }
    }

    let ttl =
        parse_duration(&config.locks.auto_ttl).unwrap_or(std::time::Duration::from_secs(8 * 3600));
    let _ = presence::fetch(&repo);
    let team = presence::team(&repo).unwrap_or_default();
    let partial = args.partial;
    let expander_for_overlap = std::cell::RefCell::new(Expander::new(&repo));
    let subtree_cache =
        std::cell::RefCell::new(BTreeMap::<String, Option<unity_yaml::Hierarchy>>::new());
    let subtree = |file: &str, id: i64| -> BTreeSet<i64> {
        let mut cache = subtree_cache.borrow_mut();
        let h = cache
            .entry(file.to_owned())
            .or_insert_with(|| expander_for_overlap.borrow_mut().hierarchy(file));
        h.as_ref()
            .map(|h| h.subtree(id))
            .unwrap_or_else(|| BTreeSet::from([id]))
    };

    let outcome = locks::update(&repo, |table| {
        let others: Vec<&Lock> = table
            .locks
            .iter()
            .filter(|l| l.owner != me && !locks::expired(l, &team, ttl))
            .collect();
        let mut accepted = Vec::new();
        let mut blockers = Vec::new();
        for lock in &new_locks {
            let mut lock = lock.clone();
            if table
                .locks
                .iter()
                .any(|l| l.owner == me && l.root == lock.root)
            {
                blockers.push(format!("{} is already locked by you", lock.root.label()));
                continue;
            }
            let blocked_by = |target: &Target| {
                others
                    .iter()
                    .find(|o| o.targets().any(|t| locks::overlaps(t, target, &subtree)))
                    .map(|o| o.owner.clone())
            };
            if let Some(owner) = blocked_by(&lock.root) {
                blockers.push(format!("{} is locked by {owner}", lock.root.label()));
                continue;
            }
            let mut kept = Vec::new();
            for derived in lock.derived {
                match blocked_by(&derived.target) {
                    Some(owner) => {
                        blockers.push(format!("{} is locked by {owner}", derived.target.label()))
                    }
                    None => kept.push(derived),
                }
            }
            lock.derived = kept;
            accepted.push(lock);
        }
        if !blockers.is_empty() && !partial {
            bail!("nothing locked:\n  {}", blockers.join("\n  "));
        }
        table.locks.extend(accepted.iter().cloned());
        Ok((accepted, blockers))
    });
    let (accepted, blockers) = match outcome {
        Ok(result) => result,
        Err(err) => {
            eprintln!("✗ {err:#}");
            if !partial {
                ui::hint("Use --partial to lock what is free.");
            }
            return Ok(1);
        }
    };
    for lock in &accepted {
        let extra = if lock.derived.is_empty() {
            match &lock.root {
                Target::File { .. } => " (+ .meta)".to_owned(),
                _ => String::new(),
            }
        } else {
            format!(
                " (level {}, {} targets)",
                lock.level,
                1 + lock.derived.len()
            )
        };
        let what = if lock.kind == LockKind::Bake {
            format!(
                "bake data for {}.unity",
                lock.root.label().trim_end_matches('/')
            )
        } else {
            lock.root.label()
        };
        println!("🔒 Locked {what}{extra}");
    }
    for blocker in blockers {
        ui::warn(format!("not locked: {blocker}"));
    }
    if let Some(repo) = Some(&repo) {
        crate::presence_cmd::spawn_publish(repo);
    }
    Ok(0)
}

fn pick(text: &str, options: &[String]) -> Result<Target> {
    if !ui::interactive() {
        bail!(
            "{} assets match {text:?}: {}",
            options.len(),
            options.join(", ")
        );
    }
    println!("Several assets match {text:?}:");
    for (i, option) in options.iter().enumerate() {
        println!("  [{}] {option}", i + 1);
    }
    print!("Which one? ");
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let index: usize = line.trim().parse().context("not a number")?;
    let path = options
        .get(index.wrapping_sub(1))
        .context("no such option")?;
    Ok(Target::File { path: path.clone() })
}

/// A scene's bake outputs and lighting documents.
fn bake_targets(expander: &mut Expander<'_>, scene: &str) -> (Target, Vec<Derived>) {
    let folder = Target::Folder {
        path: bake_folder(scene),
    };
    let mut derived = Vec::new();
    if let Some(file) = expander.file(scene).cloned() {
        for doc in &file.docs {
            let (Some(id), Some(class)) = (doc.file_id, doc.class_id) else {
                continue;
            };
            if matches!(class, 29 | 104 | 157 | 196) {
                let name = doc.type_name().unwrap_or("Settings").to_owned();
                if class == 157 {
                    let settings = doc
                        .body()
                        .and_then(|b| b.get("m_LightingSettings"))
                        .and_then(unity_yaml::Value::as_ref)
                        .and_then(|r| r.guid)
                        .and_then(|g| expander.guids().path(&g).map(str::to_owned));
                    if let Some(path) = settings {
                        derived.push(Derived {
                            target: Target::File { path },
                            why: None,
                        });
                    }
                }
                derived.push(Derived {
                    target: Target::Document {
                        file: scene.to_owned(),
                        file_id: id,
                        class: name,
                    },
                    why: None,
                });
            }
        }
    }
    (folder, derived)
}

fn preview(
    root: &Target,
    derived: &[Derived],
    level: u8,
    kind: LockKind,
    expander: &mut Expander<'_>,
) {
    if kind == LockKind::Bake {
        let folder = root.label();
        println!("Bake lock for {}.unity:", folder.trim_end_matches('/'));
        println!("    {folder:<46} lightmaps, LightingData, NavMesh");
        for d in derived {
            println!("    {}", d.target.label());
        }
        println!("  The rest of the scene stays editable.");
        return;
    }
    println!("Lock preview — level {level}");
    println!();
    if let Target::Object { file, file_id, .. } = root {
        let descendants = expander
            .hierarchy(file)
            .map(|h| h.subtree(*file_id).len().saturating_sub(1))
            .unwrap_or(0);
        println!("  Object");
        let more = if descendants > 0 {
            format!(
                "+ {descendants} descendant{}",
                if descendants == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        println!("    {:<46} {more}", root.label());
    }
    let section = |title: &str, items: Vec<(String, String)>| {
        if !items.is_empty() {
            println!("  {title}");
            for (label, why) in items {
                println!("    {label:<46} {why}");
            }
        }
    };
    let direct = |d: &&Derived| d.why.is_none();
    let scripts: Vec<(String, String)> = derived
        .iter()
        .filter(direct)
        .filter(|d| matches!(&d.target, Target::File { path } if path.ends_with(".cs")))
        .map(|d| (d.target.label(), String::new()))
        .collect();
    let prefabs: Vec<(String, String)> = derived
        .iter()
        .filter(direct)
        .filter(|d| matches!(&d.target, Target::File { path } if !path.ends_with(".cs")))
        .map(|d| (d.target.label(), String::new()))
        .collect();
    let interacting = group_objects(derived.iter().filter(|d| d.why.is_some()).collect());
    let mut later_scripts: Vec<(String, String)> = Vec::new();
    // Scripts listed after an interacting object belong to it.
    let mut after = false;
    for d in derived {
        if d.why.is_some() {
            after = true;
        } else if after {
            if let Target::File { path } = &d.target {
                if path.ends_with(".cs") {
                    later_scripts.push((d.target.label(), String::new()));
                }
            }
        }
    }
    let first_scripts: Vec<(String, String)> = scripts
        .into_iter()
        .filter(|s| !later_scripts.contains(s))
        .collect();
    section("Scripts on it and its descendants", first_scripts);
    section("Nested prefabs", prefabs);
    section("Interacting objects", interacting);
    section("Scripts on interacting objects", later_scripts);
    let objects = derived
        .iter()
        .filter(|d| matches!(d.target, Target::Object { .. }))
        .count()
        + usize::from(matches!(root, Target::Object { .. }));
    let prefab_count = derived
        .iter()
        .filter(|d| matches!(&d.target, Target::File { path } if path.ends_with(".prefab")))
        .count();
    let script_count = derived
        .iter()
        .filter(|d| matches!(&d.target, Target::File { path } if path.ends_with(".cs")))
        .count();
    println!();
    println!(
        "  Total: {objects} object{}, {prefab_count} prefab{}, {script_count} script{}",
        s(objects),
        s(prefab_count),
        s(script_count)
    );
}

fn s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Interacting objects with the same reason under the same parent are shown
/// as one line: `Level1.unity › Enemies/* (12 objects)`.
fn group_objects(items: Vec<&Derived>) -> Vec<(String, String)> {
    let mut groups: Vec<(String, String, usize)> = Vec::new();
    for d in items {
        let why = d.why.clone().unwrap_or_default();
        let (label, parent) = match &d.target {
            Target::Object { file, path, .. } => {
                let parent = path.rsplit_once('/').map(|(p, _)| p.to_owned());
                (
                    d.target.label(),
                    parent.map(|p| format!("{} › {p}/*", file.rsplit('/').next().unwrap_or(file))),
                )
            }
            other => (other.label(), None),
        };
        match parent.and_then(|p| {
            groups
                .iter_mut()
                .find(|(l, w, _)| *w == why && (l.starts_with(&p) || *l == p))
                .map(|g| (g, p))
        }) {
            Some((group, p)) => {
                group.0 = p;
                group.2 += 1;
            }
            None => groups.push((label, why, 1)),
        }
    }
    groups
        .into_iter()
        .map(|(label, why, n)| {
            if n > 1 {
                (format!("{label} ({n} objects)"), why)
            } else {
                (label, why)
            }
        })
        .collect()
}

/// What kind of file, for `locks.auto_allowed`.
fn kind_of(path: &str) -> &'static str {
    if attributes::is_binary_path(path) {
        "binary"
    } else if attributes::is_unity_yaml(path) {
        "unity"
    } else if [
        ".cs", ".shader", ".hlsl", ".cginc", ".compute", ".asmdef", ".asmref",
    ]
    .iter()
    .any(|e| path.ends_with(e))
    {
        "code"
    } else {
        "other"
    }
}

fn auto(repo: &Repo, config: &Config, me: &str, args: &LockArgs) -> Result<i32> {
    if args.watch || args.off {
        let path = repo.gang_dir().join("local.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines: Vec<String> = text
            .lines()
            .filter(|l| !l.starts_with("auto_lock_watch"))
            .map(str::to_owned)
            .collect();
        lines.push(format!("auto_lock_watch = {}", args.watch));
        std::fs::create_dir_all(repo.gang_dir())?;
        std::fs::write(&path, lines.join("\n") + "\n")?;
        if args.watch {
            ui::done("Auto-locking files as you change them while `gang` (the panel) is open.");
        } else {
            ui::done("Stopped auto-locking.");
            return Ok(0);
        }
    }
    let quiet = args.quiet;
    let upstream = presence::upstream(repo, config)?;
    let flags = presence::compute_flags(repo, upstream.as_deref())?;
    let _ = presence::fetch(repo);
    let team = presence::team(repo).unwrap_or_default();
    let others_changing: BTreeMap<&str, &str> = team
        .iter()
        .filter(|p| p.user != me)
        .flat_map(|p| {
            p.files
                .iter()
                .filter(|(_, f)| f.state != State::Stashed)
                .map(move |(f, _)| (f.as_str(), p.user.as_str()))
        })
        .collect();
    let allowed: Vec<String> = config
        .locks
        .auto_allowed
        .iter()
        .filter(|k| args.only.is_empty() || args.only.contains(k))
        .cloned()
        .collect();
    let _ = locks::fetch(repo);
    let table = locks::read(repo)?;

    let mut rows = Vec::new();
    let mut to_lock = Vec::new();
    for (path, state) in &flags {
        if *state == State::Stashed || path.ends_with(".meta") {
            continue;
        }
        let kind = kind_of(path);
        let note = if !allowed.iter().any(|k| k == kind) {
            Some(if kind == "code" {
                "skipped — code merges automatically".to_owned()
            } else {
                format!("skipped — {kind} files are not auto-locked (locks.auto_allowed)")
            })
        } else if let Some(user) = others_changing.get(path.as_str()) {
            Some(format!("skipped — {user} is also changing it"))
        } else {
            table
                .locks
                .iter()
                .find(|l| l.targets().any(|t| t.covers_file(path)))
                .map(|lock| {
                    if lock.owner == me {
                        "already locked by you".to_owned()
                    } else {
                        format!("locked by {}", lock.owner)
                    }
                })
        };
        match note {
            Some(note) => rows.push((false, path.clone(), note)),
            None => {
                rows.push((true, path.clone(), state.label().to_owned()));
                to_lock.push(path.clone());
            }
        }
    }
    if !quiet {
        println!("Unpushed changes in {} file{}:", rows.len(), s(rows.len()));
        for (locked, path, note) in &rows {
            println!("  {} {path:<44} {note}", if *locked { "🔒" } else { "· " });
        }
    }
    if args.dry_run || to_lock.is_empty() {
        if !quiet && to_lock.is_empty() {
            println!("Nothing new to lock.");
        }
        return Ok(0);
    }
    let github_id = presence::identity(repo)?.github_id;
    let count = locks::update(repo, |table| {
        let mut added = 0;
        for path in &to_lock {
            if table
                .locks
                .iter()
                .any(|l| l.targets().any(|t| t.covers_file(path)))
            {
                continue;
            }
            table.locks.push(Lock {
                id: locks::new_id(),
                owner: me.to_owned(),
                github_id,
                since: clock::format(clock::now()),
                reason: None,
                kind: LockKind::Auto,
                level: 0,
                root: Target::File { path: path.clone() },
                derived: Vec::new(),
            });
            added += 1;
        }
        Ok(added)
    })?;
    if !quiet {
        println!();
        println!(
            "Locked {count} file{} (auto, level 0). They will be released when you push.",
            s(count)
        );
    }
    Ok(0)
}

/// For the panel's `--watch` mode: auto-lock new changes, release pushed
/// ones, without output.
pub fn watch_tick(repo: &Repo) -> Result<()> {
    let enabled = std::fs::read_to_string(repo.gang_dir().join("local.toml"))
        .map(|t| {
            t.lines()
                .any(|l| l.replace(' ', "") == "auto_lock_watch=true")
        })
        .unwrap_or(false);
    if !enabled {
        return Ok(());
    }
    let config = Config::load(repo.root())?;
    let me = presence::identity(repo)?.login;
    let args = LockArgs {
        auto: true,
        quiet: true,
        ..LockArgs::default()
    };
    auto(repo, &config, &me, &args)?;
    release_auto(repo)?;
    Ok(())
}

/// Releases auto locks whose files no longer differ from upstream
/// (pushed or reverted).
pub fn release_auto(repo: &Repo) -> Result<Vec<String>> {
    let config = Config::load(repo.root()).unwrap_or_default();
    let me = presence::identity(repo)?.login;
    let _ = locks::fetch(repo);
    let mine: Vec<Lock> = locks::read(repo)?
        .locks
        .into_iter()
        .filter(|l| l.owner == me && l.kind == LockKind::Auto)
        .collect();
    if mine.is_empty() {
        return Ok(Vec::new());
    }
    let upstream = presence::upstream(repo, &config)?;
    let flags = presence::compute_flags(repo, upstream.as_deref())?;
    let done: Vec<String> = mine
        .iter()
        .filter(|l| match &l.root {
            Target::File { path } => !flags.contains_key(path),
            _ => false,
        })
        .map(|l| l.id.clone())
        .collect();
    if done.is_empty() {
        return Ok(Vec::new());
    }
    let released = locks::update(repo, |table| {
        let labels: Vec<String> = table
            .locks
            .iter()
            .filter(|l| done.contains(&l.id))
            .map(|l| l.root.label())
            .collect();
        table.locks.retain(|l| !done.contains(&l.id));
        Ok(labels)
    })?;
    Ok(released)
}

/// After a push: release auto locks, and offer to release manual locks on
/// what was pushed.
pub fn after_push(repo: &Repo, pushed: &[String]) -> Result<()> {
    let released = release_auto(repo).unwrap_or_default();
    if !released.is_empty() {
        let names: Vec<&str> = released
            .iter()
            .map(|p| p.rsplit('/').next().unwrap_or(p))
            .collect();
        println!(
            "🔓 Released {} auto lock{} ({})",
            released.len(),
            s(released.len()),
            names.join(", ")
        );
    }
    let me = presence::identity(repo)?.login;
    let mine: Vec<Lock> = locks::read(repo)?
        .locks
        .into_iter()
        .filter(|l| l.owner == me && l.kind != LockKind::Auto)
        .collect();
    for lock in mine {
        let touched = pushed.iter().any(|p| {
            lock.targets()
                .any(|t| t.covers_file(p) || t.unity_file() == Some(p.as_str()))
        });
        if !touched || !ui::interactive() {
            continue;
        }
        let label = lock.root.label();
        let extra = if lock.derived.is_empty() {
            String::new()
        } else {
            format!(" (level {})", lock.level)
        };
        println!();
        println!("You pushed changes to locked targets: {label}{extra}");
        if ui::confirm("Release your lock?", true)? {
            let id = lock.id.clone();
            locks::update(repo, |table| {
                table.locks.retain(|l| l.id != id);
                Ok(())
            })?;
            let derived = if lock.derived.is_empty() {
                String::new()
            } else {
                format!(" and {} derived targets", lock.derived.len())
            };
            println!("🔓 Released {label}{derived}");
        }
    }
    Ok(())
}

fn refresh(repo: &Repo, me: &str, prefix: &str, text: &str) -> Result<i32> {
    let _ = locks::fetch(repo);
    let table = locks::read(repo)?;
    let mut expander = Expander::new(repo);
    let target = match resolve_target(repo, &mut expander, prefix, text) {
        Ok(Resolve::Found(t)) => Some(t),
        _ => None,
    };
    let lock = table
        .locks
        .iter()
        .find(|l| l.owner == me && (l.id == text || Some(&l.root) == target.as_ref()))
        .cloned()
        .with_context(|| format!("you have no lock on {text}"))?;
    let derived = expander.expand(&lock.root, lock.level)?;
    let count = derived.len();
    locks::update(repo, |table| {
        if let Some(l) = table.locks.iter_mut().find(|l| l.id == lock.id) {
            l.derived = derived.clone();
        }
        Ok(())
    })?;
    ui::done(format!(
        "{} now covers {} target{}",
        lock.root.label(),
        count + 1,
        s(count + 1)
    ));
    Ok(0)
}

pub fn unlock(invocation: &Invocation) -> Result<i32> {
    let args: UnlockArgs = parse("gang unlock", invocation);
    let (repo, config, me) = context(invocation)?;
    let prefix = prefix(invocation);
    let mut expander = Expander::new(&repo);
    let mut wanted: Vec<(String, Option<Target>)> = Vec::new();
    for text in &args.targets {
        let target = match resolve_target(&repo, &mut expander, &prefix, text) {
            Ok(Resolve::Found(Target::File { path })) if args.bake => Some(Target::Folder {
                path: bake_folder(&path),
            }),
            Ok(Resolve::Found(t)) => Some(t),
            _ => None,
        };
        wanted.push((text.clone(), target));
    }
    if wanted.is_empty() && !args.all {
        bail!("say what to unlock, or use --all");
    }
    // A binary file's lock matters most while its changes are not on the
    // server yet: released now, someone else's edit cannot be merged (A7).
    if !args.yes {
        let upstream = presence::upstream(&repo, &config)?;
        let flags = presence::compute_flags(&repo, upstream.as_deref())?;
        let at_risk: Vec<&String> = flags
            .keys()
            .filter(|path| attributes::is_binary_path(path))
            .filter(|path| {
                wanted.iter().any(|(_, t)| t.as_ref().is_some_and(|t| t.covers_file(path)))
            })
            .collect();
        if !at_risk.is_empty() {
            println!("These locked files have changes that are not pushed yet:");
            for path in &at_risk {
                println!("    {path}");
            }
            println!("Once unlocked, a teammate's edit to them cannot be merged with yours.");
            let release = ui::interactive() && ui::confirm("Release the lock anyway?", false)?;
            if !release {
                ui::hint("Push first (gang sync), or release anyway with --yes.");
                return Ok(1);
            }
        }
    }
    let force = args.force;
    let all = args.all;
    let me_clone = me.clone();
    let removed = locks::update(&repo, move |table| {
        let mut removed: Vec<Lock> = Vec::new();
        let mut missing = Vec::new();
        if all {
            removed.extend(table.locks.iter().filter(|l| l.owner == me_clone).cloned());
        }
        for (text, target) in &wanted {
            let matches: Vec<Lock> = table
                .locks
                .iter()
                .filter(|l| l.id == *text || target.as_ref().is_some_and(|t| l.root == *t))
                .cloned()
                .collect();
            let (mine, theirs): (Vec<Lock>, Vec<Lock>) =
                matches.into_iter().partition(|l| l.owner == me_clone);
            if !mine.is_empty() {
                removed.extend(mine);
            } else if !theirs.is_empty() {
                if !force {
                    bail!(
                        "{text} is locked by {}; use --force to release it (the team is told)",
                        theirs[0].owner
                    );
                }
                removed.extend(theirs);
            } else {
                missing.push(text.clone());
            }
        }
        if removed.is_empty() {
            bail!("no lock found for {}", missing.join(", "));
        }
        let ids: BTreeSet<String> = removed.iter().map(|l| l.id.clone()).collect();
        table.locks.retain(|l| !ids.contains(&l.id));
        Ok(removed)
    });
    let removed = match removed {
        Ok(removed) => removed,
        Err(err) => {
            eprintln!("✗ {err:#}");
            return Ok(1);
        }
    };
    for lock in &removed {
        let derived = if lock.derived.is_empty() {
            String::new()
        } else {
            format!(" and {} derived targets", lock.derived.len())
        };
        let whose = if lock.owner == me {
            String::new()
        } else {
            format!(" (held by {})", lock.owner)
        };
        println!("🔓 Released {}{derived}{whose}", lock.root.label());
        if lock.owner != me {
            presence::add_notice(
                &repo,
                Notice {
                    ts: clock::format(clock::now()),
                    kind: "force_unlock".into(),
                    target: lock.root.label(),
                    from: Some(lock.owner.clone()),
                },
            )?;
        }
    }
    crate::presence_cmd::spawn_publish(&repo);
    Ok(0)
}

pub fn list(invocation: &Invocation) -> Result<i32> {
    let args: LocksArgs = parse("gang locks", invocation);
    let (repo, config, _me) = context(invocation)?;
    let _ = locks::fetch(&repo);
    let _ = presence::fetch(&repo);
    let table = locks::read(&repo)?;
    if invocation.options.json {
        println!("{}", serde_json::to_string_pretty(&table)?);
        return Ok(0);
    }
    if table.locks.is_empty() {
        println!("No locks.");
        return Ok(0);
    }
    let ttl =
        parse_duration(&config.locks.auto_ttl).unwrap_or(std::time::Duration::from_secs(8 * 3600));
    let team = presence::team(&repo).unwrap_or_default();
    let now = clock::now();
    println!(
        "{:<44} {:<10} {:<10} {:<7} {:<8} REASON",
        "TARGET", "OWNER", "SINCE", "TYPE", "STATUS"
    );
    for lock in &table.locks {
        let since = clock::parse(&lock.since)
            .map(|t| clock::ago(now - t))
            .unwrap_or_default();
        let kind = match lock.kind {
            LockKind::Manual if lock.derived.is_empty() => "manual".to_owned(),
            LockKind::Manual => format!("L{}", lock.level),
            LockKind::Auto => "auto".to_owned(),
            LockKind::Bake => "bake".to_owned(),
        };
        let status = if locks::expired(lock, &team, ttl) {
            let away = team
                .iter()
                .find(|p| p.user == lock.owner)
                .map(|p| format!(" ({} offline {})", lock.owner, clock::ago(p.age(now))))
                .unwrap_or_default();
            format!("expired{away}")
        } else {
            "active".to_owned()
        };
        println!(
            "{:<44} {:<10} {:<10} {:<7} {:<8} {}",
            lock.root.label(),
            lock.owner,
            since,
            kind,
            status,
            lock.reason.clone().unwrap_or_default()
        );
        if args.tree {
            for derived in &lock.derived {
                let why = derived
                    .why
                    .as_deref()
                    .map(|w| format!("  ({w})"))
                    .unwrap_or_default();
                println!("    └─ {}{why}", derived.target.label());
            }
        } else if !lock.derived.is_empty() {
            println!(
                "    + {} derived target{} (gang locks --tree)",
                lock.derived.len(),
                s(lock.derived.len())
            );
        }
    }
    Ok(0)
}

/// Lock problems in the given changes, for hooks and status.
pub fn check_changes(
    repo: &Repo,
    fetch: bool,
    changes: &[(String, Option<String>, Option<String>)],
) -> Result<Vec<lock_check::Blocked>> {
    let checker = lock_check::Checker::load(repo, fetch)?;
    if checker.is_empty() {
        return Ok(Vec::new());
    }
    let mut blocked = Vec::new();
    for (path, old, new) in changes {
        blocked.extend(checker.check(path, old.as_deref(), new.as_deref()));
    }
    Ok(blocked)
}
