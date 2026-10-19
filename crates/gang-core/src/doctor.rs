//! Project health checks for `gang doctor` (DESIGN.md §8.2, PLAN.md P11).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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

    // Objects on layers with no name, and tags nobody defined.
    checks.extend(undefined_names(repo, &projects, &tracked));

    // Hotspots.
    checks.extend(hotspots(repo)?);

    // Storage: GitHub's limits, and what makes the history big.
    checks.extend(storage(repo).unwrap_or_default());
    Ok(checks)
}

/// The repository's size against GitHub's limits (GitHub recommends staying
/// under 1 GB and asks large repositories to stay under 5 GB), and the files
/// whose versions take the most room.
fn storage(repo: &Repo) -> Result<Vec<Check>> {
    let mut checks = Vec::new();
    let counts = repo.run(["count-objects", "-v"])?;
    let kib = |key: &str| -> u64 {
        counts
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    let bytes = (kib("size-pack:") + kib("size:")) * 1024;
    let partial = repo.config_get("remote.origin.promisor")?.as_deref() == Some("true");
    const MB: u64 = 1024 * 1024;
    if partial {
        checks.push(check(
            Level::Info,
            "storage",
            format!(
                "partial clone: this clone holds {} MB; older versions of large files download only when needed",
                bytes / MB
            ),
            None,
            false,
        ));
    }
    if bytes < 100 * MB {
        return Ok(checks);
    }
    let level = if bytes > 5 * 1024 * MB {
        Level::Problem
    } else if bytes > 1024 * MB {
        Level::Warn
    } else {
        Level::Info
    };
    if level != Level::Info || !partial {
        checks.push(check(
            level,
            "storage",
            format!(
                "the repository's history is {} MB (GitHub recommends under 1 GB, and asks for under 5 GB)",
                bytes / MB
            ),
            Some("teammates clone with `gang clone` (partial: large old versions stay on GitHub until needed); keep raw source art (PSD, WAV, video) out of git where you can"),
            false,
        ));
    }
    // What takes the room: total size of every stored version, per path.
    // Not in a partial clone: asking for the sizes would download them.
    if partial {
        return Ok(checks);
    }
    let objects = repo.run(["rev-list", "--objects", "--all"])?;
    let mut paths: BTreeMap<String, String> = BTreeMap::new();
    let mut ids = Vec::new();
    for line in objects.lines() {
        if let Some((id, path)) = line.split_once(' ') {
            if !path.is_empty() {
                paths.insert(id.to_owned(), path.to_owned());
                ids.push(id.to_owned());
            }
        }
    }
    let input = ids.join(
        "
",
    );
    let sizes = repo.run_with_input(
        [
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ],
        input.as_bytes(),
    )?;
    let mut totals: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    for line in sizes.lines() {
        let mut parts = line.split(' ');
        let (Some(id), Some("blob"), Some(size)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(path) = paths.get(id) else { continue };
        let entry = totals.entry(path.clone()).or_default();
        entry.0 += size.parse::<u64>().unwrap_or(0);
        entry.1 += 1;
    }
    let mut biggest: Vec<(String, (u64, usize))> = totals.into_iter().collect();
    biggest.sort_by_key(|(_, (size, _))| std::cmp::Reverse(*size));
    for (path, (size, versions)) in biggest.into_iter().take(3) {
        if size < 50 * MB {
            break;
        }
        checks.push(check(
            Level::Info,
            "storage",
            format!(
                "{path}: {versions} version{} take {} MB of the history",
                if versions == 1 { "" } else { "s" },
                size / MB
            ),
            None,
            false,
        ));
    }
    Ok(checks)
}

/// Objects on a layer that has no name (someone removed it while others
/// still used it) and objects with a tag the TagManager does not define.
fn undefined_names(repo: &Repo, projects: &[UnityProject], tracked: &[String]) -> Vec<Check> {
    const BUILT_IN_TAGS: &[&str] = &[
        "Untagged",
        "Respawn",
        "Finish",
        "EditorOnly",
        "MainCamera",
        "Player",
        "GameController",
    ];
    let mut checks = Vec::new();
    for project in projects {
        let Some(tags_text) =
            semantic::worktree(repo, &project.join("ProjectSettings/TagManager.asset"))
        else {
            continue;
        };
        let Ok(tags_file) = unity_yaml::parse(&tags_text) else {
            continue;
        };
        let body = tags_file.docs.first().and_then(|d| d.body());
        let names: Vec<String> = body
            .and_then(|b| b.get("layers"))
            .and_then(Value::as_seq)
            .unwrap_or(&[])
            .iter()
            .map(|i| i.value().text().unwrap_or_default())
            .collect();
        let tags: BTreeSet<String> = body
            .and_then(|b| b.get("tags"))
            .and_then(Value::as_seq)
            .unwrap_or(&[])
            .iter()
            .filter_map(|i| i.value().text())
            .collect();
        let assets = project.join("Assets/");
        let mut unnamed: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
        let mut unknown: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for path in tracked.iter().filter(|p| {
            p.starts_with(assets.as_str()) && (p.ends_with(".unity") || p.ends_with(".prefab"))
        }) {
            let Some(text) = semantic::worktree(repo, path) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim();
                if let Some(layer) = line
                    .strip_prefix("m_Layer: ")
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    if layer >= 8 && names.get(layer as usize).is_none_or(|n| n.is_empty()) {
                        unnamed.entry(layer).or_default().insert(path.clone());
                    }
                } else if let Some(tag) = line.strip_prefix("m_TagString: ") {
                    let tag = tag.trim();
                    if !tag.is_empty() && !BUILT_IN_TAGS.contains(&tag) && !tags.contains(tag) {
                        unknown
                            .entry(tag.to_owned())
                            .or_default()
                            .insert(path.clone());
                    }
                }
            }
        }
        for (layer, files) in unnamed {
            let list: Vec<&str> = files.iter().map(String::as_str).take(3).collect();
            checks.push(check(
                Level::Warn,
                "layers",
                format!(
                    "objects are on layer {layer}, which has no name ({}{})",
                    list.join(", "),
                    if files.len() > 3 { ", …" } else { "" }
                ),
                Some("name the layer again in Project Settings › Tags and Layers, or move those objects to another layer"),
                false,
            ));
        }
        for (tag, files) in unknown {
            let list: Vec<&str> = files.iter().map(String::as_str).take(3).collect();
            checks.push(check(
                Level::Warn,
                "tags",
                format!(
                    "objects use the tag {tag:?}, which the project does not define ({}{})",
                    list.join(", "),
                    if files.len() > 3 { ", …" } else { "" }
                ),
                Some("add the tag again in Project Settings › Tags and Layers, or retag those objects"),
                false,
            ));
        }
    }
    checks
}

/// Scripts that use layers by number. The layer remap rewrites what it
/// recognizes and lists the rest, but names never need either.
fn layer_numbers(repo: &Repo, projects: &[UnityProject], tracked: &[String]) -> Vec<Check> {
    // With a Unity editor on the machine, the compiler says exactly where a
    // number reaches Unity as a layer (§14.2).
    let analyses: Vec<crate::csharp::Analysis> = projects
        .iter()
        .filter_map(|p| crate::csharp::analyze(repo, p))
        .collect();
    let mut checks = Vec::new();
    let mut traced_files: BTreeSet<String> = BTreeSet::new();
    {
        let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for analysis in &analyses {
            for u in &analysis.uses {
                let user_layer = match (u.role, u.value) {
                    (crate::csharp::Role::Index, Some(v)) => v >= 8,
                    (crate::csharp::Role::Mask, Some(v)) => {
                        (v as u64 as u32) & !0xff != 0 && v != -1
                    }
                    _ => false,
                };
                if u.kind == "literal" && user_layer && u.file.contains("Assets/") {
                    by_file
                        .entry(u.file.clone())
                        .or_default()
                        .push(format!("{} ({})", u.line, u.via));
                }
            }
        }
        traced_files.extend(by_file.keys().cloned());
        checks.extend(by_file.into_iter().map(|(file, uses)| {
                let mut shown: Vec<String> = uses.iter().take(5).cloned().collect();
                if uses.len() > 5 {
                    shown.push("…".to_owned());
                }
                check(
                    Level::Warn,
                    "layers",
                    format!("{file} gives Unity layers by number: line {}", shown.join(", line ")),
                    Some(
                        "use LayerMask.NameToLayer(\"…\") for a layer and LayerMask.GetMask(\"…\") for a mask, so the code keeps working when a layer moves (`gang layers code` lists them all)",
                    ),
                    false,
                )
        }));
    }
    // The text rules for the rest: code the compiler could not resolve, or
    // no editor on this machine.
    let assets: Vec<String> = projects.iter().map(|p| p.join("Assets/")).collect();
    for path in tracked.iter().filter(|p| {
        p.ends_with(".cs")
            && assets.iter().any(|a| p.starts_with(a.as_str()))
            && !traced_files.contains(*p)
    }) {
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
        // Where a missing asset went: the commit that deleted it (R29).
        let mut deletions: HashMap<String, Option<(String, String, String)>> = HashMap::new();
        for (path, guids) in missing {
            let mut guids = guids;
            guids.sort();
            guids.dedup();
            let named: Vec<String> = guids
                .iter()
                .take(3)
                .map(|guid| {
                    match deletions
                        .entry(guid.clone())
                        .or_insert_with(|| crate::orphans::deleted_asset(repo, guid))
                    {
                        Some((asset, hash, author)) => {
                            format!("{asset}, deleted in {hash} by {author}")
                        }
                        None => guid.clone(),
                    }
                })
                .collect();
            let fix = guids
                .iter()
                .find_map(|g| deletions.get(g).cloned().flatten())
                .map(|(asset, hash, _)| {
                    format!(
                        "choose another asset in Unity, or bring it back: gang git checkout {hash}~1 -- \"{asset}\" \"{asset}.meta\""
                    )
                })
                .unwrap_or_else(|| "someone may have forgotten to commit an asset".to_owned());
            checks.push(check(
                Level::Problem,
                "references",
                format!(
                    "{path} references {} missing asset{} ({})",
                    guids.len(),
                    if guids.len() == 1 { "" } else { "s" },
                    named.join("; ")
                ),
                Some(&fix),
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
