//! Number claims (DESIGN.md §14.6): Unity names layers, rendering layers and
//! NavMesh areas by their *position* in a project-wide table. Two people who
//! fill the same empty position with different names will clash at the next
//! merge, and every object and script that used the number silently changes
//! meaning.
//!
//! Instead of only repairing that after the fact (the layer remap, §14.2),
//! GANG reserves a new entry for its author the moment it sees it, on the
//! shared ref `refs/gang/claims` (compare-and-swap, like locks). The first
//! claim wins; whoever comes second is told within seconds, before their code
//! and scenes depend on the number, and `gang save` moves their entry to a
//! free number for them.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use unity_yaml::Value;

use crate::config::Config;
use crate::repo::Repo;
use crate::unity::{self, UnityProject};
use crate::{clock, presence, semantic};

pub const REF: &str = "refs/gang/claims";
const LOCAL_REF: &str = "refs/gang/remote/claims";
const SCHEMA: u32 = 1;
const FILE: &str = "claims.json";
/// A claim whose owner has not been seen for this long no longer counts.
const TTL: i64 = 14 * 24 * 3600;

/// A table of names Unity refers to by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Table {
    /// Physics layers (`TagManager.asset`, `layers`).
    Layers,
    /// Rendering layers (`TagManager.asset`, `m_RenderingLayers`).
    RenderingLayers,
    /// NavMesh areas (`NavMeshAreas.asset`, `areas[].name`).
    NavMeshAreas,
}

pub const TABLES: [Table; 3] = [Table::Layers, Table::RenderingLayers, Table::NavMeshAreas];

impl Table {
    /// "layer", "rendering layer", "NavMesh area".
    pub fn label(self) -> &'static str {
        match self {
            Table::Layers => "layer",
            Table::RenderingLayers => "rendering layer",
            Table::NavMeshAreas => "NavMesh area",
        }
    }

    /// The settings file, relative to the project.
    pub fn file(self) -> &'static str {
        match self {
            Table::Layers | Table::RenderingLayers => "ProjectSettings/TagManager.asset",
            Table::NavMeshAreas => "ProjectSettings/NavMeshAreas.asset",
        }
    }

    /// The first position people may name; the ones before are Unity's.
    pub fn first_user(self) -> usize {
        match self {
            Table::Layers => 8,
            Table::RenderingLayers => 1,
            Table::NavMeshAreas => 3,
        }
    }

    pub fn size(self) -> usize {
        32
    }

    /// The names in the table, "" for unused positions.
    pub fn names(self, text: &str) -> Option<Vec<String>> {
        let file = unity_yaml::parse(text).ok()?;
        let body = file.docs.first()?.body()?;
        let key = match self {
            Table::Layers => "layers",
            Table::RenderingLayers => "m_RenderingLayers",
            Table::NavMeshAreas => "areas",
        };
        let items = body.get(key)?.as_seq()?;
        Some(
            items
                .iter()
                .map(|item| match self {
                    Table::NavMeshAreas => item
                        .value()
                        .as_map()
                        .and_then(|m| m.get("name"))
                        .and_then(Value::text)
                        .unwrap_or_default(),
                    _ => item.value().text().unwrap_or_default(),
                })
                .collect(),
        )
    }
}

/// One reserved position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub table: Table,
    /// The Unity project's folder in the repository ("" for the root).
    #[serde(default)]
    pub project: String,
    pub index: u32,
    pub name: String,
    pub owner: String,
    pub since: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClaimFile {
    pub schema: u32,
    pub claims: Vec<Claim>,
}

/// A name this clone has that the default branch does not have yet.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub table: Table,
    pub project: String,
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Reserved for this user.
    Mine,
    /// Not reserved yet (no remote, offline, or not synced yet).
    Unclaimed,
    /// A teammate reserved the same name at the same position: one entry.
    Shared { with: String },
    /// A teammate reserved this position for another name first. `to` is
    /// where ours can go: the teammate's position when they reserved the
    /// same name elsewhere, else the first free one.
    Clash { holder: Claim, to: Option<u32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub entry: Entry,
    pub status: Status,
}

impl Check {
    pub fn is_clash(&self) -> bool {
        matches!(self.status, Status::Clash { .. })
    }

    /// One line for a person: what clashes and what GANG will do.
    pub fn describe(&self) -> Option<String> {
        let Status::Clash { holder, to } = &self.status else {
            return None;
        };
        let e = &self.entry;
        let when = clock::parse(&holder.since)
            .map(|t| format!(", {}", clock::ago(clock::now() - t)))
            .unwrap_or_default();
        let target = match to {
            Some(to) if holder.name == e.name => format!(
                "{} already reserved {:?} as {} {to}; yours moves there",
                holder.owner,
                e.name,
                e.table.label()
            ),
            Some(to) => format!("yours moves to {} {to}", e.table.label()),
            None => format!("no free {} is left", e.table.label()),
        };
        Some(format!(
            "{} {} is reserved for {:?} by {}{when}; you named it {:?}: {target}",
            capitalize(e.table.label()),
            e.index,
            holder.name,
            holder.owner,
            e.name,
        ))
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `GANG_NO_CLAIMS=1` turns reservations off (tests use it to stand for a
/// teammate whose GANG predates them, or who used plain git offline).
fn disabled() -> bool {
    std::env::var_os("GANG_NO_CLAIMS").is_some()
}

/// The branch new names are compared with: the team's default branch.
pub fn default_branch(repo: &Repo, config: &Config) -> Result<Option<String>> {
    let mut candidates = Vec::new();
    if let Some(branch) = &config.project.default_branch {
        candidates.push(format!("origin/{branch}"));
    }
    if let Some(head) =
        repo.try_run(["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"])?
    {
        candidates.push(head.trim().to_owned());
    }
    candidates.extend(["origin/main".to_owned(), "origin/master".to_owned()]);
    for candidate in candidates {
        if repo
            .try_run([
                "rev-parse",
                "-q",
                "--verify",
                &format!("{candidate}^{{commit}}"),
            ])?
            .is_some()
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn table_path(project: &UnityProject, table: Table) -> String {
    project.join(table.file())
}

/// The names in `table` at `rev` ("" for the working tree).
fn names_at(repo: &Repo, project: &UnityProject, table: Table, rev: &str) -> Option<Vec<String>> {
    let path = table_path(project, table);
    let text = if rev.is_empty() {
        semantic::worktree(repo, &path)?
    } else {
        semantic::blob(repo, &format!("{rev}:{path}"))
            .ok()
            .flatten()?
    };
    table.names(&text)
}

/// Names in the working tree at positions the default branch leaves empty.
pub fn new_entries(repo: &Repo, upstream: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    for project in unity::find_projects(repo.root()) {
        for table in TABLES {
            let Some(mine) = names_at(repo, &project, table, "") else {
                continue;
            };
            let theirs = names_at(repo, &project, table, upstream).unwrap_or_default();
            for (i, name) in mine.iter().enumerate().skip(table.first_user()) {
                let upstream_name = theirs.get(i).map(String::as_str).unwrap_or("");
                if !name.is_empty() && upstream_name.is_empty() {
                    entries.push(Entry {
                        table,
                        project: project.dir.clone(),
                        index: i as u32,
                        name: name.clone(),
                    });
                }
            }
        }
    }
    entries
}

fn same_place(claim: &Claim, table: Table, project: &str, index: u32) -> bool {
    claim.table == table && claim.project == project && claim.index == index
}

/// Whether a claim still counts: its owner was seen recently.
fn alive(claim: &Claim, team: &[presence::Presence], now: i64) -> bool {
    let seen = team
        .iter()
        .find(|p| p.user == claim.owner)
        .and_then(|p| clock::parse(&p.ts))
        .or_else(|| clock::parse(&claim.since));
    seen.is_none_or(|t| now - t <= TTL)
}

/// A claim the default branch has made true: the name is there now.
fn fulfilled(repo: &Repo, upstream: &str, claim: &Claim) -> bool {
    let project = UnityProject {
        dir: claim.project.clone(),
        editor_version: None,
    };
    names_at(repo, &project, claim.table, upstream)
        .and_then(|names| names.get(claim.index as usize).cloned())
        .is_some_and(|name| !name.is_empty())
}

/// The status of each new entry against `table`.
pub fn check(
    repo: &Repo,
    upstream: &str,
    entries: &[Entry],
    table: &ClaimFile,
    me: &str,
    team: &[presence::Presence],
) -> Vec<Check> {
    let now = clock::now();
    let live: Vec<&Claim> = table
        .claims
        .iter()
        .filter(|c| alive(c, team, now))
        .collect();
    let mut taken: BTreeSet<(Table, String, u32)> = BTreeSet::new();
    let mut checks = Vec::new();
    for entry in entries {
        let here = live
            .iter()
            .find(|c| same_place(c, entry.table, &entry.project, entry.index));
        let status = match here {
            None => Status::Unclaimed,
            Some(c) if c.owner == me => Status::Mine,
            Some(c) if c.name == entry.name => Status::Shared {
                with: c.owner.clone(),
            },
            Some(c) => {
                let holder = (*c).clone();
                // The same name reserved elsewhere is the same entry.
                let same_name = live.iter().find(|o| {
                    o.table == entry.table
                        && o.project == entry.project
                        && o.name == entry.name
                        && o.owner != me
                });
                let to = match same_name {
                    Some(o) => Some(o.index),
                    None => free_index(repo, upstream, entry, &live, entries, &taken),
                };
                if let Some(to) = to {
                    taken.insert((entry.table, entry.project.clone(), to));
                }
                Status::Clash { holder, to }
            }
        };
        checks.push(Check {
            entry: entry.clone(),
            status,
        });
    }
    checks
}

/// The first position that nobody uses: empty on the default branch and in
/// this clone, and not reserved by anyone.
fn free_index(
    repo: &Repo,
    upstream: &str,
    entry: &Entry,
    live: &[&Claim],
    entries: &[Entry],
    taken: &BTreeSet<(Table, String, u32)>,
) -> Option<u32> {
    let project = UnityProject {
        dir: entry.project.clone(),
        editor_version: None,
    };
    let mine = names_at(repo, &project, entry.table, "").unwrap_or_default();
    let theirs = names_at(repo, &project, entry.table, upstream).unwrap_or_default();
    let empty = |list: &[String], i: usize| list.get(i).is_none_or(|n| n.is_empty());
    (entry.table.first_user()..entry.table.size())
        .map(|i| i as u32)
        .find(|&i| {
            empty(&mine, i as usize)
                && empty(&theirs, i as usize)
                && !live
                    .iter()
                    .any(|c| same_place(c, entry.table, &entry.project, i))
                && !entries
                    .iter()
                    .any(|e| e.table == entry.table && e.project == entry.project && e.index == i)
                && !taken.contains(&(entry.table, entry.project.clone(), i))
        })
}

/// Fetches the shared claims into `refs/gang/remote/claims`.
pub fn fetch(repo: &Repo) -> Result<()> {
    if !presence::has_origin(repo)? {
        return Ok(());
    }
    let output = repo.output([
        "fetch",
        "-q",
        "--no-tags",
        "--no-write-fetch-head",
        "origin",
        &format!("+{REF}:{LOCAL_REF}"),
    ])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("couldn't find remote ref") {
            let _ = repo.run(["update-ref", "-d", LOCAL_REF]);
            return Ok(());
        }
        bail!("could not fetch claims: {}", stderr.trim());
    }
    Ok(())
}

/// The claims as last fetched.
pub fn read(repo: &Repo) -> Result<ClaimFile> {
    let Some(text) = repo.try_run(["cat-file", "-p", &format!("{LOCAL_REF}:{FILE}")])? else {
        return Ok(ClaimFile {
            schema: SCHEMA,
            claims: Vec::new(),
        });
    };
    serde_json::from_str(&text).context("the shared claims are unreadable")
}

/// Changes the shared claims with compare-and-swap, like the lock table.
/// `change` returns whether it changed anything; nothing is pushed if not.
pub fn update(repo: &Repo, mut change: impl FnMut(&mut ClaimFile) -> Result<bool>) -> Result<()> {
    if !presence::has_origin(repo)? {
        return Ok(());
    }
    let mut delay = Duration::from_millis(200);
    for _ in 0..12 {
        let tip = repo
            .try_run(["ls-remote", "origin", REF])?
            .and_then(|out| out.split_whitespace().next().map(str::to_owned));
        if tip.is_some() {
            fetch(repo)?;
        }
        let mut table = if tip.is_some() {
            read(repo)?
        } else {
            ClaimFile {
                schema: SCHEMA,
                claims: Vec::new(),
            }
        };
        if !change(&mut table)? {
            return Ok(());
        }
        table.schema = SCHEMA;
        table.claims.sort_by(|a, b| {
            (a.table, &a.project, a.index, &a.owner).cmp(&(b.table, &b.project, b.index, &b.owner))
        });
        let commit = write_commit(repo, &table)?;
        let lease = format!("--force-with-lease={REF}:{}", tip.as_deref().unwrap_or(""));
        let output = repo.output(["push", "-q", &lease, "origin", &format!("{commit}:{REF}")])?;
        if output.status.success() {
            repo.run(["update-ref", LOCAL_REF, &commit])?;
            return Ok(());
        }
        std::thread::sleep(delay + Duration::from_millis(u64::from(std::process::id() % 97)));
        delay = (delay * 2).min(Duration::from_secs(3));
    }
    bail!("could not update the shared claims: too many people changing them at once")
}

fn write_commit(repo: &Repo, table: &ClaimFile) -> Result<String> {
    let mut json = serde_json::to_string_pretty(table)?;
    json.push('\n');
    let blob = repo.run_with_input(["hash-object", "-w", "--stdin"], json.as_bytes())?;
    let tree = repo.run_with_input(
        ["mktree"],
        format!("100644 blob {}\t{FILE}\n", blob.trim()).as_bytes(),
    )?;
    let output = repo
        .command()
        .env("GIT_AUTHOR_NAME", "GANG")
        .env("GIT_AUTHOR_EMAIL", "gang@localhost")
        .env("GIT_COMMITTER_NAME", "GANG")
        .env("GIT_COMMITTER_EMAIL", "gang@localhost")
        .args(["commit-tree", tree.trim(), "-m", "claims [skip ci]"])
        .output()?;
    anyhow::ensure!(output.status.success(), "could not write the claims commit");
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// What this clone knows without the network: its new entries checked
/// against the claims as last fetched. Empty without a default branch.
pub fn local(repo: &Repo, config: &Config) -> Result<Vec<Check>> {
    if disabled() {
        return Ok(Vec::new());
    }
    let Some(upstream) = default_branch(repo, config)? else {
        return Ok(Vec::new());
    };
    let entries = new_entries(repo, &upstream);
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let me = presence::identity(repo)?.login;
    let team = presence::team(repo).unwrap_or_default();
    Ok(check(repo, &upstream, &entries, &read(repo)?, &me, &team))
}

/// `(table, project, index, name)` of every name in the tables on our
/// local branches.
fn on_local_branches(repo: &Repo) -> BTreeSet<(Table, String, u32, String)> {
    let mut names = BTreeSet::new();
    let Ok(branches) = repo.run(["for-each-ref", "--format=%(refname)", "refs/heads/"]) else {
        return names;
    };
    let projects = unity::find_projects(repo.root());
    for branch in branches.lines().take(50) {
        for project in &projects {
            for table in TABLES {
                for (i, name) in names_at(repo, project, table, branch)
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    if !name.is_empty() {
                        names.insert((table, project.dir.clone(), i as u32, name));
                    }
                }
            }
        }
    }
    names
}

/// What [`sync`] did.
#[derive(Debug, Default)]
pub struct Synced {
    /// This clone's new entries against the shared claims.
    pub checks: Vec<Check>,
    /// Entries reserved for this user just now.
    pub reserved: Vec<Entry>,
}

/// Brings the shared claims in line with this clone: reserves its new
/// entries that nobody holds, renames its own reservations when the name
/// changed, releases the ones it no longer has, and drops reservations the
/// default branch has made true or whose owner is long gone. Uses the
/// network only when this clone has new entries or reservations.
pub fn sync(repo: &Repo, config: &Config) -> Result<Synced> {
    if disabled() {
        return Ok(Synced::default());
    }
    let Some(upstream) = default_branch(repo, config)? else {
        return Ok(Synced::default());
    };
    if !presence::has_origin(repo)? {
        return Ok(Synced::default());
    }
    let mut entries = new_entries(repo, &upstream);
    if !entries.is_empty() {
        // A name the team already pushed is not ours to reserve: the merge
        // moves ours then (§14.2). Look at the branch as it is now.
        if let Some(branch) = upstream.strip_prefix("origin/") {
            let _ = repo.output([
                "fetch",
                "-q",
                "--no-tags",
                "origin",
                &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
            ]);
            entries = new_entries(repo, &upstream);
        }
    }
    let me = presence::identity(repo)?.login;
    let ours = read(repo)?.claims.iter().any(|c| c.owner == me);
    if entries.is_empty() && !ours {
        return Ok(Synced::default());
    }
    let team = presence::team(repo).unwrap_or_default();
    let now = clock::now();
    let mut reserved = Vec::new();
    // Names our other local branches still have keep their reservations:
    // switching to main must not free a number the feature branch uses.
    let kept = on_local_branches(repo);
    update(repo, |table| {
        reserved.clear();
        let before = table.clone();
        let checks = check(repo, &upstream, &entries, table, &me, &team);
        // Keep other people's live, unfulfilled claims, and ours that this
        // clone still has.
        table.claims.retain(|c| {
            if fulfilled(repo, &upstream, c) || !alive(c, &team, now) {
                return false;
            }
            c.owner != me
                || entries
                    .iter()
                    .any(|e| same_place(c, e.table, &e.project, e.index))
                || kept.contains(&(c.table, c.project.clone(), c.index, c.name.clone()))
        });
        for check in &checks {
            let e = &check.entry;
            match check.status {
                Status::Unclaimed => {
                    reserved.push(e.clone());
                    table.claims.push(Claim {
                        table: e.table,
                        project: e.project.clone(),
                        index: e.index,
                        name: e.name.clone(),
                        owner: me.clone(),
                        since: clock::format(now),
                    });
                }
                Status::Mine => {
                    if let Some(c) = table
                        .claims
                        .iter_mut()
                        .find(|c| same_place(c, e.table, &e.project, e.index) && c.owner == me)
                    {
                        c.name = e.name.clone();
                    }
                }
                _ => {}
            }
        }
        Ok(*table != before)
    })?;
    Ok(Synced {
        checks: check(repo, &upstream, &entries, &read(repo)?, &me, &team),
        reserved,
    })
}

/// Names this clone added at positions the default branch has meanwhile
/// given another name: the merge-time remap will move them at the next
/// sync. One line each, for an early warning (no network).
pub fn taken_upstream(repo: &Repo, config: &Config) -> Vec<String> {
    if disabled() {
        return Vec::new();
    }
    let Ok(Some(upstream)) = default_branch(repo, config) else {
        return Vec::new();
    };
    let Some(base) = repo
        .try_run(["merge-base", "HEAD", &upstream])
        .ok()
        .flatten()
        .map(|b| b.trim().to_owned())
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for project in unity::find_projects(repo.root()) {
        let table = Table::Layers;
        let (Some(mine), Some(theirs)) = (
            names_at(repo, &project, table, ""),
            names_at(repo, &project, table, &upstream),
        ) else {
            continue;
        };
        let base_names = names_at(repo, &project, table, &base).unwrap_or_default();
        for i in table.first_user()..table.size() {
            let get = |list: &[String]| list.get(i).cloned().unwrap_or_default();
            let (m, t, b) = (get(&mine), get(&theirs), get(&base_names));
            if !m.is_empty() && !t.is_empty() && m != t && b.is_empty() {
                lines.push(format!(
                    "Layer {i} is {t:?} on {upstream} already; your {m:?} moves to a free layer when you sync. Until then, avoid writing its number in code."
                ));
            }
        }
    }
    lines
}

/// Gives up this user's reservations of `name` (all tables, all projects).
pub fn release(repo: &Repo, name: &str) -> Result<usize> {
    let me = presence::identity(repo)?.login;
    let mut released = 0;
    update(repo, |table| {
        let before = table.claims.len();
        table.claims.retain(|c| !(c.owner == me && c.name == name));
        released = before - table.claims.len();
        Ok(released > 0)
    })?;
    Ok(released)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_table() {
        let tags = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!78 &1\nTagManager:\n  serializedVersion: 3\n  tags: []\n  layers:\n  - Default\n  - \n  - UI\n  m_SortingLayers:\n  - name: Default\n    uniqueID: 0\n    locked: 0\n  m_RenderingLayers:\n  - Default\n  - Glow\n";
        assert_eq!(
            Table::Layers.names(tags).unwrap(),
            vec!["Default", "", "UI"]
        );
        assert_eq!(
            Table::RenderingLayers.names(tags).unwrap(),
            vec!["Default", "Glow"]
        );
        let areas = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!126 &1\nNavMeshProjectSettings:\n  m_ObjectHideFlags: 0\n  serializedVersion: 2\n  areas:\n  - name: Walkable\n    cost: 1\n  - name: Not Walkable\n    cost: 1\n  - name: Jump\n    cost: 2\n  - name: Water\n    cost: 3\n  - name: \n    cost: 1\n";
        assert_eq!(
            Table::NavMeshAreas.names(areas).unwrap(),
            vec!["Walkable", "Not Walkable", "Jump", "Water", ""]
        );
    }

    #[test]
    fn clash_message() {
        let check = Check {
            entry: Entry {
                table: Table::Layers,
                project: String::new(),
                index: 8,
                name: "Interactable".into(),
            },
            status: Status::Clash {
                holder: Claim {
                    table: Table::Layers,
                    project: String::new(),
                    index: 8,
                    name: "Projectile".into(),
                    owner: "ayberk".into(),
                    since: "2000-01-01T00:00:00Z".into(),
                },
                to: Some(9),
            },
        };
        let text = check.describe().unwrap();
        assert!(
            text.starts_with("Layer 8 is reserved for \"Projectile\" by ayberk"),
            "{text}"
        );
        assert!(
            text.ends_with("you named it \"Interactable\": yours moves to layer 9"),
            "{text}"
        );
    }
}
