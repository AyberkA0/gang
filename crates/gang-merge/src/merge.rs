//! 3-way semantic merge of Unity YAML files (DESIGN.md §13).
//!
//! Documents are matched by fileID, then merged property by property. A
//! conflict never produces conflict markers: the result always holds a valid
//! value (ours unless a decision says otherwise) and the conflict is reported
//! so `gang bang` can ask. Re-running the merge with decisions applies them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use unity_yaml::{Doc, Entry, File, FileId, Item, Map, ObjRef, Scalar, Value};

use crate::diff::Rules;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Ours,
    Theirs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed the same value differently.
    Value,
    /// Both sides moved the object to different parents.
    Move,
    /// Both sides reordered the same UI object's children.
    Order,
    /// One side deleted an object the other side changed or added to.
    DeleteModify { deleted_by: Side },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// Stable key for decisions.
    pub id: String,
    /// The document (for delete/modify: the deleted object).
    pub doc: FileId,
    /// Field path inside the document.
    pub path: Vec<String>,
    pub kind: ConflictKind,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    /// For delete/modify: objects and components the other side added to the
    /// deleted object.
    pub orphans: Vec<FileId>,
}

/// An answer to a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Ours,
    Theirs,
    Base,
    /// A new value, as scalar text.
    Value(String),
    /// Delete/modify: delete the object (and what was added to it).
    Delete,
    /// Delete/modify: keep the object.
    Keep,
    /// Delete/modify: delete the object, move what was added under it to its
    /// parent.
    Reparent,
}

pub type Decisions = HashMap<String, Choice>;

pub struct Merged {
    pub file: File,
    pub conflicts: Vec<Conflict>,
}

/// Fields merged as sets of elements (order kept from ours).
const IDENTITY_LISTS: &[&str] = &[
    "m_Children",
    "m_Component",
    "m_Roots",
    "m_RemovedComponents",
    "m_RemovedGameObjects",
    "m_AddedGameObjects",
    "m_AddedComponents",
];

/// Flow mappings whose fields only make sense together.
const COUPLED: &[&str] = &["m_LocalRotation", "m_Rotation"];

/// RectTransform fields that together define a layout.
const LAYOUT: &[&str] = &[
    "m_AnchorMin",
    "m_AnchorMax",
    "m_AnchoredPosition",
    "m_SizeDelta",
    "m_Pivot",
];

/// Fields that follow other fields and never conflict on their own.
const FOLLOWERS: &[&str] = &["m_LocalEulerAnglesHint", "m_RootOrder"];

/// Merges three versions of a Unity YAML file.
pub fn merge(
    base: &File,
    ours: &File,
    theirs: &File,
    rules: &Rules,
    decisions: &Decisions,
) -> Merged {
    let mut m = Merger {
        rules,
        decisions,
        conflicts: Vec::new(),
    };
    let file = m.merge_files(base, ours, theirs);
    Merged {
        file,
        conflicts: m.conflicts,
    }
}

struct Merger<'a> {
    rules: &'a Rules,
    decisions: &'a Decisions,
    conflicts: Vec<Conflict>,
}

fn key_of(doc: &Doc, index: usize) -> FileId {
    doc.file_id.unwrap_or(index as FileId)
}

fn doc_text(doc: &Doc) -> String {
    let mut out = String::new();
    doc.write(&mut out);
    out
}

/// Text of a value, for comparisons.
fn text_of(value: &Value) -> String {
    let mut out = String::new();
    Entry::new("v", value.clone()).write(0, &mut out);
    out
}

fn same(a: Option<&Value>, b: Option<&Value>, epsilon: f64) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            if let (Value::Scalar(sx), Value::Scalar(sy)) = (x, y) {
                if sx.as_raw() == sy.as_raw() {
                    return true;
                }
                if let (Ok(fx), Ok(fy)) = (sx.value().parse::<f64>(), sy.value().parse::<f64>()) {
                    return (fx - fy).abs() <= epsilon * fx.abs().max(fy.abs()).max(1.0);
                }
                return false;
            }
            text_of(x) == text_of(y)
        }
        _ => false,
    }
}

fn summary(value: Option<&Value>) -> Option<String> {
    value.map(crate::diff::summary)
}

impl Merger<'_> {
    fn decision(&self, id: &str) -> Option<&Choice> {
        self.decisions.get(id)
    }

    fn merge_files(&mut self, base: &File, ours: &File, theirs: &File) -> File {
        let index = |file: &File| -> BTreeMap<FileId, Doc> {
            file.docs
                .iter()
                .enumerate()
                .map(|(i, d)| (key_of(d, i), d.clone()))
                .collect()
        };
        let base_docs = index(base);
        let our_docs = index(ours);
        let mut their_docs = index(theirs);
        let theirs_order: Vec<FileId> = theirs
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| key_of(d, i))
            .collect();

        // Both added the same fileID with different content: give theirs'
        // additions new ids (all of them, so their references stay together).
        let added_by_both: Vec<FileId> = their_docs
            .keys()
            .filter(|id| !base_docs.contains_key(id) && our_docs.contains_key(id))
            .copied()
            .collect();
        let any_differs = added_by_both
            .iter()
            .any(|id| doc_text(&our_docs[id]) != doc_text(&their_docs[id]));
        let collisions: Vec<FileId> = if any_differs {
            added_by_both
        } else {
            Vec::new()
        };
        let theirs_order = if collisions.is_empty() {
            theirs_order
        } else {
            let used: HashSet<FileId> = base_docs
                .keys()
                .chain(our_docs.keys())
                .chain(their_docs.keys())
                .copied()
                .collect();
            let mut remap = HashMap::new();
            for id in collisions {
                let mut new_id = id.wrapping_mul(31).wrapping_add(0x5bd1_e995);
                while used.contains(&new_id) || remap.values().any(|v| *v == new_id) {
                    new_id = new_id.wrapping_add(1);
                }
                remap.insert(id, new_id);
            }
            let added: BTreeSet<FileId> = their_docs
                .keys()
                .filter(|id| !base_docs.contains_key(id))
                .copied()
                .collect();
            let mut renamed = BTreeMap::new();
            for (id, mut doc) in std::mem::take(&mut their_docs) {
                if added.contains(&id) {
                    rewrite_refs(&mut doc.root, &remap);
                }
                let id = match remap.get(&id) {
                    Some(new_id) => {
                        doc.file_id = Some(*new_id);
                        if let (Some(header), Some(class)) = (&doc.header, doc.class_id) {
                            let stripped = if header.contains(" stripped") {
                                " stripped"
                            } else {
                                ""
                            };
                            doc.header = Some(format!("--- !u!{class} &{new_id}{stripped}\n"));
                        }
                        *new_id
                    }
                    None => id,
                };
                renamed.insert(id, doc);
            }
            their_docs = renamed;
            theirs_order
                .iter()
                .map(|id| remap.get(id).copied().unwrap_or(*id))
                .collect()
        };

        let mut result: Vec<Doc> = Vec::new();
        let mut deleted_by: HashMap<FileId, Side> = HashMap::new();
        let mut modify_delete: Vec<(FileId, Side)> = Vec::new();

        let mut ids: Vec<FileId> = ours
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| key_of(d, i))
            .collect();
        for id in base_docs.keys() {
            if !our_docs.contains_key(id) {
                ids.push(*id);
            }
        }
        for id in ids {
            let b = base_docs.get(&id);
            let o = our_docs.get(&id);
            let t = their_docs.get(&id);
            match (b, o, t) {
                (_, Some(o), Some(t)) => {
                    let merged = self.merge_doc(id, b, o, t);
                    result.push(merged);
                }
                (Some(b), Some(o), None) => {
                    deleted_by.insert(id, Side::Theirs);
                    if doc_text(o) != doc_text(b) {
                        modify_delete.push((id, Side::Theirs));
                        result.push(o.clone());
                    }
                }
                (Some(b), None, Some(t)) => {
                    deleted_by.insert(id, Side::Ours);
                    if doc_text(t) != doc_text(b) {
                        modify_delete.push((id, Side::Ours));
                    }
                }
                (None, Some(o), None) => result.push(o.clone()),
                (Some(_), None, None) => {
                    deleted_by.insert(id, Side::Ours);
                }
                _ => {}
            }
        }

        // Documents only theirs added.
        let sorted_by_id = is_sorted_scene(ours);
        for (n, id) in theirs_order.iter().enumerate() {
            if base_docs.contains_key(id)
                || our_docs.contains_key(id)
                || result.iter().any(|d| d.file_id == Some(*id))
            {
                continue;
            }
            let doc = their_docs[id].clone();
            let position = if sorted_by_id {
                sorted_position(&result, *id)
            } else {
                theirs_order[..n]
                    .iter()
                    .rev()
                    .find_map(|prev| result.iter().position(|d| d.file_id == Some(*prev)))
                    .map(|p| p + 1)
                    .unwrap_or(result.len())
            };
            result.insert(position, doc);
        }

        let mut file = File {
            prefix: ours.prefix.clone(),
            docs: result,
        };
        self.resolve_deletions(&mut file, base, ours, theirs, &deleted_by, &modify_delete);
        // Only fix what the merge broke: inconsistencies already in ours stay.
        let mut ours_repaired = ours.clone();
        let already = repair(&mut ours_repaired);
        repair_except(&mut file, &already);
        file
    }

    fn merge_doc(&mut self, id: FileId, b: Option<&Doc>, o: &Doc, t: &Doc) -> Doc {
        let (ot, tt) = (doc_text(o), doc_text(t));
        if ot == tt {
            return o.clone();
        }
        if let Some(b) = b {
            let bt = doc_text(b);
            if bt == ot {
                return t.clone();
            }
            if bt == tt {
                return o.clone();
            }
        }
        let type_name = o.type_name().unwrap_or("").to_owned();
        let mut doc = o.clone();
        let base_root = b.map(|b| &b.root);
        let root = self.merge_map(id, &type_name, &mut Vec::new(), base_root, &o.root, &t.root);
        doc.root = root;
        doc
    }

    fn merge_map(
        &mut self,
        id: FileId,
        type_name: &str,
        path: &mut Vec<String>,
        b: Option<&Map>,
        o: &Map,
        t: &Map,
    ) -> Map {
        let in_body = path.len() == 1;
        let mut layout_from: Option<Side> = None;
        if in_body && type_name == "RectTransform" {
            layout_from = self.layout_group(id, path, b, o, t);
        }

        let mut out: Vec<Entry> = Vec::new();
        for entry in o.entries() {
            let key = &entry.key;
            path.push(key.clone());
            let bv = b.and_then(|m| m.get(key));
            let tv = t.get(key);
            let merged = if in_body && LAYOUT.contains(&key.as_str()) && layout_from.is_some() {
                match layout_from {
                    Some(Side::Theirs) => tv.cloned(),
                    _ => Some(entry.value().clone()),
                }
            } else {
                self.merge_value(id, type_name, path, bv, Some(entry.value()), tv)
            };
            path.pop();
            if let Some(value) = merged {
                out.push(pick_entry(entry, t.entry(key), key, value));
            }
        }
        for (i, entry) in t.entries().iter().enumerate() {
            let key = &entry.key;
            if o.get(key).is_some() {
                continue;
            }
            path.push(key.clone());
            let merged = self.merge_value(
                id,
                type_name,
                path,
                b.and_then(|m| m.get(key)),
                None,
                Some(entry.value()),
            );
            path.pop();
            if let Some(value) = merged {
                let position = t.entries()[..i]
                    .iter()
                    .rev()
                    .find_map(|prev| out.iter().position(|e| e.key == prev.key))
                    .map(|p| p + 1)
                    .unwrap_or(0);
                out.insert(position, pick_entry(entry, Some(entry), key, value));
            }
        }
        Map::new(out)
    }

    /// RectTransform layout fields change together. Returns which side's
    /// layout to use when both sides changed it.
    fn layout_group(
        &mut self,
        id: FileId,
        path: &[String],
        b: Option<&Map>,
        o: &Map,
        t: &Map,
    ) -> Option<Side> {
        let changed = |m: &Map| -> Vec<&str> {
            LAYOUT
                .iter()
                .copied()
                .filter(|k| !same(b.and_then(|b| b.get(k)), m.get(k), self.rules.float_epsilon))
                .collect()
        };
        let (oc, tc) = (changed(o), changed(t));
        if oc.is_empty() || tc.is_empty() {
            return None;
        }
        let identical = LAYOUT
            .iter()
            .all(|k| same(o.get(k), t.get(k), self.rules.float_epsilon));
        if identical {
            return None;
        }
        let show = |m: Option<&Map>| {
            m.map(|m| {
                LAYOUT
                    .iter()
                    .filter_map(|k| {
                        m.get(k).map(|v| {
                            format!("{}: {}", crate::names::display(k), crate::diff::summary(v))
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        };
        let mut conflict_path = path.to_vec();
        conflict_path.push("(layout)".to_owned());
        let conflict_id = format!("{id}:{}", conflict_path.join("/"));
        let side = match self.decision(&conflict_id) {
            Some(Choice::Theirs) => Side::Theirs,
            _ => Side::Ours,
        };
        self.conflicts.push(Conflict {
            id: conflict_id,
            doc: id,
            path: conflict_path,
            kind: ConflictKind::Value,
            base: show(b),
            ours: show(Some(o)),
            theirs: show(Some(t)),
            orphans: Vec::new(),
        });
        Some(side)
    }

    fn merge_value(
        &mut self,
        id: FileId,
        type_name: &str,
        path: &mut Vec<String>,
        b: Option<&Value>,
        o: Option<&Value>,
        t: Option<&Value>,
    ) -> Option<Value> {
        let eps = self.rules.float_epsilon;
        if same(o, t, eps) {
            return o.cloned();
        }
        if same(b, o, eps) {
            return t.cloned();
        }
        if same(b, t, eps) {
            return o.cloned();
        }
        let field = path.get(1).cloned().unwrap_or_default();
        let last = path.last().cloned().unwrap_or_default();

        match (b, o, t) {
            (b, Some(Value::Map(om)), Some(Value::Map(tm)))
                if b.is_none_or(|b| matches!(b, Value::Map(_))) =>
            {
                let bm = b.and_then(Value::as_map);
                return Some(Value::Map(self.merge_map(id, type_name, path, bm, om, tm)));
            }
            (b, Some(ov), Some(tv))
                if ov.as_seq().is_some()
                    && tv.as_seq().is_some()
                    && b.is_none_or(|b| b.as_seq().is_some()) =>
            {
                let bs = b.and_then(Value::as_seq).unwrap_or(&[]);
                let (os, ts) = (ov.as_seq().unwrap(), tv.as_seq().unwrap());
                if let Some(items) =
                    self.merge_seq(id, type_name, path, &last, bs, os, ts, b.is_some())
                {
                    return Some(if items.is_empty() {
                        ov.clone().empty_like()
                    } else {
                        Value::Seq(items)
                    });
                }
            }
            (_, Some(Value::Scalar(os)), Some(Value::Scalar(ts))) => {
                let flows = (
                    b.and_then(Value::as_scalar).map(Scalar::flow_entries),
                    os.flow_entries(),
                    ts.flow_entries(),
                );
                if let (bf, Some(of), Some(tf)) = flows {
                    let is_ref = of.iter().chain(&tf).any(|(k, _)| k == "fileID");
                    let is_color =
                        of.iter().any(|(k, _)| k == "r") && of.iter().any(|(k, _)| k == "a");
                    if !is_ref && !is_color && !COUPLED.contains(&last.as_str()) {
                        let bf = bf.flatten().unwrap_or_default();
                        return Some(Value::Scalar(
                            self.merge_flow(id, type_name, path, &bf, &of, &tf),
                        ));
                    }
                }
            }
            _ => {}
        }
        if FOLLOWERS.contains(&field.as_str()) || self.is_noise(type_name, &field) {
            return o.cloned().or_else(|| t.cloned());
        }
        let kind = if field == "m_Father" {
            ConflictKind::Move
        } else {
            ConflictKind::Value
        };
        self.value_conflict(id, path, kind, b, o, t)
    }

    fn is_noise(&self, type_name: &str, field: &str) -> bool {
        (type_name == "RenderSettings" && field == "m_IndirectSpecularColor")
            || self
                .rules
                .extra_noise
                .iter()
                .any(|(t, f)| t == type_name && f == field)
    }

    fn value_conflict(
        &mut self,
        id: FileId,
        path: &[String],
        kind: ConflictKind,
        b: Option<&Value>,
        o: Option<&Value>,
        t: Option<&Value>,
    ) -> Option<Value> {
        let conflict_id = format!("{id}:{}", path.join("/"));
        let chosen = match self.decision(&conflict_id) {
            Some(Choice::Theirs) => t.cloned(),
            Some(Choice::Base) => b.cloned(),
            Some(Choice::Value(text)) => Some(Value::Scalar(Scalar::raw(text.clone()))),
            _ => o.cloned(),
        };
        self.conflicts.push(Conflict {
            id: conflict_id,
            doc: id,
            path: path.to_vec(),
            kind,
            base: summary(b),
            ours: summary(o),
            theirs: summary(t),
            orphans: Vec::new(),
        });
        chosen
    }

    fn merge_flow(
        &mut self,
        id: FileId,
        type_name: &str,
        path: &mut Vec<String>,
        b: &[(String, String)],
        o: &[(String, String)],
        t: &[(String, String)],
    ) -> Scalar {
        let get = |list: &[(String, String)], k: &str| {
            list.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| Value::Scalar(Scalar::raw(v.clone())))
        };
        let mut keys: Vec<String> = o.iter().map(|(k, _)| k.clone()).collect();
        for (k, _) in t {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        let mut out = Vec::new();
        for key in keys {
            path.push(key.clone());
            let merged = self.merge_value(
                id,
                type_name,
                path,
                get(b, &key).as_ref(),
                get(o, &key).as_ref(),
                get(t, &key).as_ref(),
            );
            path.pop();
            if let Some(Value::Scalar(s)) = merged {
                out.push((key, s.as_raw().to_owned()));
            }
        }
        Scalar::flow(&out)
    }

    /// Merges lists. `None` means "no list-specific rule; treat as a value".
    #[allow(clippy::too_many_arguments)]
    fn merge_seq(
        &mut self,
        id: FileId,
        type_name: &str,
        path: &mut Vec<String>,
        field: &str,
        b: &[Item],
        o: &[Item],
        t: &[Item],
        had_base: bool,
    ) -> Option<Vec<Item>> {
        if field == "m_Modifications" || keyed(b, o, t).is_some() {
            return Some(self.merge_keyed(id, path, b, o, t));
        }
        if IDENTITY_LISTS.contains(&field) {
            return Some(self.merge_identity(id, type_name, path, field, b, o, t));
        }
        // Same length everywhere: element by element (layers, material slots).
        if had_base && b.len() == o.len() && o.len() == t.len() {
            let mut out = Vec::new();
            for i in 0..o.len() {
                path.push(format!("[{i}]"));
                let merged = self.merge_value(
                    id,
                    type_name,
                    path,
                    Some(b[i].value()),
                    Some(o[i].value()),
                    Some(t[i].value()),
                );
                path.pop();
                let item = match merged {
                    Some(v) if same(Some(&v), Some(o[i].value()), self.rules.float_epsilon) => {
                        o[i].clone()
                    }
                    Some(v) if same(Some(&v), Some(t[i].value()), self.rules.float_epsilon) => {
                        t[i].clone()
                    }
                    Some(v) => Item::new(v),
                    None => o[i].clone(),
                };
                out.push(item);
            }
            return Some(out);
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_identity(
        &mut self,
        id: FileId,
        type_name: &str,
        path: &[String],
        field: &str,
        b: &[Item],
        o: &[Item],
        t: &[Item],
    ) -> Vec<Item> {
        let key = |i: &Item| text_of(i.value());
        let bk: Vec<String> = b.iter().map(key).collect();
        let ok: Vec<String> = o.iter().map(key).collect();
        let tk: Vec<String> = t.iter().map(key).collect();
        let removed_by_theirs: HashSet<&String> = bk.iter().filter(|k| !tk.contains(k)).collect();

        // Both reordered a UI object's children differently.
        if field == "m_Children" && type_name == "RectTransform" {
            let common: Vec<String> = bk
                .iter()
                .filter(|k| ok.contains(k) && tk.contains(k))
                .cloned()
                .collect();
            let order = |keys: &[String]| -> Vec<String> {
                keys.iter()
                    .filter(|k| common.contains(k))
                    .cloned()
                    .collect()
            };
            let (bo, oo, to) = (order(&bk), order(&ok), order(&tk));
            if bo != oo && bo != to && oo != to {
                let conflict_id = format!("{id}:{}", path.join("/"));
                let theirs_wins = matches!(self.decision(&conflict_id), Some(Choice::Theirs));
                self.conflicts.push(Conflict {
                    id: conflict_id,
                    doc: id,
                    path: path.to_vec(),
                    kind: ConflictKind::Order,
                    base: Some(bk.join(", ")),
                    ours: Some(ok.join(", ")),
                    theirs: Some(tk.join(", ")),
                    orphans: Vec::new(),
                });
                if theirs_wins {
                    return self.merge_identity_from(t, o, b);
                }
            }
        }
        let _ = removed_by_theirs;
        self.merge_identity_from(o, t, b)
    }

    /// `primary`'s order, without what `other` removed, plus what `other`
    /// added (after its predecessor).
    fn merge_identity_from(&self, primary: &[Item], other: &[Item], b: &[Item]) -> Vec<Item> {
        let key = |i: &Item| text_of(i.value());
        let bk: Vec<String> = b.iter().map(key).collect();
        let other_keys: Vec<String> = other.iter().map(key).collect();
        let mut out: Vec<Item> = primary
            .iter()
            .filter(|i| {
                let k = key(i);
                !bk.contains(&k) || other_keys.contains(&k)
            })
            .cloned()
            .collect();
        for (n, item) in other.iter().enumerate() {
            let k = key(item);
            if bk.contains(&k) || out.iter().any(|i| key(i) == k) {
                continue;
            }
            let mut position = other_keys[..n]
                .iter()
                .rev()
                .find_map(|prev| out.iter().position(|i| key(i) == *prev))
                .map(|p| p + 1)
                .unwrap_or(0);
            // Our own additions right there come first, as git would order them.
            while position < out.len() {
                let k = key(&out[position]);
                if bk.contains(&k) || other_keys.contains(&k) {
                    break;
                }
                position += 1;
            }
            out.insert(position, item.clone());
        }
        out
    }

    fn merge_keyed(
        &mut self,
        id: FileId,
        path: &mut Vec<String>,
        b: &[Item],
        o: &[Item],
        t: &[Item],
    ) -> Vec<Item> {
        let key = |i: &Item| item_key(i).unwrap_or_else(|| text_of(i.value()));
        let index = |items: &[Item]| -> BTreeMap<String, Item> {
            items.iter().map(|i| (key(i), i.clone())).collect()
        };
        let (bi, oi, ti) = (index(b), index(o), index(t));
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for item in o.iter().chain(t.iter()) {
            let k = key(item);
            if !seen.insert(k.clone()) {
                continue;
            }
            path.push(k.clone());
            let merged = self.merge_value(
                id,
                "",
                path,
                bi.get(&k).map(Item::value),
                oi.get(&k).map(Item::value),
                ti.get(&k).map(Item::value),
            );
            path.pop();
            if let Some(value) = merged {
                let item = [oi.get(&k), ti.get(&k)]
                    .into_iter()
                    .flatten()
                    .find(|i| same(Some(i.value()), Some(&value), self.rules.float_epsilon))
                    .cloned()
                    .unwrap_or_else(|| Item::new(value));
                out.push(item);
            }
        }
        out
    }

    /// Objects deleted on one side and changed or added to on the other.
    fn resolve_deletions(
        &mut self,
        file: &mut File,
        base: &File,
        ours: &File,
        theirs: &File,
        deleted_by: &HashMap<FileId, Side>,
        modify_delete: &[(FileId, Side)],
    ) {
        let base_h = unity_yaml::Hierarchy::build(base);

        // Objects one side deleted; only the top-most of a deleted subtree.
        let deleted_objects: BTreeMap<FileId, Side> = deleted_by
            .iter()
            .filter(|(id, _)| base_h.object(**id).is_some())
            .map(|(id, side)| (*id, *side))
            .collect();
        let top: Vec<(FileId, Side)> = deleted_objects
            .iter()
            .filter(|(id, _)| {
                base_h
                    .object(**id)
                    .and_then(|o| o.parent)
                    .is_none_or(|p| !deleted_objects.contains_key(&p))
            })
            .map(|(id, side)| (*id, *side))
            .collect();

        // Deleted object → (who deleted it, what the other side added to it).
        let mut groups: BTreeMap<FileId, (Side, BTreeSet<FileId>)> = BTreeMap::new();
        for (object, side) in top {
            let docs = base_h.subtree_docs(object);
            let modified = modify_delete
                .iter()
                .any(|(d, s)| docs.contains(d) && *s == side);
            let orphans: BTreeSet<FileId> = file
                .docs
                .iter()
                .filter_map(|d| {
                    let id = d.file_id?;
                    if base.doc(id).is_some() {
                        return None;
                    }
                    let body = d.body()?;
                    ["m_Father", "m_GameObject"]
                        .iter()
                        .any(|f| {
                            body.get(f)
                                .and_then(Value::as_ref)
                                .is_some_and(|r| r.guid.is_none() && docs.contains(&r.file_id))
                        })
                        .then_some(id)
                })
                .collect();
            if modified || !orphans.is_empty() {
                groups.insert(object, (side, orphans));
            }
        }
        // A component deleted on one side and changed on the other.
        for (doc, side) in modify_delete {
            let owner = base_h.owner(*doc).unwrap_or(*doc);
            if !deleted_objects.contains_key(&owner) {
                groups.entry(*doc).or_insert((*side, BTreeSet::new()));
            }
        }
        for (object, (side, orphans)) in groups {
            let conflict_id = format!("{object}:delete");
            let default = if side == Side::Ours {
                Choice::Delete
            } else {
                Choice::Keep
            };
            let choice = self.decision(&conflict_id).cloned().unwrap_or(default);
            let name = base_h.path(object);
            self.conflicts.push(Conflict {
                id: conflict_id,
                doc: object,
                path: Vec::new(),
                kind: ConflictKind::DeleteModify { deleted_by: side },
                base: name.clone(),
                ours: (side == Side::Theirs).then(|| name.clone().unwrap_or_default()),
                theirs: (side == Side::Ours).then(|| name.clone().unwrap_or_default()),
                orphans: orphans.iter().copied().collect(),
            });
            let keeper = if side == Side::Ours { theirs } else { ours };
            let object_docs: BTreeSet<FileId> = if base_h.object(object).is_some() {
                base_h.subtree_docs(object)
            } else {
                BTreeSet::from([object])
            };
            // "Ours"/"Theirs" mean the version of whoever did not delete it.
            let keep = choice == Choice::Keep
                || (choice == Choice::Ours && side == Side::Theirs)
                || (choice == Choice::Theirs && side == Side::Ours);
            let choice = if keep { Choice::Keep } else { choice };
            match choice {
                Choice::Keep => {
                    for id in &object_docs {
                        if file.doc(*id).is_none() {
                            if let Some(doc) = keeper.doc(*id).or_else(|| base.doc(*id)) {
                                let position = sorted_position(&file.docs, *id);
                                file.docs.insert(position, doc.clone());
                            }
                        }
                    }
                }
                Choice::Reparent => {
                    let parent_transform = base_h
                        .object(object)
                        .and_then(|o| o.parent)
                        .and_then(|p| transform_of(base, p));
                    file.docs
                        .retain(|d| d.file_id.is_none_or(|id| !object_docs.contains(&id)));
                    for orphan in &orphans {
                        let Some(doc) = file.doc_mut(*orphan) else {
                            continue;
                        };
                        let is_transform = doc.body().and_then(|b| b.get("m_Father")).is_some();
                        if is_transform {
                            if let Some(body) = doc.body_mut() {
                                body.set(
                                    "m_Father",
                                    Value::Scalar(
                                        ObjRef::local(parent_transform.unwrap_or(0)).to_scalar(),
                                    ),
                                );
                            }
                        }
                    }
                    // Components added to the deleted object cannot move.
                    let gone: HashSet<FileId> = object_docs.iter().copied().collect();
                    file.docs.retain(|d| {
                        d.body()
                            .and_then(|b| b.get("m_GameObject"))
                            .and_then(Value::as_ref)
                            .is_none_or(|r| !gone.contains(&r.file_id))
                    });
                }
                _ => {
                    // Delete the object and everything that hangs off it.
                    let mut gone: HashSet<FileId> = object_docs.iter().copied().collect();
                    loop {
                        let before = gone.len();
                        for doc in &file.docs {
                            let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
                                continue;
                            };
                            for field in ["m_Father", "m_GameObject"] {
                                if body
                                    .get(field)
                                    .and_then(Value::as_ref)
                                    .is_some_and(|r| gone.contains(&r.file_id))
                                {
                                    gone.insert(id);
                                }
                            }
                        }
                        // A GameObject goes with its transform, and its components with it.
                        for doc in &file.docs {
                            let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
                                continue;
                            };
                            let Some(items) = body.get("m_Component").and_then(Value::as_seq)
                            else {
                                continue;
                            };
                            let components: Vec<FileId> = items
                                .iter()
                                .filter_map(|i| {
                                    i.value()
                                        .as_map()?
                                        .get("component")?
                                        .as_ref()
                                        .map(|r| r.file_id)
                                })
                                .collect();
                            if gone.contains(&id) || components.iter().any(|c| gone.contains(c)) {
                                gone.insert(id);
                                gone.extend(components);
                            }
                        }
                        if gone.len() == before {
                            break;
                        }
                    }
                    file.docs
                        .retain(|d| d.file_id.is_none_or(|id| !gone.contains(&id)));
                }
            }
        }
    }
}

trait EmptyLike {
    fn empty_like(self) -> Value;
}

impl EmptyLike for Value {
    /// An empty list written the way the original was (`[]` or a block).
    fn empty_like(self) -> Value {
        match self {
            Value::Scalar(s) => Value::Scalar(s),
            _ => Value::Scalar(Scalar::raw("[]")),
        }
    }
}

/// Keeps the original entry (and its exact text) when the merged value is
/// one of the sides'.
fn pick_entry(ours: &Entry, theirs: Option<&Entry>, key: &str, value: Value) -> Entry {
    if text_of(ours.value()) == text_of(&value) {
        return ours.clone();
    }
    if let Some(theirs) = theirs {
        if text_of(theirs.value()) == text_of(&value) {
            return theirs.clone();
        }
    }
    Entry::new(key, value)
}

/// The key of a keyed-list item: prefab modifications by target and property,
/// lists of maps with a `guid` (build settings scenes) by guid.
fn item_key(item: &Item) -> Option<String> {
    let map = item.value().as_map()?;
    if let (Some(target), Some(property)) = (map.get("target"), map.get("propertyPath")) {
        let target = target
            .as_ref()
            .map(|r| format!("{}:{}", r.file_id, r.guid.unwrap_or_default()))?;
        return Some(format!("{}@{target}", property.text()?));
    }
    map.get("guid")
        .and_then(Value::text)
        .map(|g| format!("guid:{g}"))
}

/// Whether every item of the lists has a key.
fn keyed(b: &[Item], o: &[Item], t: &[Item]) -> Option<()> {
    let all = b.iter().chain(o).chain(t);
    let mut any = false;
    for item in all {
        item_key(item)?;
        any = true;
    }
    any.then_some(())
}

fn is_sorted_scene(file: &File) -> bool {
    let ids: Vec<FileId> = file
        .docs
        .iter()
        .filter(|d| !matches!(d.class_id, Some(29 | 104 | 157 | 196 | 1_660_057_539)))
        .filter_map(|d| d.file_id)
        .collect();
    ids.len() >= 2 && ids.windows(2).all(|w| w[0] <= w[1])
}

/// Where a document goes in a scene sorted by fileID (scene settings first,
/// SceneRoots last).
fn sorted_position(docs: &[Doc], id: FileId) -> usize {
    let settings = |d: &Doc| matches!(d.class_id, Some(29 | 104 | 157 | 196));
    let roots = |d: &Doc| d.class_id == Some(1_660_057_539);
    docs.iter()
        .position(|d| roots(d) || (!settings(d) && d.file_id.is_some_and(|f| f > id)))
        .unwrap_or(docs.len())
}

fn transform_of(file: &File, object: FileId) -> Option<FileId> {
    let doc = file.doc(object)?;
    let items = doc.body()?.get("m_Component")?.as_seq()?;
    items.iter().find_map(|item| {
        let component = item.value().as_map()?.get("component")?.as_ref()?.file_id;
        let class = file.doc(component)?.class_id?;
        matches!(class, 4 | 224).then_some(component)
    })
}

/// Rewrites local references (`{fileID: N}` without a guid).
fn rewrite_refs(map: &mut Map, remap: &HashMap<FileId, FileId>) {
    for entry in map.entries_mut() {
        rewrite_value(entry.value_mut(), remap);
    }
}

fn rewrite_value(value: &mut Value, remap: &HashMap<FileId, FileId>) {
    match value {
        Value::Scalar(s) => {
            if let Some(r) = Value::Scalar(s.clone()).as_ref() {
                if r.guid.is_none() {
                    if let Some(new_id) = remap.get(&r.file_id) {
                        *s = ObjRef::local(*new_id).to_scalar();
                    }
                }
            }
        }
        Value::Map(m) => rewrite_refs(m, remap),
        Value::Seq(items) => {
            for item in items {
                rewrite_value(item.value_mut(), remap);
            }
        }
        Value::Empty => {}
    }
}

/// Makes the hierarchy consistent after a merge: children lists match
/// parents, component lists match components, scene roots match roots.
/// Returns the documents it changed.
pub fn repair(file: &mut File) -> HashSet<FileId> {
    repair_except(file, &HashSet::new())
}

/// [`repair`], leaving the documents in `skip` alone.
fn repair_except(file: &mut File, skip: &HashSet<FileId>) -> HashSet<FileId> {
    let before: HashMap<FileId, String> = file
        .docs
        .iter()
        .filter_map(|d| Some((d.file_id?, doc_text(d))))
        .collect();
    repair_all(file, skip);
    file.docs
        .iter()
        .filter_map(|d| {
            let id = d.file_id?;
            (before.get(&id) != Some(&doc_text(d))).then_some(id)
        })
        .collect()
}

fn repair_all(file: &mut File, skip: &HashSet<FileId>) {
    let present: HashSet<FileId> = file.docs.iter().filter_map(|d| d.file_id).collect();
    let mut father: HashMap<FileId, FileId> = HashMap::new();
    let mut stripped: HashSet<FileId> = HashSet::new();
    let mut owner_of_component: HashMap<FileId, FileId> = HashMap::new();
    let mut transform_classes: HashSet<FileId> = HashSet::new();
    for doc in &file.docs {
        let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
            continue;
        };
        if doc.stripped {
            stripped.insert(id);
            continue;
        }
        if matches!(doc.class_id, Some(4 | 224)) {
            transform_classes.insert(id);
            if let Some(f) = body.get("m_Father").and_then(Value::as_ref) {
                father.insert(id, f.file_id);
            }
        }
        if doc.class_id != Some(1) {
            if let Some(go) = body.get("m_GameObject").and_then(Value::as_ref) {
                if go.file_id != 0 {
                    owner_of_component.insert(id, go.file_id);
                }
            }
        }
    }
    // Break cycles: a transform whose father chain returns to itself goes to the root.
    for &t in father.clone().keys() {
        let mut seen = HashSet::new();
        let mut current = t;
        while let Some(&f) = father.get(&current) {
            if f == 0 || !seen.insert(current) {
                break;
            }
            if f == t {
                father.insert(t, 0);
                if !skip.contains(&t) {
                    if let Some(body) = file.doc_mut(t).and_then(Doc::body_mut) {
                        body.set("m_Father", Value::Scalar(ObjRef::local(0).to_scalar()));
                    }
                }
                break;
            }
            current = f;
        }
    }

    for doc in file.docs.iter_mut() {
        let Some(id) = doc.file_id else { continue };
        if skip.contains(&id) {
            continue;
        }
        let class = doc.class_id;
        let is_stripped = doc.stripped;
        let Some(body) = doc.body_mut() else { continue };
        if matches!(class, Some(4 | 224)) && !is_stripped {
            let current: Vec<FileId> = body
                .get("m_Children")
                .and_then(Value::as_seq)
                .unwrap_or(&[])
                .iter()
                .filter_map(|i| i.value().as_ref().map(|r| r.file_id))
                .collect();
            let keep: Vec<FileId> = current
                .iter()
                .copied()
                .filter(|c| {
                    present.contains(c) && (stripped.contains(c) || father.get(c) == Some(&id))
                })
                .collect();
            let mut wanted = keep.clone();
            let mut missing: Vec<FileId> = father
                .iter()
                .filter(|(_, f)| **f == id)
                .map(|(c, _)| *c)
                .filter(|c| !wanted.contains(c))
                .collect();
            missing.sort();
            wanted.extend(missing);
            if wanted != current {
                let items = body.get_mut("m_Children").and_then(Value::as_seq_mut);
                if let Some(items) = items {
                    items.retain(|i| {
                        i.value()
                            .as_ref()
                            .is_some_and(|r| keep.contains(&r.file_id))
                    });
                    for c in wanted.iter().filter(|c| !keep.contains(c)) {
                        items.push(Item::new(Value::Scalar(ObjRef::local(*c).to_scalar())));
                    }
                    if items.is_empty() {
                        body.set("m_Children", Value::Scalar(Scalar::raw("[]")));
                    }
                }
            }
        }
        if class == Some(1) {
            let current: Vec<FileId> = body
                .get("m_Component")
                .and_then(Value::as_seq)
                .unwrap_or(&[])
                .iter()
                .filter_map(|i| {
                    i.value()
                        .as_map()?
                        .get("component")?
                        .as_ref()
                        .map(|r| r.file_id)
                })
                .collect();
            let mut missing: Vec<FileId> = owner_of_component
                .iter()
                .filter(|(c, o)| **o == id && !current.contains(c))
                .map(|(c, _)| *c)
                .collect();
            missing.sort();
            let dangling: Vec<FileId> = current
                .iter()
                .copied()
                .filter(|c| !present.contains(c))
                .collect();
            if !missing.is_empty() || !dangling.is_empty() {
                if let Some(items) = body.get_mut("m_Component").and_then(Value::as_seq_mut) {
                    items.retain(|i| {
                        i.value()
                            .as_map()
                            .and_then(|m| m.get("component"))
                            .and_then(Value::as_ref)
                            .is_some_and(|r| present.contains(&r.file_id))
                    });
                    for c in missing {
                        let mut map = Map::default();
                        map.push(Entry::new(
                            "component",
                            Value::Scalar(ObjRef::local(c).to_scalar()),
                        ));
                        items.push(Item::new(Value::Map(map)));
                    }
                }
            }
        }
        if class == Some(1_660_057_539) {
            if let Some(items) = body.get_mut("m_Roots").and_then(Value::as_seq_mut) {
                items.retain(|i| {
                    i.value()
                        .as_ref()
                        .is_none_or(|r| present.contains(&r.file_id))
                });
            }
        }
    }
    // New root transforms belong in SceneRoots.
    let listed: HashSet<FileId> = file
        .docs
        .iter()
        .filter(|d| d.class_id == Some(1_660_057_539))
        .filter_map(|d| {
            d.body()?.get("m_Roots")?.as_seq().map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.value().as_ref().map(|r| r.file_id))
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect();
    let mut new_roots: Vec<FileId> = father
        .iter()
        .filter(|(t, f)| **f == 0 && transform_classes.contains(t) && !listed.contains(t))
        .map(|(t, _)| *t)
        .collect();
    new_roots.sort();
    if !new_roots.is_empty() {
        if let Some(doc) = file.docs.iter_mut().find(|d| {
            d.class_id == Some(1_660_057_539) && d.file_id.is_none_or(|id| !skip.contains(&id))
        }) {
            if let Some(items) = doc
                .body_mut()
                .and_then(|b| b.get_mut("m_Roots"))
                .and_then(Value::as_seq_mut)
            {
                for t in new_roots {
                    items.push(Item::new(Value::Scalar(ObjRef::local(t).to_scalar())));
                }
            }
        }
    }
}
