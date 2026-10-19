//! Automatic layer remap (DESIGN.md §14.2): when two people gave the same
//! layer number different names, ours moves to a free layer, together with
//! everything our own commits used it for, in a commit of its own.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use unity_yaml::{File, Item, Map, Scalar, Value};

use crate::csharp;
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
    /// The other side already moved this layer; our commits' new uses of
    /// the old number follow it. Our TagManager is left to the merge.
    pub follow: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub moves: Vec<Move>,
    /// `path: what changed`
    pub updated: Vec<String>,
    /// Places a person should look at.
    pub review: Vec<String>,
    /// Our layers that moved in a TagManager, for a rebase to replay.
    pub renamed: Vec<Renamed>,
}

/// One of our layers moved to a free number in a TagManager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renamed {
    pub path: String,
    pub name: String,
    pub from: u32,
    pub to: u32,
    /// What layer `from` was in the merge base.
    pub previous: String,
}

/// The moves made before a rebase onto `onto` (`.git/gang/rebase-layers.json`).
#[derive(Debug, Serialize, Deserialize)]
struct Replay {
    onto: String,
    moves: Vec<Renamed>,
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
            // Their side moved a layer we both had (a remap like this one,
            // done by a teammate): our new uses of the old number follow it.
            if !bi.is_empty() && oi == bi && ti != bi {
                if let Some(j) = (FIRST_USER_LAYER..LAYER_COUNT)
                    .find(|&j| j != i && get(&t, j) == bi && get(&b, j) != bi)
                {
                    moves.push(Move {
                        name: bi.clone(),
                        from: i as u32,
                        to: j as u32,
                        taken_by: format!(
                            "{bi:?} moved from layer {i} to {j} on {upstream} ({who})"
                        ),
                        follow: true,
                    });
                }
                continue;
            }
            // Only a layer that is new on our side can clash. A layer the base
            // already had and both renamed is one layer with two names: the
            // merge asks which name, and nothing moves.
            if oi.is_empty() || oi == bi || !bi.is_empty() {
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
                    follow: false,
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
                        follow: false,
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
    apply_in(repo, base, Some("HEAD"), project, moves)
}

/// Like [`apply`], for work that is not committed yet: everything that
/// differs between `base` and the working tree, new files included.
pub fn apply_to_worktree(
    repo: &Repo,
    base: &str,
    project: &UnityProject,
    moves: &[Move],
) -> Result<Report> {
    apply_in(repo, base, None, project, moves)
}

fn apply_in(
    repo: &Repo,
    base: &str,
    head: Option<&str>,
    project: &UnityProject,
    moves: &[Move],
) -> Result<Report> {
    let mut report = Report {
        moves: moves.to_vec(),
        ..Report::default()
    };

    // TagManager (layers the other side moved already are its business).
    let tags_path = tag_manager(project);
    let own: Vec<&Move> = moves.iter().filter(|m| !m.follow).collect();
    if own.is_empty() {
        // Nothing to rename here.
    } else if let Some(text) = semantic::worktree(repo, &tags_path) {
        if let Ok(mut file) = unity_yaml::parse(&text) {
            let base_names = semantic::blob(repo, &format!("{base}:{tags_path}"))?
                .and_then(|t| layer_names(&t))
                .unwrap_or_default();
            if let Some(items) = file.docs[0]
                .body_mut()
                .and_then(|b| b.get_mut("layers"))
                .and_then(Value::as_seq_mut)
            {
                for m in &own {
                    let previous = base_names.get(m.from as usize).cloned().unwrap_or_default();
                    set_layer(items, m.from as usize, &previous);
                    set_layer(items, m.to as usize, &m.name);
                    report.renamed.push(Renamed {
                        path: tags_path.clone(),
                        name: m.name.clone(),
                        from: m.from,
                        to: m.to,
                        previous,
                    });
                }
            }
            std::fs::write(repo.root().join(&tags_path), file.to_text())?;
            let what = own
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
    let mut changed = match head {
        Some(head) => {
            split_nul(&repo.run(["diff", "--name-only", "-z", base, head, "--", &scope])?)
        }
        None => split_nul(&repo.run(["diff", "--name-only", "-z", base, "--", &scope])?),
    };
    let untracked: BTreeSet<String> = match head {
        Some(_) => BTreeSet::new(),
        None => split_nul(&repo.run([
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            &scope,
        ])?)
        .into_iter()
        .collect(),
    };
    changed.extend(untracked.iter().cloned());
    // The compiler's view of the scripts: which numbers really reach a
    // layer, and which serialized fields hold one (None without Unity).
    let needs_code = changed
        .iter()
        .any(|p| p.ends_with(".cs") || attributes::is_unity_yaml(p));
    let analysis = if needs_code {
        csharp::analyze(repo, project)
    } else {
        None
    };
    let field_roles = analysis.as_ref().map(csharp::Analysis::field_roles);
    let scripts = analysis.as_ref().map(|_| semantic::guid_index(repo));
    let pairs: Vec<(u32, u32)> = moves.iter().map(|m| (m.from, m.to)).collect();
    for path in &changed {
        let path = path.clone();
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
            let fields = |guid: &str| -> Option<BTreeMap<String, csharp::Role>> {
                let roles = field_roles.as_ref()?;
                let script = scripts.as_ref()?.path(guid)?;
                let class = script.rsplit('/').next()?.strip_suffix(".cs")?;
                Some(
                    roles
                        .iter()
                        .filter(|((c, _), _)| c == class)
                        .map(|((_, f), r)| (f.clone(), *r))
                        .collect(),
                )
            };
            for m in moves {
                remap_file(
                    &mut file,
                    base_file.as_ref(),
                    m.from,
                    m.to,
                    &fields,
                    &mut counts,
                );
            }
            if counts.unclear_masks > 0 {
                report.review.push(format!(
                    "{path}: {} layer mask{} already covered layer {} as well; left as is, check {}",
                    counts.unclear_masks,
                    if counts.unclear_masks == 1 { "" } else { "s" },
                    m_to(moves),
                    if counts.unclear_masks == 1 { "it" } else { "them" }
                ));
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
            let added = if untracked.contains(&path) {
                (1..=source.lines().count()).collect()
            } else {
                added_lines(repo, base, head, &path)?
            };
            let mut text = source.clone();
            if let Some(analysis) = &analysis {
                // What the compiler traced is rewritten; the text rules only
                // add a second opinion for review (code the compiler did not
                // see, such as inactive #if branches).
                let uses: Vec<&csharp::Use> = analysis.uses_in(&path).collect();
                let result = csharp::rewrite(&text, &uses, &added, &pairs);
                for line in &result.changed {
                    report.updated.push(format!("{path}:{line}"));
                }
                for (line, why) in &result.review {
                    report.review.push(format!("{path}:{line}: {why}"));
                }
                text = result.text;
                let traced: BTreeSet<usize> = uses.iter().map(|u| u.line).collect();
                // The text rules still cover what the compiler did not trace:
                // declarations nothing uses yet (`enum Layers { UI3D = 8 }`),
                // code in inactive #if branches, and anything a compile with
                // missing references lost. The compiler only ever adds.
                for m in moves {
                    let second = gang_csharp::rewrite_layer_number(&text, &added, m.from, m.to);
                    for line in second.changed.iter().filter(|l| !traced.contains(l)) {
                        report.updated.push(format!("{path}:{line}"));
                        report.review.push(format!(
                            "{path}:{line}: changed because it reads like layer {}; no compiled code uses it as a layer yet",
                            m.from
                        ));
                    }
                    for review in second.review.iter().filter(|r| !traced.contains(&r.line)) {
                        report
                            .review
                            .push(format!("{path}:{}: {}", review.line, review.message));
                    }
                    text = second.text;
                }
            } else {
                for m in moves {
                    let result = gang_csharp::rewrite_layer_number(&text, &added, m.from, m.to);
                    for line in &result.changed {
                        report.updated.push(format!("{path}:{line}"));
                    }
                    for review in &result.review {
                        report
                            .review
                            .push(format!("{path}:{}: {}", review.line, review.message));
                    }
                    text = result.text;
                }
            }
            if text != source {
                std::fs::write(repo.root().join(&path), text)?;
            }
        }
    }
    // Scripts our work did not touch that give a moved number to Unity.
    if let Some(analysis) = &analysis {
        let mut others: BTreeMap<&str, Vec<&csharp::Use>> = BTreeMap::new();
        for u in &analysis.uses {
            if !changed.iter().any(|c| c == &u.file) {
                others.entry(u.file.as_str()).or_default().push(u);
            }
        }
        for (file, uses) in others {
            let Some(text) = semantic::worktree(repo, file) else {
                continue;
            };
            let result = csharp::rewrite(&text, &uses, &BTreeSet::new(), &pairs);
            for (line, why) in result.review {
                report.review.push(format!("{file}:{line}: {why}"));
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

/// Lines (1-based, in the current version) our commits (or, without
/// `head`, our work in the working tree) added to a file.
fn added_lines(repo: &Repo, base: &str, head: Option<&str>, path: &str) -> Result<BTreeSet<usize>> {
    let diff = match head {
        Some(head) => repo.run(["diff", "-U0", base, head, "--", path])?,
        None => repo.run(["diff", "-U0", base, "--", path])?,
    };
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
    /// Script fields the compiler knows hold a layer or a mask.
    fields: usize,
    objects: usize,
    masks: usize,
    overrides: usize,
    heuristic: Vec<String>,
    /// Masks that already covered the target layer: left alone.
    unclear_masks: usize,
}

impl Counts {
    fn any(&self) -> bool {
        self.fields + self.objects + self.masks + self.overrides + self.heuristic.len() > 0
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
        if self.fields > 0 {
            parts.push(format!(
                "{} layer field{} in scripts",
                self.fields,
                if self.fields == 1 { "" } else { "s" }
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

/// `fields` gives, for a script's GUID, its serialized fields that hold a
/// layer or a mask (from the compiler); `None` falls back to the field's name.
fn remap_file(
    file: &mut File,
    base: Option<&File>,
    from: u32,
    to: u32,
    fields: &dyn Fn(&str) -> Option<BTreeMap<String, csharp::Role>>,
    counts: &mut Counts,
) {
    for doc in &mut file.docs {
        let (Some(id), Some(class)) = (doc.file_id, doc.class_id) else {
            continue;
        };
        let type_name = doc.type_name().unwrap_or("").to_owned();
        let base_body = base.and_then(|b| b.doc(id)).and_then(|d| d.body()).cloned();
        let Some(body) = doc.body_mut() else { continue };
        let script_fields = body
            .get("m_Script")
            .and_then(Value::as_ref)
            .and_then(|r| r.guid)
            .and_then(|guid| fields(&guid));
        remap_map(
            body,
            base_body.as_ref(),
            class,
            &type_name,
            script_fields.as_ref(),
            from,
            to,
            counts,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn remap_map(
    map: &mut Map,
    base: Option<&Map>,
    class: u32,
    type_name: &str,
    script_fields: Option<&BTreeMap<String, csharp::Role>>,
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
                    } else if covers_both(&value, from, to) {
                        counts.unclear_masks += 1;
                    }
                } else if type_name == "MonoBehaviour" {
                    match script_fields.map(|f| f.get(&key)) {
                        // The compiler says what the field holds.
                        Some(Some(csharp::Role::Index)) if value == from.to_string() => {
                            map.set(&key, Value::Scalar(Scalar::raw(to.to_string())));
                            counts.fields += 1;
                        }
                        Some(Some(csharp::Role::Mask)) => {
                            if let Some(bits) = move_bit(&value, from, to) {
                                map.set(&key, Value::Scalar(Scalar::raw(bits)));
                                counts.fields += 1;
                            } else if covers_both(&value, from, to) {
                                counts.unclear_masks += 1;
                            }
                        }
                        Some(_) => {}
                        // Without the compiler: a field named like a layer.
                        None if key.to_ascii_lowercase().contains("layer")
                            && value == from.to_string() =>
                        {
                            map.set(&key, Value::Scalar(Scalar::raw(to.to_string())));
                            counts.heuristic.push(key.clone());
                        }
                        None => {}
                    }
                }
            }
            Some(Value::Map(_)) => {
                let base_map = base_value.as_ref().and_then(Value::as_map).cloned();
                if let Some(child) = map.get_mut(&key).and_then(Value::as_map_mut) {
                    remap_map(
                        child,
                        base_map.as_ref(),
                        class,
                        type_name,
                        None,
                        from,
                        to,
                        counts,
                    );
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

/// Moves bit `from` to bit `to` in a LayerMask value. Leaves alone masks
/// without the bit, and masks that already have bit `to` ("Everything", or
/// "everything except …"): clearing `from` there would drop the other side's
/// layer, keeping it might include it by accident, so a person decides.
fn move_bit(value: &str, from: u32, to: u32) -> Option<String> {
    let bits: u64 = value.trim().parse().ok()?;
    let bits = bits as u32;
    if bits & (1 << from) == 0 || bits & (1 << to) != 0 {
        return None;
    }
    Some(((bits & !(1 << from)) | (1 << to)).to_string())
}

fn covers_both(value: &str, from: u32, to: u32) -> bool {
    let bits = value.trim().parse::<u64>().map(|b| b as u32).unwrap_or(0);
    bits != u32::MAX && bits & (1 << from) != 0 && bits & (1 << to) != 0
}

fn m_to(moves: &[Move]) -> String {
    moves
        .iter()
        .map(|m| m.to.to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
        total.renamed.extend(report.renamed);
    }
    if repo
        .run(["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty()
    {
        // A teammate's move that none of our commits used: nothing to do.
        return Ok(None);
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
        if m.follow {
            message.push_str(&format!(
                "{}, so our new uses of layer {} moved with it.\n",
                m.taken_by, m.from
            ));
        } else {
            message.push_str(&format!(
                "{}, so {:?} moved to layer {}.\n",
                m.taken_by, m.name, m.to
            ));
        }
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

fn replay_path(repo: &Repo) -> std::path::PathBuf {
    repo.gang_dir().join("rebase-layers.json")
}

/// Before a rebase onto `onto`: remembers the layers the pass moved.
///
/// A rebase replays our commits one by one, and the remap commit comes
/// last; every earlier commit that added the layer would clash with the
/// upstream name again. The merge driver replays those commits with the
/// layer already at its new number (`replay_moves`), so the TagManager
/// merges cleanly; the objects follow in the remap commit.
pub fn remember_for_rebase(repo: &Repo, onto: &str, report: &Report) -> Result<()> {
    forget_rebase(repo);
    if report.renamed.is_empty() {
        return Ok(());
    }
    let onto = repo.run(["rev-parse", &format!("{onto}^{{commit}}")])?;
    let replay = Replay {
        onto: onto.trim().to_owned(),
        moves: report.renamed.clone(),
    };
    std::fs::create_dir_all(repo.gang_dir())?;
    std::fs::write(replay_path(repo), serde_json::to_string_pretty(&replay)?)?;
    Ok(())
}

pub fn forget_rebase(repo: &Repo) {
    let _ = std::fs::remove_file(replay_path(repo));
}

/// During a rebase the pass prepared: `text` (a version of the TagManager
/// at `path` from our side) with our moved layers at their new numbers.
/// `None` when nothing applies.
pub fn replay_moves(repo: &Repo, path: &str, text: &str) -> Option<String> {
    let replay: Replay =
        serde_json::from_str(&std::fs::read_to_string(replay_path(repo)).ok()?).ok()?;
    if repo.rebase_onto()? != replay.onto {
        return None;
    }
    let moves: Vec<&Renamed> = replay.moves.iter().filter(|m| m.path == path).collect();
    if moves.is_empty() {
        return None;
    }
    let names = layer_names(text)?;
    let mut file = unity_yaml::parse(text).ok()?;
    let items = file.docs[0]
        .body_mut()
        .and_then(|b| b.get_mut("layers"))
        .and_then(Value::as_seq_mut)?;
    let mut changed = false;
    for m in moves {
        if names.get(m.from as usize) == Some(&m.name) {
            set_layer(items, m.from as usize, &m.previous);
            set_layer(items, m.to as usize, &m.name);
            changed = true;
        }
    }
    changed.then(|| file.to_text())
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
        // Everything except layer 5: bit 9 is already set.
        let all_but_5 = (!(1u32 << 5)).to_string();
        assert_eq!(move_bit(&all_but_5, 8, 9), None);
        assert!(covers_both(&all_but_5, 8, 9));
        assert!(!covers_both("4294967295", 8, 9));
    }

    #[test]
    fn names() {
        let text = "%YAML 1.1\n--- !u!78 &1\nTagManager:\n  tags: []\n  layers:\n  - Default\n  - \n  - UI\n";
        assert_eq!(layer_names(text).unwrap(), vec!["Default", "", "UI"]);
    }
}
