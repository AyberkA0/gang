//! Semantic diff of Unity YAML files, with noise detection (DESIGN.md §12).

use std::collections::{BTreeMap, HashSet};

use unity_yaml::{File, FileId, Hierarchy, Item, Value};

use crate::names;

/// A changed property inside one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropChange {
    /// Field path: `["m_LocalPosition", "x"]`, `["m_Children", "[2]"]`.
    pub path: Vec<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    /// A change nobody made on purpose (DESIGN.md §12.2).
    pub noise: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocChange {
    Added(FileId),
    Removed(FileId),
    Changed { id: FileId, props: Vec<PropChange> },
}

impl DocChange {
    pub fn id(&self) -> FileId {
        match self {
            DocChange::Added(id) | DocChange::Removed(id) | DocChange::Changed { id, .. } => *id,
        }
    }

    pub fn is_noise(&self) -> bool {
        matches!(self, DocChange::Changed { props, .. } if props.iter().all(|p| p.noise))
    }
}

/// Options for comparing values.
#[derive(Debug, Clone)]
pub struct Rules {
    /// Floats closer than this (relative) are equal.
    pub float_epsilon: f64,
    /// Extra `(type name, field)` pairs that are always noise.
    pub extra_noise: Vec<(String, String)>,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            float_epsilon: 1e-6,
            extra_noise: Vec::new(),
        }
    }
}

/// Fields that change by themselves (bakes, editor bookkeeping).
const ALWAYS_NOISE: &[(&str, &str)] = &[("RenderSettings", "m_IndirectSpecularColor")];

/// RectTransform fields a layout component drives.
const DRIVEN: &[&str] = &[
    "m_AnchoredPosition",
    "m_SizeDelta",
    "m_AnchorMin",
    "m_AnchorMax",
    "m_Pivot",
];

/// Compares two versions of a file document by document.
pub fn diff(old: &File, new: &File, rules: &Rules) -> Vec<DocChange> {
    let key = |doc: &unity_yaml::Doc, i: usize| doc.file_id.unwrap_or(i as FileId);
    let old_docs: BTreeMap<FileId, &unity_yaml::Doc> = old
        .docs
        .iter()
        .enumerate()
        .map(|(i, d)| (key(d, i), d))
        .collect();
    let new_docs: BTreeMap<FileId, &unity_yaml::Doc> = new
        .docs
        .iter()
        .enumerate()
        .map(|(i, d)| (key(d, i), d))
        .collect();
    let driven = driven_transforms(new);

    let mut changes = Vec::new();
    for (id, old_doc) in &old_docs {
        match new_docs.get(id) {
            None => changes.push(DocChange::Removed(*id)),
            Some(new_doc) => {
                let type_name = new_doc.type_name().unwrap_or("");
                let mut props = Vec::new();
                compare_maps(
                    &old_doc.root,
                    &new_doc.root,
                    &mut Vec::new(),
                    &mut props,
                    rules,
                );
                // The root entry is the type name; drop it from paths.
                for prop in &mut props {
                    if prop.path.first().map(String::as_str) == Some(type_name) {
                        prop.path.remove(0);
                    }
                    prop.noise = is_noise(type_name, *id, prop, rules, &driven);
                }
                if !props.is_empty() {
                    changes.push(DocChange::Changed { id: *id, props });
                }
            }
        }
    }
    for id in new_docs.keys() {
        if !old_docs.contains_key(id) {
            changes.push(DocChange::Added(*id));
        }
    }
    changes
}

/// Whether two texts differ only by noise. Files that do not parse count as
/// different unless they are byte-for-byte equal.
pub fn noise_only(old: &str, new: &str, rules: &Rules) -> bool {
    if old == new {
        return true;
    }
    match (unity_yaml::parse(old), unity_yaml::parse(new)) {
        (Ok(a), Ok(b)) => diff(&a, &b, rules).iter().all(DocChange::is_noise),
        _ => false,
    }
}

fn is_noise(
    type_name: &str,
    id: FileId,
    prop: &PropChange,
    rules: &Rules,
    driven: &HashSet<FileId>,
) -> bool {
    let field = prop.path.first().map(String::as_str).unwrap_or("");
    if ALWAYS_NOISE
        .iter()
        .any(|(t, f)| *t == type_name && *f == field)
        || rules
            .extra_noise
            .iter()
            .any(|(t, f)| t == type_name && f == field)
    {
        return true;
    }
    if type_name == "RectTransform" && DRIVEN.contains(&field) && driven.contains(&id) {
        return true;
    }
    match (&prop.old, &prop.new) {
        (Some(a), Some(b)) => floats_equal(a, b, rules.float_epsilon),
        _ => false,
    }
}

fn floats_equal(a: &str, b: &str, epsilon: f64) -> bool {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(x), Ok(y)) => (x - y).abs() <= epsilon * x.abs().max(y.abs()).max(1.0),
        _ => false,
    }
}

/// RectTransforms whose size and position a layout component sets: children
/// of a layout group, and objects with a size fitter.
fn driven_transforms(file: &File) -> HashSet<FileId> {
    let hierarchy = Hierarchy::build(file);
    let mut layout_objects = HashSet::new();
    let mut fitted_objects = HashSet::new();
    for doc in &file.docs {
        let Some(body) = doc.body() else { continue };
        let Some(guid) = body
            .get("m_Script")
            .and_then(Value::as_ref)
            .and_then(|r| r.guid)
        else {
            continue;
        };
        let Some(owner) = body
            .get("m_GameObject")
            .and_then(Value::as_ref)
            .map(|r| r.file_id)
        else {
            continue;
        };
        if names::LAYOUT_GROUPS.contains(&guid.as_str()) {
            layout_objects.insert(owner);
        }
        if names::SIZE_FITTERS.contains(&guid.as_str()) {
            fitted_objects.insert(owner);
        }
    }
    let mut driven = HashSet::new();
    for object in hierarchy.objects() {
        let parent_has_layout = object.parent.is_some_and(|p| layout_objects.contains(&p));
        if parent_has_layout || fitted_objects.contains(&object.id) {
            for component in &object.components {
                driven.insert(*component);
            }
        }
    }
    driven
}

fn compare_values(
    old: &Value,
    new: &Value,
    path: &mut Vec<String>,
    out: &mut Vec<PropChange>,
    rules: &Rules,
) {
    // `[]` written inline is the same kind of value as a block list.
    if let (Some(a), Some(b)) = (old.as_seq(), new.as_seq()) {
        if !matches!((old, new), (Value::Seq(_), Value::Seq(_))) {
            return compare_seqs(a, b, path, out, rules);
        }
    }
    match (old, new) {
        (Value::Scalar(a), Value::Scalar(b)) => {
            if a.as_raw() == b.as_raw() {
                return;
            }
            match (a.flow_entries(), b.flow_entries()) {
                (Some(fa), Some(fb)) if !fa.is_empty() || !fb.is_empty() => {
                    let fa: BTreeMap<String, String> = fa.into_iter().collect();
                    let fb: BTreeMap<String, String> = fb.into_iter().collect();
                    let is_ref = fa.contains_key("fileID") || fb.contains_key("fileID");
                    if is_ref {
                        push(path, out, Some(a.value()), Some(b.value()));
                        return;
                    }
                    for key in fa.keys().chain(fb.keys().filter(|k| !fa.contains_key(*k))) {
                        let (x, y) = (fa.get(key), fb.get(key));
                        if x != y {
                            path.push(key.clone());
                            push(path, out, x.cloned(), y.cloned());
                            path.pop();
                        }
                    }
                }
                _ => {
                    let (x, y) = (a.value(), b.value());
                    if x != y {
                        push(path, out, Some(x), Some(y));
                    }
                }
            }
        }
        (Value::Map(a), Value::Map(b)) => compare_maps(a, b, path, out, rules),
        (Value::Seq(a), Value::Seq(b)) => compare_seqs(a, b, path, out, rules),
        (Value::Empty, Value::Empty) => {}
        _ => push(path, out, Some(summary(old)), Some(summary(new))),
    }
}

fn compare_maps(
    old: &unity_yaml::Map,
    new: &unity_yaml::Map,
    path: &mut Vec<String>,
    out: &mut Vec<PropChange>,
    rules: &Rules,
) {
    for entry in old.entries() {
        path.push(entry.key.clone());
        match new.get(&entry.key) {
            Some(value) => compare_values(entry.value(), value, path, out, rules),
            None => push(path, out, Some(summary(entry.value())), None),
        }
        path.pop();
    }
    for entry in new.entries() {
        if old.get(&entry.key).is_none() {
            path.push(entry.key.clone());
            push(path, out, None, Some(summary(entry.value())));
            path.pop();
        }
    }
}

fn compare_seqs(
    old: &[Item],
    new: &[Item],
    path: &mut Vec<String>,
    out: &mut Vec<PropChange>,
    rules: &Rules,
) {
    if old.len() == new.len() {
        for (i, (a, b)) in old.iter().zip(new).enumerate() {
            path.push(format!("[{i}]"));
            compare_values(a.value(), b.value(), path, out, rules);
            path.pop();
        }
        return;
    }
    // Different lengths: report removed and added elements.
    let text = |item: &Item| summary(item.value());
    let old_texts: Vec<String> = old.iter().map(text).collect();
    let new_texts: Vec<String> = new.iter().map(text).collect();
    for (i, t) in old_texts.iter().enumerate() {
        if !new_texts.contains(t) {
            path.push(format!("[{i}]"));
            push(path, out, Some(t.clone()), None);
            path.pop();
        }
    }
    for (i, t) in new_texts.iter().enumerate() {
        if !old_texts.contains(t) {
            path.push(format!("[{i}]"));
            push(path, out, None, Some(t.clone()));
            path.pop();
        }
    }
}

fn push(path: &[String], out: &mut Vec<PropChange>, old: Option<String>, new: Option<String>) {
    out.push(PropChange {
        path: path.to_vec(),
        old,
        new,
        noise: false,
    });
}

/// A one-line summary of a value.
pub fn summary(value: &Value) -> String {
    match value {
        Value::Scalar(s) => s.value(),
        Value::Empty => String::new(),
        Value::Map(m) => {
            let parts: Vec<String> = m
                .entries()
                .iter()
                .map(|e| format!("{}: {}", e.key, summary(e.value())))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Seq(items) => format!("[{} items]", items.len()),
    }
}

/// The Inspector-style label of a property path: `["m_LocalPosition", "x"]`
/// → `Position.x`.
pub fn label(path: &[String]) -> String {
    path.iter()
        .enumerate()
        .map(|(i, part)| {
            if part.starts_with('[') {
                part.clone()
            } else if i == 0 {
                names::display(part)
            } else {
                part.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
        .replace(".[", "[")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> File {
        unity_yaml::parse(text).unwrap()
    }

    const BASE: &str = "--- !u!1 &1
GameObject:
  m_Component:
  - component: {fileID: 2}
  m_Name: Player
--- !u!4 &2
Transform:
  m_GameObject: {fileID: 1}
  m_LocalPosition: {x: 0, y: 1, z: 0}
  m_Children: []
  m_Father: {fileID: 0}
--- !u!104 &3
RenderSettings:
  m_IndirectSpecularColor: {r: 0, g: 0, b: 0, a: 1}
";

    #[test]
    fn finds_property_changes() {
        let new = BASE
            .replace("{x: 0, y: 1, z: 0}", "{x: 2, y: 1, z: 0}")
            .replace("m_Name: Player", "m_Name: Hero");
        let changes = diff(&file(BASE), &file(&new), &Rules::default());
        assert_eq!(changes.len(), 2);
        let DocChange::Changed { id, props } = &changes[0] else {
            panic!()
        };
        assert_eq!(*id, 1);
        assert_eq!(props[0].path, vec!["m_Name"]);
        assert_eq!(
            (props[0].old.as_deref(), props[0].new.as_deref()),
            (Some("Player"), Some("Hero"))
        );
        let DocChange::Changed { props, .. } = &changes[1] else {
            panic!()
        };
        assert_eq!(props[0].path, vec!["m_LocalPosition", "x"]);
        assert_eq!(label(&props[0].path), "Position.x");
    }

    #[test]
    fn added_and_removed_documents() {
        let new = format!("{BASE}--- !u!1 &9\nGameObject:\n  m_Name: New\n");
        let changes = diff(&file(BASE), &file(&new), &Rules::default());
        assert_eq!(changes, vec![DocChange::Added(9)]);
        let changes = diff(&file(&new), &file(BASE), &Rules::default());
        assert_eq!(changes, vec![DocChange::Removed(9)]);
    }

    #[test]
    fn noise() {
        let tiny = BASE.replace("{x: 0, y: 1, z: 0}", "{x: 0, y: 1.0000001, z: 0}");
        assert!(noise_only(BASE, &tiny, &Rules::default()));
        let baked = BASE.replace("{r: 0, g: 0, b: 0, a: 1}", "{r: 0.2, g: 0.3, b: 0.4, a: 1}");
        assert!(noise_only(BASE, &baked, &Rules::default()));
        let real = BASE.replace("{x: 0, y: 1, z: 0}", "{x: 0, y: 2, z: 0}");
        assert!(!noise_only(BASE, &real, &Rules::default()));
        assert!(!noise_only("a", "b", &Rules::default()));
    }

    #[test]
    fn sequence_changes() {
        let new = BASE.replace("  m_Children: []\n", "  m_Children:\n  - {fileID: 5}\n");
        let changes = diff(&file(BASE), &file(&new), &Rules::default());
        let DocChange::Changed { props, .. } = &changes[0] else {
            panic!()
        };
        assert_eq!(props[0].path, vec!["m_Children", "[0]"]);
        assert_eq!(props[0].new.as_deref(), Some("{fileID: 5}"));
    }
}
