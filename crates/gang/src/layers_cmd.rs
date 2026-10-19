//! Reserved numbers in Unity's indexed tables (DESIGN.md §14.6): `gang
//! layers`, the check `gang save` runs before staging, and the notes in
//! `gang status` and the pre-commit hook.

use std::collections::BTreeMap;
use std::ffi::OsString;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gang_core::claims::{self, Check, Status, Table};
use gang_core::config::Config;
use gang_core::invocation::Invocation;
use gang_core::layers;
use gang_core::repo::Repo;
use gang_core::unity::{self, UnityProject};
use gang_core::{clock, presence, setup};

use crate::ui;

/// Layers, rendering layers and NavMesh areas: who reserved which number
#[derive(Parser)]
#[command(name = "gang layers")]
pub struct LayersArgs {
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Move your new layers away from numbers a teammate reserved first,
    /// with your objects and scripts (Unity must be closed)
    Fix,
    /// Give up your reservation of a name
    Release { name: String },
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = LayersArgs::parse_from(
        std::iter::once(OsString::from("gang layers")).chain(invocation.args.iter().cloned()),
    );
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    let config = Config::load(repo.root())?;
    match args.action {
        None => list(&repo, &config, invocation.options.json),
        Some(Action::Fix) => {
            let checks = sync_or_local(&repo, &config)?;
            let clashes: Vec<&Check> = checks.iter().filter(|c| c.is_clash()).collect();
            if clashes.is_empty() {
                ui::done("Nothing to move: none of your new names clash.");
                return Ok(0);
            }
            Ok(fix(&repo, &config, &clashes, "gang layers fix")?.unwrap_or(0))
        }
        Some(Action::Release { name }) => {
            let released = claims::release(&repo, &name)?;
            if released == 0 {
                println!("You have no reservation for {name:?}.");
            } else {
                ui::done(format!("Released {name:?}."));
            }
            Ok(0)
        }
    }
}

/// The shared claims brought up to date, or, offline, the last fetched.
fn sync_or_local(repo: &Repo, config: &Config) -> Result<Vec<Check>> {
    match claims::sync(repo, config) {
        Ok(synced) => {
            for entry in &synced.reserved {
                ui::done(format!(
                    "{} {} {:?} is reserved for you until it reaches the team's main branch",
                    capitalize(entry.table.label()),
                    entry.index,
                    entry.name
                ));
            }
            Ok(synced.checks)
        }
        Err(err) => {
            ui::warn(format!("could not reach the team's reservations: {err:#}"));
            claims::local(repo, config)
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Before `gang save` stages anything: reserves new names and moves the
/// ones a teammate reserved first. `Some(code)` stops the save.
pub fn before_save(repo: &Repo) -> Result<Option<i32>> {
    if !setup::is_gang_repo(repo) {
        return Ok(None);
    }
    let config = Config::load(repo.root()).unwrap_or_default();
    let checks = sync_or_local(repo, &config)?;
    let clashes: Vec<&Check> = checks.iter().filter(|c| c.is_clash()).collect();
    if clashes.is_empty() {
        return Ok(None);
    }
    fix(repo, &config, &clashes, "gang save")
}

/// Moves physics layers that clash; anything else is for the person to do
/// in Unity. `Some(code)` when the caller must stop.
fn fix(repo: &Repo, config: &Config, clashes: &[&Check], again: &str) -> Result<Option<i32>> {
    for check in clashes {
        if let Some(text) = check.describe() {
            eprintln!("⚠ {text}");
        }
    }
    let manual: Vec<&&Check> = clashes
        .iter()
        .filter(|c| {
            c.entry.table != Table::Layers || !matches!(c.status, Status::Clash { to: Some(_), .. })
        })
        .collect();
    if !manual.is_empty() {
        for check in &manual {
            let where_ = match check.entry.table {
                Table::Layers | Table::RenderingLayers => "Project Settings › Tags and Layers",
                Table::NavMeshAreas => "the Navigation window's Areas tab",
            };
            let to = match check.status {
                Status::Clash { to: Some(to), .. } => format!(" (the first free one is {to})"),
                _ => String::new(),
            };
            eprintln!(
                "✗ In Unity, give your {} {:?} another number in {where_}{to}, and move what uses it.",
                check.entry.table.label(),
                check.entry.name
            );
        }
        ui::hint(format!(
            "Then run `{again}` again. Nothing was saved; your changes are where they were."
        ));
        return Ok(Some(1));
    }

    let projects: BTreeMap<String, UnityProject> = unity::find_projects(repo.root())
        .into_iter()
        .map(|p| (p.dir.clone(), p))
        .collect();
    let open: Vec<&UnityProject> = clashes
        .iter()
        .filter_map(|c| projects.get(&c.entry.project))
        .filter(|p| unity::is_editor_open(&p.path(repo.root())))
        .collect();
    if !open.is_empty() {
        eprintln!(
            "✗ Unity is open. GANG cannot move the layer while Unity holds the project settings."
        );
        ui::hint(format!(
            "Save your work in Unity and close it, then run `{again}` again: GANG moves your layer with your objects and scripts."
        ));
        ui::hint(
            "Meanwhile, avoid using the layer's number in code; LayerMask.NameToLayer(\"…\") never needs to move.",
        );
        return Ok(Some(1));
    }

    let upstream =
        claims::default_branch(repo, config)?.context("no default branch to compare with")?;
    let base = repo
        .try_run(["merge-base", "HEAD", &upstream])?
        .map(|b| b.trim().to_owned())
        .unwrap_or_else(|| upstream.clone());
    let mut per_project: BTreeMap<String, Vec<layers::Move>> = BTreeMap::new();
    for check in clashes {
        let Status::Clash {
            holder,
            to: Some(to),
        } = &check.status
        else {
            continue;
        };
        per_project
            .entry(check.entry.project.clone())
            .or_default()
            .push(layers::Move {
                name: check.entry.name.clone(),
                from: check.entry.index,
                to: *to,
                taken_by: format!(
                    "Layer {} is reserved for {:?} by {}",
                    check.entry.index, holder.name, holder.owner
                ),
                follow: false,
            });
    }
    let mut report = layers::Report::default();
    for (dir, moves) in &per_project {
        let Some(project) = projects.get(dir) else {
            continue;
        };
        let done = layers::apply_to_worktree(repo, &base, project, moves)?;
        report.moves.extend(done.moves);
        report.updated.extend(done.updated);
        report.review.extend(done.review);
    }
    for m in &report.moves {
        ui::done(format!(
            "Moved your layer {:?} from {} to {}",
            m.name, m.from, m.to
        ));
    }
    for line in &report.updated {
        println!("    ✓ {line}");
    }
    if report.review.is_empty() {
        println!("  Nothing needs review.");
    } else {
        println!("  Please review:");
        for line in &report.review {
            println!("    ⚠ {line}");
        }
    }
    // Reserve the new numbers and release the old ones.
    if let Err(err) = claims::sync(repo, config) {
        ui::warn(format!("could not update the reservations: {err:#}"));
    }
    Ok(None)
}

/// Blocking lines for the pre-commit hook, from the reservations as last
/// fetched (no network in a hook).
pub fn pre_commit_problems(repo: &Repo) -> Vec<String> {
    let config = Config::load(repo.root()).unwrap_or_default();
    let Ok(checks) = claims::local(repo, &config) else {
        return Vec::new();
    };
    checks.iter().filter_map(Check::describe).collect()
}

/// Lines for `gang status`.
pub fn status_notes(repo: &Repo) -> Vec<String> {
    let config = Config::load(repo.root()).unwrap_or_default();
    let Ok(checks) = claims::local(repo, &config) else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    for check in &checks {
        match &check.status {
            Status::Clash { .. } => {
                if let Some(text) = check.describe() {
                    notes.push(format!("✗ {text}"));
                    notes.push("  `gang save` moves it for you (close Unity first).".to_owned());
                }
            }
            Status::Unclaimed => notes.push(format!(
                "⚠ Your new {} {} {:?} is not reserved yet; it will be at your next save.",
                check.entry.table.label(),
                check.entry.index,
                check.entry.name
            )),
            _ => {}
        }
    }
    notes
}

/// Runs in the background publisher: keeps this clone's reservations up to
/// date without anyone typing a command.
pub fn background(repo: &Repo, config: &Config) {
    let _ = claims::sync(repo, config);
}

fn list(repo: &Repo, config: &Config, json: bool) -> Result<i32> {
    let checks = sync_or_local(repo, config)?;
    let table = claims::read(repo)?;
    let upstream = claims::default_branch(repo, config)?;
    let me = presence::identity(repo)?.login;
    if json {
        let rows: Vec<serde_json::Value> = table
            .claims
            .iter()
            .map(|c| {
                serde_json::json!({
                    "table": c.table,
                    "project": c.project,
                    "index": c.index,
                    "name": c.name,
                    "owner": c.owner,
                    "since": c.since,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    let now = clock::now();
    let mut any = false;
    for project in unity::find_projects(repo.root()) {
        for kind in claims::TABLES {
            let path = project.join(kind.file());
            let Some(names) =
                gang_core::semantic::worktree(repo, &path).and_then(|t| kind.names(&t))
            else {
                continue;
            };
            let pushed = upstream
                .as_ref()
                .and_then(|u| {
                    gang_core::semantic::blob(repo, &format!("{u}:{path}"))
                        .ok()
                        .flatten()
                })
                .and_then(|t| kind.names(&t))
                .unwrap_or_default();
            let mut rows = Vec::new();
            for i in kind.first_user()..kind.size() {
                let name = names.get(i).cloned().unwrap_or_default();
                let on_main = pushed.get(i).cloned().unwrap_or_default();
                let claim = table
                    .claims
                    .iter()
                    .find(|c| c.table == kind && c.project == project.dir && c.index as usize == i);
                let check = checks.iter().find(|c| {
                    c.entry.table == kind
                        && c.entry.project == project.dir
                        && c.entry.index as usize == i
                });
                let state = match (check.map(|c| &c.status), claim) {
                    (Some(Status::Clash { holder, .. }), _) => format!(
                        "✗ yours ({name:?}), but reserved for {:?} by {}",
                        holder.name, holder.owner
                    ),
                    (Some(Status::Mine), _) => {
                        "yours, reserved, not on the main branch yet".to_owned()
                    }
                    (Some(Status::Shared { with }), _) => format!("shared with {with}"),
                    (Some(Status::Unclaimed), _) => "yours, not reserved yet".to_owned(),
                    (None, Some(c)) if c.owner != me => {
                        let ago = clock::parse(&c.since)
                            .map(|t| format!(", {}", clock::ago(now - t)))
                            .unwrap_or_default();
                        format!("reserved for {:?} by {}{ago}", c.name, c.owner)
                    }
                    _ if !on_main.is_empty() => "on the main branch".to_owned(),
                    _ => continue,
                };
                let shown = if name.is_empty() {
                    claim.map(|c| c.name.clone()).unwrap_or_default()
                } else {
                    name
                };
                rows.push(format!("  {i:>2}  {shown:<20} {state}"));
            }
            if rows.is_empty() {
                continue;
            }
            any = true;
            let title = if project.dir.is_empty() {
                capitalize(kind.label())
            } else {
                format!("{} ({})", capitalize(kind.label()), project.dir)
            };
            println!("{title}s:");
            for row in rows {
                println!("{row}");
            }
        }
    }
    if !any {
        println!("No layers, rendering layers or NavMesh areas of your own yet.");
    }
    Ok(0)
}
