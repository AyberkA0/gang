//! Project health checks for `gang doctor` (DESIGN.md §8.2, PLAN.md P11).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use unity_yaml::{GuidIndex, Value};

use crate::repo::{Repo, split_nul};
use crate::unity::UnityProject;
use crate::{attributes, git, guards, semantic, setup, unity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
    Problem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub level: Level,
    pub area: &'static str,
    pub message: String,
    pub fix: Option<String>,
    /// Whether `gang doctor --fix` can fix it.
    pub fixable: bool,
}

fn check(
    level: Level,
    area: &'static str,
    message: impl Into<String>,
    fix: Option<&str>,
    fixable: bool,
) -> Check {
    Check {
        level,
        area,
        message: message.into(),
        fix: fix.map(str::to_owned),
        fixable,
    }
}

/// GUIDs of Unity's built-in resources, never in a project.
fn builtin(guid: &str) -> bool {
    guid.starts_with("0000000000000000") || guid.chars().all(|c| c == '0')
}

pub fn run(repo: &Repo) -> Result<Vec<Check>> {
    let mut checks = Vec::new();
    let projects = unity::find_projects(repo.root());

    // Setup.
    if !setup::is_gang_repo(repo) {
        checks.push(check(
            Level::Problem,
            "setup",
            "this repository is not set up for GANG",
            Some("gang init"),
            false,
        ));
    } else if repo.config_get("merge.gang.driver")?.is_none() {
        checks.push(check(
            Level::Problem,
            "setup",
            "GANG's merge drivers and hooks are not installed in this clone",
            Some("gang doctor --fix"),
            true,
        ));
    }
    let config = crate::config::Config::load(repo.root()).unwrap_or_default();
    let lfs = crate::config::lfs_enabled(repo.root(), &config);
    if !lfs && git::lfs_version().is_some() {
        let pointers = repo.try_run(["lfs", "ls-files", "-n"])?.unwrap_or_default();
        let count = pointers.lines().filter(|l| !l.trim().is_empty()).count();
        if count > 0 {
            checks.push(check(
                Level::Warn,
                "setup",
                format!(
                    "Git LFS is off ([lfs] enabled = false), but {count} file{} in this branch stored in LFS",
                    if count == 1 { " is" } else { "s are" }
                ),
                Some("set [lfs] enabled = true, or move them out of LFS with `git lfs migrate export`"),
                false,
            ));
        }
    }
    if lfs && git::lfs_version().is_none() {
        checks.push(check(
            Level::Warn,
            "setup",
            "Git LFS is not installed; large files will not be stored in LFS",
            Some("install it from https://git-lfs.com"),
            false,
        ));
    }

    // Unity settings and versions.
    let installed = unity::installed_editors();
    for project in &projects {
        let settings = unity::read_settings(&project.path(repo.root()));
        if !settings.is_ok() {
            checks.push(check(
                Level::Problem,
                "unity",
                format!(
                    "{}: Asset Serialization must be Force Text and meta files visible",
                    name(project)
                ),
                Some("gang doctor --fix (with Unity closed)"),
                true,
            ));
        }
        if let Some(version) = &project.editor_version {
            if !installed.is_empty() && !installed.contains(version) {
                checks.push(check(
                    Level::Warn,
                    "unity",
                    format!(
                        "{}: Unity {version} is not installed here (found {})",
                        name(project),
                        installed.join(", ")
                    ),
                    Some("install it with Unity Hub"),
                    false,
                ));
            }
        }
    }

    // Files: metas, GUIDs, junk, names.
    let tracked: Vec<String> = repo.tracked_files()?;
    let index: BTreeSet<String> = tracked.iter().cloned().collect();
    let mut orphan_metas = Vec::new();
    let mut missing_metas = Vec::new();
    for path in &tracked {
        if guards::is_junk(path) {
            checks.push(check(
                Level::Problem,
                "files",
                format!("{path} is an OS junk file"),
                Some("gang doctor --fix"),
                true,
            ));
        }
        let Some(partner) = guards::meta_partner(&projects, path) else {
            continue;
        };
        if let Some(asset) = path.strip_suffix(".meta") {
            let is_folder = index
                .range(format!("{asset}/")..)
                .next()
                .is_some_and(|p| p.starts_with(&format!("{asset}/")));
            if !index.contains(&partner) && !is_folder {
                orphan_metas.push(path.clone());
            }
        } else if !index.contains(&partner) {
            missing_metas.push(path.clone());
        }
    }
    for path in &orphan_metas {
        checks.push(check(
            Level::Warn,
            "files",
            format!("{path} has no asset (an orphan .meta)"),
            Some("gang doctor --fix removes it"),
            true,
        ));
    }
    for path in &missing_metas {
        checks.push(check(
            Level::Problem,
            "files",
            format!("{path} has no .meta in the repository"),
            Some("open the project in Unity and commit the .meta"),
            false,
        ));
    }
    for project in &projects {
        let guids = GuidIndex::scan(&project.path(repo.root()), repo.root());
        for (guid, paths) in guids.duplicates() {
            checks.push(check(
                Level::Problem,
                "files",
                format!("GUID {guid} is used by {}", paths.join(", ")),
                Some("delete one copy's .meta and let Unity create a new one"),
                false,
            ));
        }
    }

    // References between files.
    checks.extend(references(repo, &projects, &tracked)?);

    // Build settings.
    for project in &projects {
        let path = project.join("ProjectSettings/EditorBuildSettings.asset");
        let Some(text) = semantic::worktree(repo, &path) else {
            continue;
        };
        let Ok(file) = unity_yaml::parse(&text) else {
            continue;
        };
        let scenes = file
            .docs
            .first()
            .and_then(|d| d.body())
            .and_then(|b| b.get("m_Scenes"))
            .and_then(Value::as_seq)
            .unwrap_or(&[])
            .to_vec();
        for item in scenes {
            let Some(map) = item.value().as_map() else {
                continue;
            };
            let Some(scene) = map.get("path").and_then(Value::text) else {
                continue;
            };
            let full = project.join(&scene);
            if !index.contains(&full) {
                checks.push(check(
                    Level::Warn,
                    "build",
                    format!("Build Settings lists {scene}, which does not exist"),
                    Some("fix the scene list in Unity (File › Build Settings)"),
                    false,
                ));
            }
        }
    }

    // Layer numbers in code.
    checks.extend(layer_numbers(repo, &projects, &tracked));

    // Hotspots.
    checks.extend(hotspots(repo)?);
    Ok(checks)
}

/// Scripts that use layers by number. The layer remap rewrites what it
/// recognizes and lists the rest, but names never need either.
fn layer_numbers(repo: &Repo, projects: &[UnityProject], tracked: &[String]) -> Vec<Check> {
    let assets: Vec<String> = projects.iter().map(|p| p.join("Assets/")).collect();
    let mut checks = Vec::new();
    for path in tracked
        .iter()
        .filter(|p| p.ends_with(".cs") && assets.iter().any(|a| p.starts_with(a.as_str())))
    {
        let Some(source) = semantic::worktree(repo, path) else {
            continue;
        };
        let lines = gang_csharp::numeric_layer_uses(&source);
        if lines.is_empty() {
            continue;
        }
        let mut list: Vec<String> = lines.iter().take(10).map(usize::to_string).collect();
        if lines.len() > 10 {
            list.push("…".to_owned());
        }
        checks.push(check(
            Level::Warn,
            "layers",
            format!(
                "{path} uses layers by number (line{} {})",
                if lines.len() == 1 { "" } else { "s" },
                list.join(", ")
            ),
            Some(
                "use LayerMask.NameToLayer(\"…\") for a layer and LayerMask.GetMask(\"…\") for a mask, so the code keeps working when a layer moves",
            ),
            false,
        ));
    }
    checks
}

fn name(project: &UnityProject) -> String {
    if project.dir.is_empty() {
        "project".to_owned()
    } else {
        project.dir.clone()
    }
}

/// Missing assets, prefab sources and scripts referenced by scenes, prefabs
/// and assets.
fn references(repo: &Repo, projects: &[UnityProject], tracked: &[String]) -> Result<Vec<Check>> {
    let mut checks = Vec::new();
    let mut known = semantic::guid_index(repo);
    // Package assets are only known when Unity has filled Library/PackageCache.
    let mut complete = true;
    for project in projects {
        let cache = project
            .path(repo.root())
            .join("Library")
            .join("PackageCache");
        if cache.is_dir() {
            let mut packages = GuidIndex::default();
            for entry in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
                packages.extend(GuidIndex::scan_folder(&entry.path(), repo.root()));
            }
            known.extend(packages);
        } else {
            complete = false;
        }
    }
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut missing_sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in tracked
        .iter()
        .filter(|p| attributes::is_unity_yaml(p) && !p.ends_with(".meta"))
    {
        let Some(text) = semantic::worktree(repo, path) else {
            continue;
        };
        let Ok(file) = unity_yaml::parse(&text) else {
            continue;
        };
        for doc in &file.docs {
            let Some(body) = doc.body() else { continue };
            if let Some(source) = body
                .get("m_SourcePrefab")
                .and_then(Value::as_ref)
                .and_then(|r| r.guid)
            {
                if known.path(&source).is_none() && !builtin(&source) {
                    missing_sources
                        .entry(path.clone())
                        .or_default()
                        .push(source);
                }
            }
            let mut refs = HashSet::new();
            collect_guids(&Value::Map(body.clone()), &mut refs);
            for guid in refs {
                if !builtin(&guid) && known.path(&guid).is_none() {
                    missing.entry(path.clone()).or_default().push(guid);
                }
            }
        }
    }
    for (path, sources) in missing_sources {
        checks.push(check(
            Level::Problem,
            "references",
            format!(
                "{path} uses {} prefab{} that no longer exist",
                sources.len(),
                if sources.len() == 1 { "" } else { "s" }
            ),
            Some("restore the prefab or remove its instances"),
            false,
        ));
    }
    if complete {
        for (path, guids) in missing {
            let mut guids = guids;
            guids.sort();
            guids.dedup();
            checks.push(check(
                Level::Problem,
                "references",
                format!(
                    "{path} references {} missing asset{} ({})",
                    guids.len(),
                    if guids.len() == 1 { "" } else { "s" },
                    guids.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ),
                Some("someone may have forgotten to commit an asset"),
                false,
            ));
        }
    } else if !missing.is_empty() {
        checks.push(check(Level::Info, "references", "References to package assets cannot be checked until Unity has opened the project here (Library/PackageCache)", None, false));
    }
    Ok(checks)
}

fn collect_guids(value: &Value, out: &mut HashSet<String>) {
    match value {
        Value::Scalar(_) => {
            if let Some(r) = value.as_ref() {
                if let Some(guid) = r.guid {
                    if r.file_id != 0 {
                        out.insert(guid);
                    }
                }
            }
        }
        Value::Map(map) => map
            .entries()
            .iter()
            .for_each(|e| collect_guids(e.value(), out)),
        Value::Seq(items) => items.iter().for_each(|i| collect_guids(i.value(), out)),
        Value::Empty => {}
    }
}

/// Scenes and prefabs that both sides of recent merges changed.
fn hotspots(repo: &Repo) -> Result<Vec<Check>> {
    let merges = repo
        .try_run(["log", "--merges", "-n", "200", "--format=%H %P"])?
        .unwrap_or_default();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in merges.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let [_, first, second, ..] = parts.as_slice() else {
            continue;
        };
        let Some(base) = repo.try_run(["merge-base", first, second])? else {
            continue;
        };
        let base = base.trim();
        let side = |head: &str| -> BTreeSet<String> {
            repo.try_run(["diff", "--name-only", "-z", base, head])
                .ok()
                .flatten()
                .map(|t| {
                    split_nul(&t)
                        .into_iter()
                        .filter(|p| p.ends_with(".unity") || p.ends_with(".prefab"))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (a, b) = (side(first), side(second));
        for path in a.intersection(&b) {
            *counts.entry(path.clone()).or_default() += 1;
        }
    }
    let mut top: Vec<(String, usize)> = counts.into_iter().filter(|(_, n)| *n >= 3).collect();
    top.sort_by_key(|a| std::cmp::Reverse(a.1));
    Ok(top
        .into_iter()
        .take(5)
        .map(|(path, n)| {
            check(
                Level::Info,
                "hotspots",
                format!("{path} was changed on both sides of {n} recent merges"),
                Some("consider splitting it into additive scenes or prefabs, or agree on who edits which part"),
                false,
            )
        })
        .collect())
}

/// Applies the fixes marked fixable. Returns what was done.
pub fn fix(repo: &Repo, checks: &[Check]) -> Result<Vec<String>> {
    let mut done = Vec::new();
    if checks.iter().any(|c| c.fixable && c.area == "setup") {
        setup::install(repo)?;
        done.push("installed merge drivers and hooks".to_owned());
    }
    if checks.iter().any(|c| c.fixable && c.area == "unity") {
        for project in unity::find_projects(repo.root()) {
            let dir = project.path(repo.root());
            if unity::is_editor_open(&dir) {
                done.push(format!(
                    "skipped Unity settings for {}: Unity is open",
                    name(&project)
                ));
                continue;
            }
            for change in unity::fix_settings(&dir).unwrap_or_default() {
                done.push(change);
            }
        }
    }
    let tracked = repo.tracked_files()?;
    let junk: Vec<&String> = tracked.iter().filter(|p| guards::is_junk(p)).collect();
    if !junk.is_empty() {
        let mut args = vec!["rm", "-q", "--cached", "--"];
        args.extend(junk.iter().map(|s| s.as_str()));
        repo.run(&args)?;
        done.push(format!(
            "stopped tracking {} junk file{}",
            junk.len(),
            if junk.len() == 1 { "" } else { "s" }
        ));
    }
    let orphans: Vec<String> = checks
        .iter()
        .filter(|c| c.area == "files" && c.fixable && c.message.contains("orphan"))
        .filter_map(|c| c.message.split(' ').next().map(str::to_owned))
        .collect();
    if !orphans.is_empty() {
        let mut args = vec!["rm", "-q", "--"];
        args.extend(orphans.iter().map(String::as_str));
        repo.run(&args)?;
        done.push(format!(
            "removed {} orphan .meta file{}",
            orphans.len(),
            if orphans.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(done)
}
