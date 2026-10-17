//! Automatic layer remap (DESIGN.md §14.2): when two people gave the same
//! layer number different names, ours moves to a free layer, together with
//! everything our own commits used it for, in a commit of its own.

use std::collections::BTreeSet;

use anyhow::Result;
use unity_yaml::{File, Item, Map, Scalar, Value};

use crate::repo::{Repo, split_nul};
use crate::unity::{self, UnityProject};
use crate::{attributes, semantic};

const FIRST_USER_LAYER: usize = 8;
const LAYER_COUNT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub name: String,
    pub from: u32,
    pub to: u32,
    /// What took the layer: `"Pickup" on origin/main (ayberk)`.
    pub taken_by: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub moves: Vec<Move>,
    /// `path: what changed`
    pub updated: Vec<String>,
    /// Places a person should look at.
    pub review: Vec<String>,
}

fn tag_manager(project: &UnityProject) -> String {
    project.join("ProjectSettings/TagManager.asset")
}

/// The layer names in a TagManager text (32 entries, "" for unused).
pub fn layer_names(text: &str) -> Option<Vec<String>> {
    let file = unity_yaml::parse(text).ok()?;
    let items = file.docs.first()?.body()?.get("layers")?.as_seq()?;
    Some(
        items
            .iter()
            .map(|i| i.value().text().unwrap_or_default())
            .collect(),
    )
}

/// The layer moves a merge of `upstream` into `HEAD` needs, per project.
pub fn plan(repo: &Repo, base: &str, upstream: &str) -> Result<Vec<(UnityProject, Vec<Move>)>> {
    let mut plans = Vec::new();
    for project in unity::find_projects(repo.root()) {
        let path = tag_manager(&project);
        let read = |rev: &str| -> Option<Vec<String>> {
            semantic::blob(repo, &format!("{rev}:{path}"))
                .ok()
                .flatten()
                .and_then(|t| layer_names(&t))
        };
        let (Some(b), Some(o), Some(t)) = (read(base), read("HEAD"), read(upstream)) else {
            continue;
        };
        let get = |list: &[String], i: usize| list.get(i).cloned().unwrap_or_default();
        let who = repo
            .try_run([
                "log",
                "-1",
                "--format=%an",
                &format!("{base}..{upstream}"),
                "--",
                &path,
            ])?
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "a teammate".to_owned());

        let mut moves = Vec::new();
        let mut taken: BTreeSet<usize> = BTreeSet::new();
        for i in FIRST_USER_LAYER..LAYER_COUNT {
            let (bi, oi, ti) = (get(&b, i), get(&o, i), get(&t, i));
            if oi.is_empty() || oi == bi {
                continue;
            }
            // The same name already at another index on their side: use theirs.
            if let Some(j) = (FIRST_USER_LAYER..LAYER_COUNT)
                .find(|&j| j != i && get(&t, j) == oi && get(&b, j) != oi)
            {
                moves.push(Move {
                    name: oi.clone(),
                    from: i as u32,
                    to: j as u32,
                    taken_by: format!("{oi:?} is already layer {j} on {upstream} ({who})"),
                });
                continue;
            }
            if ti != bi && !ti.is_empty() && ti != oi {
                let free = (FIRST_USER_LAYER..LAYER_COUNT).find(|&k| {
                    get(&b, k).is_empty()
                        && get(&o, k).is_empty()
                        && get(&t, k).is_empty()
                        && !taken.contains(&k)
                });
                if let Some(k) = free {
                    taken.insert(k);
                    moves.push(Move {
                        name: oi.clone(),
                        from: i as u32,
                        to: k as u32,
                        taken_by: format!("Layer {i} is {ti:?} on {upstream} ({who})"),
                    });
                }
            }
        }
        if !moves.is_empty() {
            plans.push((project, moves));
        }
    }
    Ok(plans)
}

/// Rewrites the working tree for the moves: TagManager, objects and masks in
/// the Unity files our commits changed, and recognizable layer numbers in
/// the lines our commits added to scripts.
pub fn apply(repo: &Repo, base: &str, project: &UnityProject, moves: &[Move]) -> Result<Report> {
    let mut report = Report {
        moves: moves.to_vec(),
        ..Report::default()
    };

    // TagManager.
    let tags_path = tag_manager(project);
    if let Some(text) = semantic::worktree(repo, &tags_path) {
        if let Ok(mut file) = unity_yaml::parse(&text) {
            let base_names = semantic::blob(repo, &format!("{base}:{tags_path}"))?
                .and_then(|t| layer_names(&t))
                .unwrap_or_default();
            if let Some(items) = file.docs[0]
                .body_mut()
                .and_then(|b| b.get_mut("layers"))
                .and_then(Value::as_seq_mut)
            {
                for m in moves {
                    let previous = base_names.get(m.from as usize).cloned().unwrap_or_default();
                    set_layer(items, m.from as usize, &previous);
                    set_layer(items, m.to as usize, &m.name);
                }
            }
            std::fs::write(repo.root().join(&tags_path), file.to_text())?;
            let what = moves
                .iter()
                .map(|m| format!("layer {} → {}", m.from, m.to))
                .collect::<Vec<_>>()
                .join(", ");
            report.updated.push(format!("{tags_path}: {what}"));
        }
    }

    let scope = if project.dir.is_empty() {
        ".".to_owned()
    } else {
        project.dir.clone()
    };
    let changed =
        split_nul(&repo.run(["diff", "--name-only", "-z", base, "HEAD", "--", &scope])?);
    for path in changed {
        if path == tags_path {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(&path);
        if name == "DynamicsManager.asset" || name == "Physics2DSettings.asset" {
            report.review.push(format!(
                "{path}: check the collision matrix for the moved layer"
            ));
            continue;
        }
        if attributes::is_unity_yaml(&path) {
            let Some(text) = semantic::worktree(repo, &path) else {
                continue;
            };
            let Ok(mut file) = unity_yaml::parse(&text) else {
                continue;
            };
            let base_file = semantic::blob(repo, &format!("{base}:{path}"))?
                .and_then(|t| unity_yaml::parse(&t).ok());
            let mut counts = Counts::default();
            for m in moves {
                remap_file(&mut file, base_file.as_ref(), m.from, m.to, &mut counts);
            }
            if counts.any() {
                std::fs::write(repo.root().join(&path), file.to_text())?;
                report
                    .updated
                    .push(format!("{path}: {}", counts.describe()));
                for field in counts.heuristic {
                    report.review.push(format!(
                        "{path}: script field {field} looked like a layer and was changed"
                    ));
                }
            }
        } else if path.ends_with(".cs") {
            let Some(source) = semantic::worktree(repo, &path) else {
                continue;
            };
            let added = added_lines(repo, base, &path)?;
            let mut text = source.clone();
            for m in moves {
                let result = gang_csharp::rewrite_layer_number(&text, &added, m.from, m.to);
                for line in &result.changed {
                    report.updated.push(format!("{path}:{line}"));
                }
                for line in &result.review {
                    report.review.push(format!(
                        "{path}:{line}: the number {} next to \"layer\"",
                        m.from
                    ));
                }
                text = result.text;
            }
            if text != source {
                std::fs::write(repo.root().join(&path), text)?;
            }
        }
    }
    Ok(report)
}

fn set_layer(items: &mut Vec<Item>, index: usize, name: &str) {
    while items.len() <= index {
        items.push(Item::new(Value::Scalar(Scalar::raw(""))));
    }
    *items[index].value_mut() = Value::Scalar(if name.is_empty() {
        Scalar::raw("")
    } else {
        Scalar::plain(name)
    });
}

/// Lines (1-based, in the current version) our commits added to a file.
fn added_lines(repo: &Repo, base: &str, path: &str) -> Result<BTreeSet<usize>> {
    let diff = repo.run(["diff", "-U0", base, "HEAD", "--", path])?;
    let mut lines = BTreeSet::new();
    for line in diff.lines().filter(|l| l.starts_with("@@")) {
        // @@ -a,b +c,d @@
        let Some(plus) = line.split_whitespace().find(|w| w.starts_with('+')) else {
            continue;
        };
        let mut parts = plus[1..].split(',');
        let start: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let count: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        lines.extend(start..start + count);
    }
    Ok(lines)
}

#[derive(Default)]
struct Counts {
    objects: usize,
    masks: usize,
    overrides: usize,
    heuristic: Vec<String>,
}

impl Counts {
    fn any(&self) -> bool {
        self.objects + self.masks + self.overrides + self.heuristic.len() > 0
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.objects > 0 {
            parts.push(format!(
                "{} object{} (Layer)",
                self.objects,
                if self.objects == 1 { "" } else { "s" }
            ));
        }
        if self.masks > 0 {
            parts.push(format!(
                "{} layer mask{}",
                self.masks,
                if self.masks == 1 { "" } else { "s" }
            ));
        }
        if self.overrides > 0 {
            parts.push(format!(
                "{} prefab override{}",
                self.overrides,
                if self.overrides == 1 { "" } else { "s" }
            ));
        }
        if !self.heuristic.is_empty() {
            parts.push(format!(
                "{} script field{}",
                self.heuristic.len(),
                if self.heuristic.len() == 1 { "" } else { "s" }
            ));
        }
        parts.join(", ")
    }
}

fn remap_file(file: &mut File, base: Option<&File>, from: u32, to: u32, counts: &mut Counts) {
    for doc in &mut file.docs {
        let (Some(id), Some(class)) = (doc.file_id, doc.class_id) else {
            continue;
        };
        let type_name = doc.type_name().unwrap_or("").to_owned();
        let base_body = base.and_then(|b| b.doc(id)).and_then(|d| d.body()).cloned();
        let Some(body) = doc.body_mut() else { continue };
        remap_map(
            body,
            base_body.as_ref(),
            class,
            &type_name,
            from,
            to,
            counts,
        );
    }
}

fn remap_map(
    map: &mut Map,
    base: Option<&Map>,
    class: u32,
    type_name: &str,
    from: u32,
    to: u32,
    counts: &mut Counts,
) {
    let keys: Vec<String> = map.entries().iter().map(|e| e.key.clone()).collect();
    for key in keys {
        let base_value = base.and_then(|b| b.get(&key)).cloned();
        let unchanged = base_value.as_ref() == map.get(&key);
        if unchanged {
            continue;
        }
        let current = map.get(&key).cloned();
        match current {
            Some(Value::Scalar(s)) => {
                let value = s.value();
                if key == "m_Layer" && class == 1 && value == from.to_string() {
                    map.set(&key, Value::Scalar(Scalar::raw(to.to_string())));
                    counts.objects += 1;
                } else if key == "m_Bits" {
                    if let Some(bits) = move_bit(&value, from, to) {
                        map.set(&key, Value::Scalar(Scalar::raw(bits)));
                        counts.masks += 1;
                    }
                } else if type_name == "MonoBehaviour"
                    && key.to_ascii_lowercase().contains("layer")
                    && value == from.to_string()
                {
                    map.set(&key, Value::Scalar(Scalar::raw(to.to_string())));
                    counts.heuristic.push(key.clone());
                }
            }
            Some(Value::Map(_)) => {
                let base_map = base_value.as_ref().and_then(Value::as_map).cloned();
                if let Some(child) = map.get_mut(&key).and_then(Value::as_map_mut) {
                    remap_map(child, base_map.as_ref(), class, type_name, from, to, counts);
                }
            }
            Some(Value::Seq(_)) if key == "m_Modifications" => {
                let base_items: Vec<String> = base_value
                    .as_ref()
                    .and_then(Value::as_seq)
                    .unwrap_or(&[])
                    .iter()
                    .map(|i| format!("{:?}", i.value()))
                    .collect();
                if let Some(items) = map.get_mut(&key).and_then(Value::as_seq_mut) {
                    for item in items.iter_mut() {
                        if base_items.contains(&format!("{:?}", item.value())) {
                            continue;
                        }
                        let Some(entry) = item.value().as_map() else {
                            continue;
                        };
                        let property = entry
                            .get("propertyPath")
                            .and_then(Value::text)
                            .unwrap_or_default();
                        let value = entry.get("value").and_then(Value::text).unwrap_or_default();
                        let new_value = if property == "m_Layer" && value == from.to_string() {
                            Some(to.to_string())
                        } else if property.ends_with("m_Bits") {
                            move_bit(&value, from, to)
                        } else {
                            None
                        };
                        if let Some(new_value) = new_value {
                            if let Some(entry) = item.value_mut().as_map_mut() {
                                entry.set("value", Value::Scalar(Scalar::raw(new_value)));
                                counts.overrides += 1;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Moves bit `from` to bit `to` in a LayerMask value. Leaves "Everything"
/// (all bits) and masks without the bit alone.
fn move_bit(value: &str, from: u32, to: u32) -> Option<String> {
    let bits: u64 = value.trim().parse().ok()?;
    let bits = bits as u32;
    if bits == u32::MAX || bits & (1 << from) == 0 {
        return None;
    }
    Some(((bits & !(1 << from)) | (1 << to)).to_string())
}

/// Plans, applies and commits the remap before merging `upstream`.
/// `None` when nothing needed to move.
pub fn run(repo: &Repo, upstream: &str) -> Result<Option<Report>> {
    let Some(base) = repo
        .try_run(["merge-base", "HEAD", upstream])?
        .map(|b| b.trim().to_owned())
    else {
        return Ok(None);
    };
    let plans = plan(repo, &base, upstream)?;
    if plans.is_empty() {
        return Ok(None);
    }
    if !repo
        .run(["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty()
    {
        anyhow::bail!(
            "two people gave the same layer number different names, and GANG needs a clean working tree \
             to move yours; commit or stash your changes, then merge again"
        );
    }
    let mut total = Report::default();
    for (project, moves) in &plans {
        let report = apply(repo, &base, project, moves)?;
        total.moves.extend(report.moves);
        total.updated.extend(report.updated);
        total.review.extend(report.review);
    }
    let mut message = String::new();
    for m in &total.moves {
        message.push_str(&format!(
            "gang: move layer {} from {} to {}\n",
            m.name, m.from, m.to
        ));
    }
    message.push('\n');
    for m in &total.moves {
        message.push_str(&format!(
            "{}, so {:?} moved to layer {}.\n",
            m.taken_by, m.name, m.to
        ));
    }
    if !total.updated.is_empty() {
        message.push_str("\nUpdated:\n");
        for line in &total.updated {
            message.push_str(&format!("- {line}\n"));
        }
    }
    if !total.review.is_empty() {
        message.push_str("\nNeeds review:\n");
        for line in &total.review {
            message.push_str(&format!("- {line}\n"));
        }
    }
    message.push('\n');
    for m in &total.moves {
        message.push_str(&format!("Gang-Remap: layer {} -> {}\n", m.from, m.to));
    }
    repo.run(["add", "-u"])?;
    repo.run(["commit", "-q", "--no-verify", "-m", &message])?;
    Ok(Some(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits() {
        assert_eq!(move_bit("256", 8, 9).as_deref(), Some("512"));
        assert_eq!(move_bit("257", 8, 9).as_deref(), Some("513"));
        assert_eq!(move_bit("4294967295", 8, 9), None);
        assert_eq!(move_bit("1", 8, 9), None);
    }

    #[test]
    fn names() {
        let text = "%YAML 1.1\n--- !u!78 &1\nTagManager:\n  tags: []\n  layers:\n  - Default\n  - \n  - UI\n";
        assert_eq!(layer_names(text).unwrap(), vec!["Default", "", "UI"]);
    }
}
