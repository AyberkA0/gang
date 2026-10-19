//! Conflicts waiting for `gang bang` (DESIGN.md §13.1, §15).
//!
//! The merge driver keeps the three versions of every file it could not
//! merge completely, with the conflicts and the answers given so far, in
//! `.git/gang/merge/<file>/`. `gang bang` re-runs the merge with the answers.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use gang_merge::describe;
use gang_merge::diff::Rules;
use gang_merge::json::merge_json;
use gang_merge::merge::{Choice, ConflictKind, Side, merge};
use serde::{Deserialize, Serialize};

use crate::repo::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Unity,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Value,
    Move,
    Order,
    DeletedByYou,
    DeletedByThem,
}

/// A conflict, with words ready for the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub kind: Kind,
    pub place: String,
    pub why: String,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    #[serde(default)]
    pub orphans: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "choice", content = "value", rename_all = "kebab-case")]
pub enum Answer {
    Ours,
    Theirs,
    Base,
    Value(String),
    Delete,
    Keep,
    Reparent,
}

impl Answer {
    fn to_choice(&self) -> Choice {
        match self {
            Answer::Ours => Choice::Ours,
            Answer::Theirs => Choice::Theirs,
            Answer::Base => Choice::Base,
            Answer::Value(v) => Choice::Value(v.clone()),
            Answer::Delete => Choice::Delete,
            Answer::Keep => Choice::Keep,
            Answer::Reparent => Choice::Reparent,
        }
    }
}

/// One file's pending conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub path: String,
    pub format: Format,
    /// Who the other side is, for the questions ("mehmet", "origin/main").
    pub theirs_name: String,
    pub questions: Vec<Question>,
    #[serde(default)]
    pub answers: BTreeMap<String, Answer>,
}

fn dir(repo: &Repo, path: &str) -> PathBuf {
    let encoded: String = path.bytes().map(|b| format!("{b:02x}")).collect();
    repo.gang_dir().join("merge").join(encoded)
}

/// Every file with pending conflicts.
pub fn pending(repo: &Repo) -> Result<Vec<Pending>> {
    let root = repo.gang_dir().join("merge");
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut all = Vec::new();
    for entry in entries.flatten() {
        let state = entry.path().join("state.json");
        if let Ok(text) = fs::read_to_string(&state) {
            if let Ok(pending) = serde_json::from_str::<Pending>(&text) {
                all.push(pending);
            }
        }
    }
    all.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(all)
}

pub fn save(repo: &Repo, pending: &Pending) -> Result<()> {
    let dir = dir(repo, &pending.path);
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("state.json"),
        serde_json::to_string_pretty(pending)?,
    )?;
    Ok(())
}

pub fn clear(repo: &Repo, path: &str) -> Result<()> {
    let dir = dir(repo, path);
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// Forgets every pending conflict (after a merge is aborted).
pub fn clear_all(repo: &Repo) -> Result<()> {
    let root = repo.gang_dir().join("merge");
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn versions(repo: &Repo, path: &str) -> Result<(String, String, String)> {
    let dir = dir(repo, path);
    let read = |name: &str| {
        fs::read_to_string(dir.join(name)).with_context(|| format!("missing {name} for {path}"))
    };
    Ok((read("base")?, read("ours")?, read("theirs")?))
}

/// Result of merging one file.
pub struct Outcome {
    pub text: String,
    pub questions: Vec<Question>,
}

/// Merges three versions. Unity YAML that cannot be parsed returns `None`
/// (the caller falls back to a text merge).
#[allow(clippy::too_many_arguments)]
pub fn merge_texts(
    format: Format,
    base: &str,
    ours: &str,
    theirs: &str,
    rules: &Rules,
    answers: &BTreeMap<String, Answer>,
    guids: Option<&unity_yaml::GuidIndex>,
    theirs_name: &str,
) -> Option<Outcome> {
    let decisions: HashMap<String, Choice> = answers
        .iter()
        .map(|(k, v)| (k.clone(), v.to_choice()))
        .collect();
    match format {
        Format::Json => {
            let merged = merge_json(base, ours, theirs, &decisions).ok()?;
            let questions = merged
                .conflicts
                .into_iter()
                .map(|c| Question {
                    id: c.path.clone(),
                    kind: Kind::Value,
                    place: c.path.replace('/', " › "),
                    why: format!(
                        "You both changed {}.",
                        c.path.rsplit('/').next().unwrap_or(&c.path)
                    ),
                    base: c.base,
                    ours: c.ours,
                    theirs: c.theirs,
                    orphans: Vec::new(),
                    parent: None,
                })
                .collect();
            Some(Outcome {
                text: merged.text,
                questions,
            })
        }
        Format::Unity => {
            let (b, mut o, mut t) = (
                unity_yaml::parse(base).ok()?,
                unity_yaml::parse(ours).ok()?,
                unity_yaml::parse(theirs).ok()?,
            );
            // Collision matrices merge cell by cell, never as one value.
            crate::matrix::premerge(&b, &mut o, &mut t);
            let merged = merge(&b, &o, &t, rules, &decisions);
            let questions = merged
                .conflicts
                .iter()
                .map(|c| {
                    let d = describe::describe(c, &b, &o, &t, guids, theirs_name);
                    Question {
                        id: c.id.clone(),
                        kind: match c.kind {
                            ConflictKind::Value => Kind::Value,
                            ConflictKind::Move => Kind::Move,
                            ConflictKind::Order => Kind::Order,
                            ConflictKind::DeleteModify {
                                deleted_by: Side::Ours,
                            } => Kind::DeletedByYou,
                            ConflictKind::DeleteModify {
                                deleted_by: Side::Theirs,
                            } => Kind::DeletedByThem,
                        },
                        place: d.place,
                        why: d.why,
                        base: c.base.clone(),
                        ours: c.ours.clone(),
                        theirs: c.theirs.clone(),
                        orphans: d.orphans,
                        parent: d.parent,
                    }
                })
                .collect();
            Some(Outcome {
                text: merged.file.to_text(),
                questions,
            })
        }
    }
}

/// Called by the merge driver: merges, keeps the versions when there are
/// conflicts, and returns the merged text and whether conflicts remain.
/// `ours` is always the user's side and `theirs` the upstream side; during
/// a rebase the caller swaps git's versions to make it so.
#[allow(clippy::too_many_arguments)]
pub fn driver_merge(
    repo: &Repo,
    path: &str,
    format: Format,
    base: &str,
    ours: &str,
    theirs: &str,
    rules: &Rules,
    theirs_name: &str,
) -> Option<(String, bool)> {
    // Both created the same folder: two .meta files that differ only in the
    // GUID. Keep the one already shared (the upstream side) without asking;
    // nothing refers to a folder by GUID in practice (field test C1).
    let folder_meta = |text: &str| text.contains("folderAsset: yes");
    if path.ends_with(".meta") && base.trim().is_empty() && folder_meta(ours) && folder_meta(theirs)
    {
        let _ = clear(repo, path);
        return Some((theirs.to_owned(), false));
    }
    // Both added the same asset: its GUID is identity, not a choice. Keep
    // the one already shared (the upstream side); references to ours are
    // pointed at it after the merge (DESIGN.md §14.5).
    let guid_of = |text: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix("guid:"))
            .map(|g| g.trim().to_owned())
    };
    let ours_meta;
    let ours = match (
        path.ends_with(".meta") && base.trim().is_empty(),
        guid_of(ours),
        guid_of(theirs),
    ) {
        (true, Some(mine), Some(shared)) if mine != shared => {
            ours_meta = ours.replacen(&format!("guid: {mine}"), &format!("guid: {shared}"), 1);
            ours_meta.as_str()
        }
        _ => ours,
    };
    let guids = crate::semantic::guid_index(repo);
    // Fields renamed with [FormerlySerializedAs] on either side: the old
    // key is written under its new name in all three versions (§13.10).
    let renamed = if format == Format::Unity
        && [base, ours, theirs]
            .iter()
            .any(|t| t.contains("MonoBehaviour"))
    {
        let renames = field_renames(repo, &guids, [base, ours, theirs]);
        let fix = |text: &str| -> Option<String> {
            let mut file = unity_yaml::parse(text).ok()?;
            crate::renames::apply(&mut file, &renames).then(|| file.to_text())
        };
        [fix(base), fix(ours), fix(theirs)]
    } else {
        [None, None, None]
    };
    let base = renamed[0].as_deref().unwrap_or(base);
    let ours = renamed[1].as_deref().unwrap_or(ours);
    let theirs = renamed[2].as_deref().unwrap_or(theirs);
    let outcome = merge_texts(
        format,
        base,
        ours,
        theirs,
        rules,
        &BTreeMap::new(),
        Some(&guids),
        theirs_name,
    )?;
    if outcome.questions.is_empty() {
        let _ = clear(repo, path);
        return Some((outcome.text, false));
    }
    let mut questions = outcome.questions;
    if path.rsplit('/').next() == Some("TagManager.asset") {
        for question in &mut questions {
            explain_layer_clash(repo, question, theirs_name);
        }
    }
    if matches!(
        path.rsplit('/').next(),
        Some("TagManager.asset" | "NavMeshAreas.asset")
    ) {
        for question in &mut questions {
            explain_table_clash(question, theirs_name);
        }
    }
    let dir = dir(repo, path);
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::write(dir.join("base"), base);
        let _ = fs::write(dir.join("ours"), ours);
        let _ = fs::write(dir.join("theirs"), theirs);
        let _ = save(
            repo,
            &Pending {
                path: path.to_owned(),
                format,
                theirs_name: theirs_name.to_owned(),
                questions,
                answers: BTreeMap::new(),
            },
        );
    }
    Some((outcome.text, true))
}

/// Renames declared in the scripts of the MonoBehaviours in `texts`, by
/// script GUID, read from both sides of the operation and the working tree.
fn field_renames(
    repo: &Repo,
    guids: &unity_yaml::GuidIndex,
    texts: [&str; 3],
) -> BTreeMap<String, Vec<(String, String)>> {
    let mut scripts: Vec<String> = Vec::new();
    for text in texts {
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("m_Script:") else {
                continue;
            };
            if let Some(guid) = rest
                .split("guid:")
                .nth(1)
                .and_then(|g| g.split([',', '}']).next())
                .map(|g| g.trim().to_owned())
            {
                if !scripts.contains(&guid) {
                    scripts.push(guid);
                }
            }
        }
    }
    // Both sides' commits: HEAD, the heads git names in GITHEAD_<sha> while
    // it merges, and the operation's own head files.
    let mut sides: Vec<String> = vec!["HEAD".to_owned()];
    for (key, _) in std::env::vars() {
        if let Some(sha) = key.strip_prefix("GITHEAD_") {
            sides.push(sha.to_owned());
        }
    }
    for head in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
    ] {
        if repo.git_dir().join(head).exists() {
            sides.push(head.to_owned());
        }
    }
    if let Some(onto) = repo.rebase_onto() {
        sides.push(onto);
    }
    let mut renames = BTreeMap::new();
    for guid in scripts {
        let Some(path) = guids.path(&guid) else {
            continue;
        };
        if !path.ends_with(".cs") {
            continue;
        }
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut versions: Vec<String> = sides
            .iter()
            .filter_map(|side| {
                crate::semantic::blob(repo, &format!("{side}:{path}"))
                    .ok()
                    .flatten()
            })
            .collect();
        versions.extend(crate::semantic::worktree(repo, path));
        for source in versions {
            for pair in crate::renames::declared(&source) {
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
        }
        if !pairs.is_empty() {
            renames.insert(guid, pairs);
        }
    }
    renames
}

/// The layer index of a TagManager `layers` conflict (`…/layers/[8]`).
pub fn layer_index(question: &Question) -> Option<u32> {
    let mut parts = question.id.rsplit('/');
    let index = parts.next()?.strip_prefix('[')?.strip_suffix(']')?;
    if parts.next()? != "layers" {
        return None;
    }
    index.parse().ok()
}

/// Both sides gave a new layer the same number (DESIGN.md §14.2). The layer
/// pass moves ours before merging; when it did not run (plain git), a
/// plain "which name?" would silently move the other side's objects onto the
/// chosen layer, so the question says so and points to `gang sync`.
fn explain_layer_clash(repo: &Repo, question: &mut Question, theirs_name: &str) {
    let Some(index) = layer_index(question) else {
        return;
    };
    let filled = |v: &Option<String>| v.as_deref().is_some_and(|v| !v.trim().is_empty());
    if filled(&question.base) || !filled(&question.ours) || !filled(&question.theirs) {
        // One layer renamed twice: the name is all there is to choose.
        return;
    }
    let git_dir = repo.git_dir();
    let operation = if repo.rebasing() {
        "rebase"
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        "cherry-pick"
    } else if git_dir.join("REVERT_HEAD").exists() {
        "revert"
    } else {
        "merge"
    };
    let (ours, theirs) = (
        question.ours.as_deref().unwrap_or_default().trim(),
        question.theirs.as_deref().unwrap_or_default().trim(),
    );
    question.why = format!(
        "Layer {index} is a new layer on both sides: {ours:?} for you, {theirs:?} for \
         {theirs_name}. Objects on layer {index} mean {ours:?} in your work and {theirs:?} in \
         theirs, so whichever name you pick, the other side's objects silently change layer. \
         Better: `gang {operation} --abort`, then `gang sync`, which moves your layer to a free \
         number together with everything that uses it."
    );
}

/// A rendering layer or NavMesh area both sides added at the same number
/// (HARDENING L9). GANG reserves these numbers (§14.6) but does not move
/// them at merge time; the question says what picking a name means.
fn explain_table_clash(question: &mut Question, theirs_name: &str) {
    let mut parts = question.id.split('/').rev();
    let (index, table) = match (parts.next(), parts.next(), parts.next()) {
        (Some(i), Some("m_RenderingLayers"), _) => (i, "rendering layer"),
        (Some("name"), Some(i), Some("areas")) => (i, "NavMesh area"),
        _ => return,
    };
    let Some(index) = index
        .strip_prefix('[')
        .and_then(|i| i.strip_suffix(']'))
        .and_then(|i| i.parse::<u32>().ok())
    else {
        return;
    };
    let filled = |v: &Option<String>| v.as_deref().is_some_and(|v| !v.trim().is_empty());
    if filled(&question.base) || !filled(&question.ours) || !filled(&question.theirs) {
        return;
    }
    let (ours, theirs) = (
        question.ours.as_deref().unwrap_or_default().trim(),
        question.theirs.as_deref().unwrap_or_default().trim(),
    );
    let where_ = if table == "NavMesh area" {
        "the Navigation window's Areas tab, then the NavMesh modifiers, links and agents that use it"
    } else {
        "Project Settings › Tags and Layers, then the renderers' Rendering Layer Mask and the lights that use it"
    };
    question.why = format!(
        "{} {index} is new on both sides: {ours:?} for you, {theirs:?} for {theirs_name}.          Whichever name you pick, the other side's work that uses number {index} changes meaning.          Pick the name that keeps {index}, then in Unity give the other one a free number in {where_}.",
        if table == "NavMesh area" {
            "NavMesh area"
        } else {
            "Rendering layer"
        }
    );
}

/// Re-runs a pending merge with its answers.
pub fn apply(repo: &Repo, pending: &Pending, rules: &Rules) -> Result<String> {
    let (base, ours, theirs) = versions(repo, &pending.path)?;
    let guids = crate::semantic::guid_index(repo);
    let outcome = merge_texts(
        pending.format,
        &base,
        &ours,
        &theirs,
        rules,
        &pending.answers,
        Some(&guids),
        &pending.theirs_name,
    )
    .context("the stored versions no longer parse")?;
    Ok(outcome.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn questions(path: &str, base: &str, ours: &str, theirs: &str) -> Vec<Question> {
        let outcome = merge_texts(
            Format::Unity,
            base,
            ours,
            theirs,
            &Rules::default(),
            &BTreeMap::new(),
            None,
            "ayberk",
        )
        .unwrap();
        let mut questions = outcome.questions;
        for q in &mut questions {
            if path.ends_with("TagManager.asset") || path.ends_with("NavMeshAreas.asset") {
                explain_table_clash(q, "ayberk");
            }
        }
        questions
    }

    #[test]
    fn navmesh_area_clashes_are_explained() {
        let areas = |name: &str| {
            format!(
                "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!126 &1\nNavMeshProjectSettings:\n  serializedVersion: 2\n  areas:\n  - name: Walkable\n    cost: 1\n  - name: Not Walkable\n    cost: 1\n  - name: Jump\n    cost: 2\n  - name: {name}\n    cost: 1\n"
            )
        };
        let qs = questions(
            "ProjectSettings/NavMeshAreas.asset",
            &areas(""),
            &areas("Water"),
            &areas("Mud"),
        );
        assert_eq!(qs.len(), 1, "{qs:?}");
        assert!(
            qs[0].why.starts_with(
                "NavMesh area 3 is new on both sides: \"Water\" for you, \"Mud\" for ayberk."
            ),
            "{} / {}",
            qs[0].id,
            qs[0].why
        );
    }

    #[test]
    fn rendering_layer_clashes_are_explained() {
        let tags = |name: &str| {
            format!(
                "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!78 &1\nTagManager:\n  serializedVersion: 3\n  tags: []\n  layers:\n  - Default\n  m_SortingLayers:\n  - name: Default\n    uniqueID: 0\n    locked: 0\n  m_RenderingLayers:\n  - Default\n  - {name}\n"
            )
        };
        let qs = questions(
            "ProjectSettings/TagManager.asset",
            &tags(""),
            &tags("Glow"),
            &tags("Outline"),
        );
        assert_eq!(qs.len(), 1, "{qs:?}");
        assert!(
            qs[0]
                .why
                .starts_with("Rendering layer 1 is new on both sides"),
            "{} / {}",
            qs[0].id,
            qs[0].why
        );
    }
}
