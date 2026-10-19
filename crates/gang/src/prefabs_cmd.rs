//! Rebuilt prefabs (DESIGN.md §13.9): references to a prefab's old object
//! ids are pointed at the new ones, when the prefab is saved and when a
//! teammate's rebuild comes in.

use anyhow::Result;
use gang_core::config::Config;
use gang_core::prefab_ids::{self, Report};
use gang_core::repo::Repo;
use gang_core::{presence, setup, unity};

use crate::ui;

fn short(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn print(report: &Report) {
    for rebuild in &report.rebuilds {
        ui::warn(format!(
            "{} was rebuilt: its objects have new ids ({} found again, {} gone).",
            short(&rebuild.path),
            rebuild.map.len(),
            rebuild.lost.len()
        ));
    }
    for (file, moved, dangling) in &report.files {
        if *moved > 0 {
            println!(
                "    ✓ {file}: {moved} reference{} now point{} at the new objects",
                if *moved == 1 { "" } else { "s" },
                if *moved == 1 { "s" } else { "" }
            );
        }
        if *dangling > 0 {
            println!(
                "    ⚠ {file}: {dangling} reference{} point{} at objects the prefab no longer has; Unity drops {}",
                if *dangling == 1 { "" } else { "s" },
                if *dangling == 1 { "s" } else { "" },
                if *dangling == 1 { "it" } else { "them" }
            );
        }
    }
    for rebuild in &report.rebuilds {
        let lost: Vec<&str> = rebuild
            .lost
            .iter()
            .filter_map(|id| rebuild.names.get(id).map(String::as_str))
            .take(5)
            .collect();
        if !lost.is_empty() {
            println!(
                "      gone from {}: {}",
                short(&rebuild.path),
                lost.join(", ")
            );
        }
    }
}

/// Before `gang save` stages anything. `Some(code)` stops the save.
pub fn before_save(repo: &Repo) -> Result<Option<i32>> {
    if !setup::is_gang_repo(repo) {
        return Ok(None);
    }
    let config = Config::load(repo.root()).unwrap_or_default();
    // Our unpushed commits and the working tree: a rebuild saved earlier
    // and a scene changed now are both ours to fix.
    let base = match presence::upstream(repo, &config)? {
        Some(upstream) => repo
            .try_run(["merge-base", "HEAD", &upstream])?
            .map(|b| b.trim().to_owned()),
        None => None,
    };
    let base = match base {
        Some(base) => base,
        None => match repo.try_run(["rev-parse", "-q", "--verify", "HEAD"])? {
            Some(head) => head.trim().to_owned(),
            None => return Ok(None),
        },
    };
    let report = prefab_ids::repair(repo, &base, None, false)?;
    if report.files.is_empty() {
        return Ok(None);
    }
    let touched = report.touched();
    if !touched.is_empty()
        && unity::find_projects(repo.root())
            .iter()
            .any(|p| unity::is_editor_open(&p.path(repo.root())))
    {
        print(&report);
        eprintln!("✗ Unity is open. GANG cannot fix the references while Unity holds the scenes.");
        ui::hint("Save your work in Unity and close it, then run `gang save` again.");
        return Ok(Some(1));
    }
    let report = prefab_ids::repair(repo, &base, None, true)?;
    print(&report);
    Ok(None)
}

/// After a merge brought in work from `before`..HEAD: our files that still
/// point at a rebuilt prefab's old ids are fixed in a commit of their own.
pub fn after_merge(repo: &Repo, before: &str) -> Result<()> {
    let report = prefab_ids::repair(repo, before, Some("HEAD"), true)?;
    if report.touched().is_empty() {
        if !report.files.is_empty() {
            print(&report);
        }
        return Ok(());
    }
    print(&report);
    let names: Vec<&str> = report.rebuilds.iter().map(|r| short(&r.path)).collect();
    let mut message = format!(
        "gang: point references at the rebuilt {}\n\n",
        names.join(", ")
    );
    for (file, moved, dangling) in &report.files {
        message.push_str(&format!("- {file}: {moved} moved"));
        if *dangling > 0 {
            message.push_str(&format!(", {dangling} with no counterpart"));
        }
        message.push('\n');
    }
    for file in report.touched() {
        repo.run(["add", "--", file])?;
    }
    repo.run(["commit", "-q", "--no-verify", "-m", &message])?;
    ui::done(format!(
        "Committed \"gang: point references at the rebuilt {}\"",
        names.join(", ")
    ));
    Ok(())
}
