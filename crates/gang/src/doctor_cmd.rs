//! `gang doctor`: project health.

use std::ffi::OsString;

use anyhow::{Context, Result};
use clap::Parser;
use gang_core::doctor::{self, Level};
use gang_core::invocation::Invocation;
use gang_core::repo::Repo;

use crate::ui;

/// Check the project's health
#[derive(Parser)]
#[command(name = "gang doctor")]
pub struct Args {
    /// Fix what can be fixed safely
    #[arg(long)]
    fix: bool,
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = Args::parse_from(
        std::iter::once(OsString::from("gang doctor")).chain(invocation.args.iter().cloned()),
    );
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    let mut checks = doctor::run(&repo)?;
    if args.fix {
        for line in doctor::fix(&repo, &checks)? {
            ui::done(line);
        }
        checks = doctor::run(&repo)?;
    }
    if invocation.options.json {
        let json: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| serde_json::json!({"level": format!("{:?}", c.level).to_lowercase(), "area": c.area, "message": c.message, "fix": c.fix}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else if checks.is_empty() {
        ui::done("No problems found.");
    } else {
        let mut area = "";
        for c in &checks {
            if c.area != area {
                area = c.area;
                println!();
                println!("{}", area[..1].to_uppercase() + &area[1..]);
            }
            let symbol = match c.level {
                Level::Problem => "✗",
                Level::Warn => "⚠",
                Level::Info => "·",
            };
            println!("  {symbol} {}", c.message);
            if let Some(fix) = &c.fix {
                println!("    Fix: {fix}");
            }
        }
    }
    Ok(if checks.iter().any(|c| c.level == Level::Problem) {
        1
    } else {
        0
    })
}

/// After a scene is renamed: code that loads it by its old name.
pub fn scene_name_references(repo: &Repo, old_scene: &str) -> Vec<String> {
    let name = old_scene
        .rsplit('/')
        .next()
        .unwrap_or(old_scene)
        .trim_end_matches(".unity");
    let pattern = format!("\"{name}\"");
    let Ok(Some(output)) = repo.try_run(["grep", "-n", "-F", &pattern, "--", "*.cs"]) else {
        return Vec::new();
    };
    output
        .lines()
        .map(|l| l.split(':').take(2).collect::<Vec<_>>().join(":"))
        .collect()
}
