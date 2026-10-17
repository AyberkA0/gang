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
            let (b, o, t) = (
                unity_yaml::parse(base).ok()?,
                unity_yaml::parse(ours).ok()?,
                unity_yaml::parse(theirs).ok()?,
            );
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
    let guids = crate::semantic::guid_index(repo);
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
                questions: outcome.questions,
                answers: BTreeMap::new(),
            },
        );
    }
    Some((outcome.text, true))
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
