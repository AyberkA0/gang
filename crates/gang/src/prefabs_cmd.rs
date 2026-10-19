//! Rebuilt prefabs (DESIGN.md §13.9): references to a prefab's old object
//! ids are pointed at the new ones, when the prefab is saved and when a
//! teammate's rebuild comes in. After a merge, also the other references a
//! merge can leave pointing at nothing: shared GUIDs (§14.5) and assets one
//! side deleted while the other used them (R29).

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

/// After a merge brought in work from `before`..HEAD: references our side
/// made that the merge left pointing at nothing are fixed, each kind in a
/// commit of its own.
pub fn after_merge(repo: &Repo, before: &str) -> Result<()> {
    rebuilt_prefabs(repo, before)?;
    shared_guids(repo, before)?;
    deleted_in_use(repo, before)
}

/// One side deleted an asset the other side's work uses (R29): git merged
/// the two without a conflict. Each one is asked on its own.
fn deleted_in_use(repo: &Repo, before: &str) -> Result<()> {
    let orphans = gang_core::orphans::after_merge(repo, before)?;
    let mut brought: Vec<(&gang_core::orphans::Orphan, Vec<String>)> = Vec::new();
    for orphan in &orphans {
        let commit = orphan
            .deleted_in
            .as_ref()
            .map(|(hash, _)| format!(" ({hash})"))
            .unwrap_or_default();
        let author = orphan.deleted_in.as_ref().map(|(_, a)| a.as_str());
        if orphan.ours {
            ui::warn(format!(
                "You deleted {}{commit}, but the incoming work uses it:",
                orphan.asset
            ));
        } else {
            ui::warn(format!(
                "{} deleted {}{commit}, but your work uses it:",
                author.unwrap_or("The other side"),
                orphan.asset
            ));
        }
        for user in &orphan.users {
            println!("    {user}");
        }
        if ui::interactive()
            && ui::confirm(
                &format!(
                    "  Bring {} back? (No: Unity shows it as Missing there)",
                    orphan.name()
                ),
                true,
            )?
        {
            let paths = gang_core::orphans::bring_back(repo, orphan)?;
            brought.push((orphan, paths));
        } else {
            ui::hint(format!(
                "Unity shows it as Missing there. To bring it back: gang git checkout {} -- \"{}\" \"{}.meta\"",
                &orphan.from[..orphan.from.len().min(10)],
                orphan.asset,
                orphan.asset
            ));
        }
    }
    if brought.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = brought.iter().map(|(o, _)| o.name()).collect();
    let mut message = format!("gang: bring back {}, still in use\n\n", names.join(", "));
    for (orphan, paths) in &brought {
        message.push_str(&format!(
            "- {} (deleted{}), used by {}\n",
            orphan.asset,
            orphan
                .deleted_in
                .as_ref()
                .map(|(hash, author)| format!(" in {hash} by {author}"))
                .unwrap_or_default(),
            orphan.users.join(", ")
        ));
        for path in paths
            .iter()
            .filter(|p| **p != orphan.asset && **p != format!("{}.meta", orphan.asset))
        {
            message.push_str(&format!("  with {path}\n"));
        }
    }
    repo.run(["commit", "-q", "--no-verify", "-m", &message])?;
    ui::done(format!(
        "Committed \"gang: bring back {}, still in use\"",
        names.join(", ")
    ));
    Ok(())
}

/// Both added the same asset; the merge kept the shared GUID.
fn shared_guids(repo: &Repo, before: &str) -> Result<()> {
    let moved = gang_core::guid_moves::repair(repo, before)?;
    let touched: Vec<&String> = moved.iter().flat_map(|(_, _, _, files)| files).collect();
    if touched.is_empty() {
        return Ok(());
    }
    let mut message = String::from(
        "gang: point references at the shared GUIDs

",
    );
    for (asset, old, new, files) in &moved {
        ui::warn(format!(
            "{} was added on both sides; the merge kept the GUID already shared ({new}).",
            short(asset)
        ));
        for file in files {
            println!("    ✓ {file}: now points at the shared {}", short(asset));
            message.push_str(&format!(
                "- {file}: {asset} {old} -> {new}
"
            ));
        }
    }
    for file in touched {
        repo.run(["add", "--", file])?;
    }
    repo.run(["commit", "-q", "--no-verify", "-m", &message])?;
    ui::done("Committed \"gang: point references at the shared GUIDs\"");
    Ok(())
}

fn rebuilt_prefabs(repo: &Repo, before: &str) -> Result<()> {
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
