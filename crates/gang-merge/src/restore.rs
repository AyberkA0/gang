//! Undoing the changes to one object, keeping everything else
//! (`gang restore <file>::<object>`).

use std::collections::BTreeSet;

use unity_yaml::{File, FileId, Hierarchy, Item, Value};

use crate::diff::{Rules, diff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreError {
    NotFound(String),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::NotFound(path) => write!(f, "no object {path:?} in either version"),
        }
    }
}

impl std::error::Error for RestoreError {}

/// Puts the object at `path` (and its components and descendants) in
/// `current` back to how it is in `original`. Returns how many documents
/// changed.
pub fn restore_object(
    current: &mut File,
    original: &File,
    path: &str,
) -> Result<usize, RestoreError> {
    let current_h = Hierarchy::build(current);
    let original_h = Hierarchy::build(original);
    let id = current_h
        .resolve(path)
        .or_else(|| original_h.resolve(path))
        .ok_or_else(|| RestoreError::NotFound(path.to_owned()))?;

    let mut docs: BTreeSet<FileId> = BTreeSet::new();
    if current_h.object(id).is_some() {
        docs.extend(current_h.subtree_docs(id));
    }
    if original_h.object(id).is_some() {
        docs.extend(original_h.subtree_docs(id));
    }
    let before = current.clone();

    // Documents: take the original's, drop ones the original does not have.
    let original_order: Vec<FileId> = original.docs.iter().filter_map(|d| d.file_id).collect();
    for &doc in &docs {
        match original.doc(doc) {
            Some(original_doc) => {
                if let Some(slot) = current.doc_mut(doc) {
                    *slot = original_doc.clone();
                } else {
                    let position = original_order
                        .iter()
                        .position(|d| *d == doc)
                        .and_then(|i| {
                            original_order[..i]
                                .iter()
                                .rev()
                                .find(|d| current.doc(**d).is_some())
                        })
                        .and_then(|prev| current.docs.iter().position(|d| d.file_id == Some(*prev)))
                        .map(|i| i + 1)
                        .unwrap_or(current.docs.len());
                    current.docs.insert(position, original_doc.clone());
                }
            }
            None => current.docs.retain(|d| d.file_id != Some(doc)),
        }
    }

    // Parent links outside the subtree: m_Children lists and scene roots.
    for &doc in &docs {
        let is_transform = original
            .doc(doc)
            .or_else(|| before.doc(doc))
            .and_then(|d| d.class_id)
            .is_some_and(|c| c == 4 || c == 224);
        if !is_transform {
            continue;
        }
        let original_place = find_in_lists(original, doc, &docs);
        for list_owner in lists_containing(current, doc, &docs) {
            remove_from_list(current, list_owner, doc);
        }
        if let Some((owner, index)) = original_place {
            insert_into_list(current, owner, index, doc);
        }
    }

    Ok(diff(&before, current, &Rules::default()).len())
}

/// The list field of a document that holds child transforms.
fn list_field(file: &File, owner: FileId) -> Option<&'static str> {
    let doc = file.doc(owner)?;
    match doc.type_name()? {
        "Transform" | "RectTransform" => Some("m_Children"),
        "SceneRoots" => Some("m_Roots"),
        _ => None,
    }
}

fn refs(file: &File, owner: FileId) -> Vec<FileId> {
    let Some(field) = list_field(file, owner) else {
        return Vec::new();
    };
    file.doc(owner)
        .and_then(|d| d.body())
        .and_then(|b| b.get(field))
        .and_then(Value::as_seq)
        .unwrap_or(&[])
        .iter()
        .filter_map(|i| i.value().as_ref().map(|r| r.file_id))
        .collect()
}

/// `(list owner, index)` of `doc` in a list outside `inside`.
fn find_in_lists(file: &File, doc: FileId, inside: &BTreeSet<FileId>) -> Option<(FileId, usize)> {
    file.docs
        .iter()
        .filter_map(|d| d.file_id)
        .filter(|id| !inside.contains(id))
        .find_map(|owner| {
            refs(file, owner)
                .iter()
                .position(|r| *r == doc)
                .map(|i| (owner, i))
        })
}

fn lists_containing(file: &File, doc: FileId, inside: &BTreeSet<FileId>) -> Vec<FileId> {
    file.docs
        .iter()
        .filter_map(|d| d.file_id)
        .filter(|id| !inside.contains(id))
        .filter(|owner| refs(file, *owner).contains(&doc))
        .collect()
}

fn remove_from_list(file: &mut File, owner: FileId, doc: FileId) {
    let Some(field) = list_field(file, owner) else {
        return;
    };
    if let Some(items) = file
        .doc_mut(owner)
        .and_then(|d| d.body_mut())
        .and_then(|b| b.get_mut(field))
        .and_then(Value::as_seq_mut)
    {
        items.retain(|i| i.value().as_ref().map(|r| r.file_id) != Some(doc));
    }
}

fn insert_into_list(file: &mut File, owner: FileId, index: usize, doc: FileId) {
    let Some(field) = list_field(file, owner) else {
        return;
    };
    if let Some(items) = file
        .doc_mut(owner)
        .and_then(|d| d.body_mut())
        .and_then(|b| b.get_mut(field))
        .and_then(Value::as_seq_mut)
    {
        let item = Item::new(Value::Scalar(unity_yaml::ObjRef::local(doc).to_scalar()));
        items.insert(index.min(items.len()), item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "--- !u!1 &10
GameObject:
  m_Component:
  - component: {fileID: 11}
  m_Name: Root
--- !u!4 &11
Transform:
  m_GameObject: {fileID: 10}
  m_Children:
  - {fileID: 21}
  m_Father: {fileID: 0}
--- !u!1 &20
GameObject:
  m_Component:
  - component: {fileID: 21}
  m_Name: Player
--- !u!4 &21
Transform:
  m_GameObject: {fileID: 20}
  m_LocalPosition: {x: 0, y: 0, z: 0}
  m_Children: []
  m_Father: {fileID: 11}
--- !u!1 &30
GameObject:
  m_Component:
  - component: {fileID: 31}
  m_Name: Tree
--- !u!4 &31
Transform:
  m_GameObject: {fileID: 30}
  m_Children: []
  m_Father: {fileID: 0}
";

    #[test]
    fn restores_one_object_and_keeps_the_rest() {
        let changed = ORIGINAL
            .replace(
                "m_LocalPosition: {x: 0, y: 0, z: 0}",
                "m_LocalPosition: {x: 5, y: 0, z: 0}",
            )
            .replace("m_Name: Tree", "m_Name: Oak");
        let mut current = unity_yaml::parse(&changed).unwrap();
        let original = unity_yaml::parse(ORIGINAL).unwrap();
        let count = restore_object(&mut current, &original, "Root/Player").unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            current.to_text(),
            ORIGINAL.replace("m_Name: Tree", "m_Name: Oak")
        );
    }

    #[test]
    fn restores_a_moved_object() {
        // Player moved from Root to Tree.
        let moved = ORIGINAL
            .replace(
                "  m_Children:\n  - {fileID: 21}\n  m_Father: {fileID: 0}\n--- !u!1 &20",
                "  m_Children: []\n  m_Father: {fileID: 0}\n--- !u!1 &20",
            )
            .replace("m_Father: {fileID: 11}", "m_Father: {fileID: 31}")
            .replace(
                "  m_GameObject: {fileID: 30}\n  m_Children: []",
                "  m_GameObject: {fileID: 30}\n  m_Children:\n  - {fileID: 21}",
            );
        let mut current = unity_yaml::parse(&moved).unwrap();
        let original = unity_yaml::parse(ORIGINAL).unwrap();
        restore_object(&mut current, &original, "Tree/Player").unwrap();
        let h = Hierarchy::build(&current);
        assert_eq!(h.path(20).as_deref(), Some("Root/Player"));
        assert!(h.object(30).unwrap().children.is_empty());
    }

    #[test]
    fn removes_an_added_object() {
        let added = format!(
            "{ORIGINAL}--- !u!1 &40\nGameObject:\n  m_Component:\n  - component: {{fileID: 41}}\n  m_Name: New\n--- !u!4 &41\nTransform:\n  m_GameObject: {{fileID: 40}}\n  m_Children: []\n  m_Father: {{fileID: 0}}\n"
        );
        let mut current = unity_yaml::parse(&added).unwrap();
        let original = unity_yaml::parse(ORIGINAL).unwrap();
        restore_object(&mut current, &original, "New").unwrap();
        assert_eq!(current.to_text(), ORIGINAL);
    }
}
