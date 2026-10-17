//! The object hierarchy of a scene or prefab: GameObjects and prefab
//! instances, their names, parents, children and components
//! (DESIGN.md §11.4), and object paths (`Environment/Enemy[2]`, §10.1).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::{File, FileId, Value};

pub const GAME_OBJECT: u32 = 1;
pub const TRANSFORM: u32 = 4;
pub const RECT_TRANSFORM: u32 = 224;
pub const MONO_BEHAVIOUR: u32 = 114;
pub const PREFAB_INSTANCE: u32 = 1001;
pub const SCENE_ROOTS: u32 = 1_660_057_539;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    GameObject,
    /// A nested prefab placed in this file.
    PrefabInstance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// The GameObject's fileID, or the PrefabInstance's.
    pub id: FileId,
    pub kind: ObjectKind,
    pub name: String,
    pub parent: Option<FileId>,
    pub children: Vec<FileId>,
    /// Component documents (GameObjects only).
    pub components: Vec<FileId>,
    /// The source prefab's GUID (prefab instances only).
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Hierarchy {
    objects: BTreeMap<FileId, Object>,
    roots: Vec<FileId>,
    /// Document → the object it belongs to (the object itself, its
    /// components, and documents stripped from prefab instances).
    owner: HashMap<FileId, FileId>,
}

/// How to name a prefab instance whose name is not overridden: by the source
/// prefab's GUID.
pub type NameLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

impl Hierarchy {
    pub fn build(file: &File) -> Hierarchy {
        Hierarchy::build_with(file, &|_| None)
    }

    pub fn build_with(file: &File, prefab_name: NameLookup<'_>) -> Hierarchy {
        let mut h = Hierarchy::default();
        // transform fileID → owning object
        let mut transform_owner: HashMap<FileId, FileId> = HashMap::new();
        // transform fileID → (parent transform, children transforms)
        let mut transforms: HashMap<FileId, (FileId, Vec<FileId>, Option<i64>)> = HashMap::new();
        let mut instance_parent: HashMap<FileId, FileId> = HashMap::new();

        for doc in &file.docs {
            let (Some(id), Some(class), Some(body)) = (doc.file_id, doc.class_id, doc.body())
            else {
                continue;
            };
            let refers = |key: &str| body.get(key).and_then(Value::as_ref).map(|r| r.file_id);

            if doc.stripped {
                if let Some(instance) = refers("m_PrefabInstance").filter(|i| *i != 0) {
                    h.owner.insert(id, instance);
                    if matches!(class, TRANSFORM | RECT_TRANSFORM) {
                        transform_owner.insert(id, instance);
                    }
                }
                continue;
            }
            match class {
                GAME_OBJECT => {
                    let components: Vec<FileId> = body
                        .get("m_Component")
                        .and_then(Value::as_seq)
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|item| {
                            let map = item.value().as_map()?;
                            map.get("component")
                                .and_then(Value::as_ref)
                                .map(|r| r.file_id)
                        })
                        .collect();
                    h.owner.insert(id, id);
                    for component in &components {
                        h.owner.insert(*component, id);
                    }
                    h.objects.insert(
                        id,
                        Object {
                            id,
                            kind: ObjectKind::GameObject,
                            name: body.get("m_Name").and_then(Value::text).unwrap_or_default(),
                            parent: None,
                            children: Vec::new(),
                            components,
                            source: None,
                        },
                    );
                }
                TRANSFORM | RECT_TRANSFORM => {
                    if let Some(owner) = refers("m_GameObject") {
                        transform_owner.insert(id, owner);
                        h.owner.insert(id, owner);
                    }
                    let children = body
                        .get("m_Children")
                        .and_then(Value::as_seq)
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|item| item.value().as_ref().map(|r| r.file_id))
                        .collect();
                    let root_order = body
                        .get("m_RootOrder")
                        .and_then(Value::text)
                        .and_then(|t| t.parse().ok());
                    transforms.insert(id, (refers("m_Father").unwrap_or(0), children, root_order));
                }
                PREFAB_INSTANCE => {
                    let modification = body.get("m_Modification").and_then(Value::as_map);
                    let parent = modification
                        .and_then(|m| m.get("m_TransformParent"))
                        .and_then(Value::as_ref)
                        .map(|r| r.file_id)
                        .unwrap_or(0);
                    let source = body
                        .get("m_SourcePrefab")
                        .and_then(Value::as_ref)
                        .and_then(|r| r.guid);
                    let name = modification
                        .and_then(|m| m.get("m_Modifications"))
                        .and_then(Value::as_seq)
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|item| item.value().as_map())
                        .find(|m| {
                            m.get("propertyPath").and_then(Value::text).as_deref() == Some("m_Name")
                        })
                        .and_then(|m| m.get("value").and_then(Value::text))
                        .or_else(|| source.as_deref().and_then(prefab_name))
                        .unwrap_or_else(|| "Prefab".to_owned());
                    h.owner.insert(id, id);
                    instance_parent.insert(id, parent);
                    h.objects.insert(
                        id,
                        Object {
                            id,
                            kind: ObjectKind::PrefabInstance,
                            name,
                            parent: None,
                            children: Vec::new(),
                            components: Vec::new(),
                            source,
                        },
                    );
                }
                _ => {
                    if let Some(owner) = refers("m_GameObject").filter(|o| *o != 0) {
                        h.owner.entry(id).or_insert(owner);
                    }
                }
            }
        }

        // Parents and ordered children, from the transforms.
        let mut ordered_children: BTreeMap<FileId, Vec<FileId>> = BTreeMap::new();
        let mut root_orders: HashMap<FileId, i64> = HashMap::new();
        for (transform, (father, children, root_order)) in &transforms {
            let Some(&object) = transform_owner.get(transform) else {
                continue;
            };
            if *father != 0 {
                if let Some(&parent) = transform_owner.get(father) {
                    if let Some(o) = h.objects.get_mut(&object) {
                        o.parent = Some(parent);
                    }
                }
            } else if let Some(order) = root_order {
                root_orders.insert(object, *order);
            }
            let kids: Vec<FileId> = children
                .iter()
                .filter_map(|c| transform_owner.get(c).copied())
                .collect();
            ordered_children.entry(object).or_default().extend(kids);
        }
        for (instance, parent_transform) in instance_parent {
            if parent_transform != 0 {
                if let Some(&parent) = transform_owner.get(&parent_transform) {
                    if let Some(o) = h.objects.get_mut(&instance) {
                        o.parent = Some(parent);
                    }
                }
            }
        }
        for (parent, kids) in &ordered_children {
            if let Some(o) = h.objects.get_mut(parent) {
                o.children = kids.iter().copied().filter(|k| k != parent).collect();
            }
        }
        // Prefab instances are not listed in m_Children of a parent in this
        // file unless a stripped transform stands in for them; add missing ones.
        let parents: Vec<(FileId, FileId)> = h
            .objects
            .values()
            .filter_map(|o| o.parent.map(|p| (p, o.id)))
            .collect();
        for (parent, child) in parents {
            if let Some(p) = h.objects.get_mut(&parent) {
                if !p.children.contains(&child) {
                    p.children.push(child);
                }
            }
        }

        // Roots: SceneRoots (newer Unity), else m_RootOrder, else file order.
        let scene_roots: Option<Vec<FileId>> = file
            .docs
            .iter()
            .find(|d| d.class_id == Some(SCENE_ROOTS))
            .and_then(|d| d.body())
            .and_then(|b| b.get("m_Roots"))
            .and_then(Value::as_seq)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.value().as_ref())
                    .filter_map(|r| transform_owner.get(&r.file_id).copied().or(Some(r.file_id)))
                    .filter(|id| h.objects.contains_key(id))
                    .collect()
            });
        let file_order: Vec<FileId> = file
            .docs
            .iter()
            .filter_map(|d| d.file_id)
            .filter(|id| h.objects.get(id).is_some_and(|o| o.parent.is_none()))
            .collect();
        let mut roots = scene_roots.unwrap_or_default();
        let mut rest: Vec<FileId> = file_order
            .into_iter()
            .filter(|id| !roots.contains(id))
            .collect();
        rest.sort_by_key(|id| root_orders.get(id).copied().unwrap_or(i64::MAX));
        roots.extend(rest);
        h.roots = roots;
        h
    }

    pub fn roots(&self) -> &[FileId] {
        &self.roots
    }

    pub fn object(&self, id: FileId) -> Option<&Object> {
        self.objects.get(&id)
    }

    pub fn objects(&self) -> impl Iterator<Item = &Object> {
        self.objects.values()
    }

    /// The object a document belongs to.
    pub fn owner(&self, doc: FileId) -> Option<FileId> {
        self.owner.get(&doc).copied()
    }

    /// The object's path from its root: `Environment/Enemy[2]`.
    pub fn path(&self, id: FileId) -> Option<String> {
        let mut parts = Vec::new();
        let mut current = Some(id);
        let mut guard = 0;
        while let Some(object_id) = current {
            let object = self.objects.get(&object_id)?;
            parts.push(self.segment(object));
            current = object.parent;
            guard += 1;
            if guard > 10_000 {
                return None;
            }
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// `Enemy`, or `Enemy[2]` when siblings share the name.
    fn segment(&self, object: &Object) -> String {
        let siblings: Vec<FileId> = match object.parent {
            Some(parent) => self
                .objects
                .get(&parent)
                .map(|p| p.children.clone())
                .unwrap_or_default(),
            None => self.roots.clone(),
        };
        let same: Vec<FileId> = siblings
            .into_iter()
            .filter(|s| self.objects.get(s).is_some_and(|o| o.name == object.name))
            .collect();
        let name = object.name.replace('/', "\\/");
        if same.len() > 1 {
            let index = same.iter().position(|s| *s == object.id).unwrap_or(0) + 1;
            format!("{name}[{index}]")
        } else {
            name
        }
    }

    /// The object at `path` (`Environment/Enemy[2]` or `&4821937561`).
    pub fn resolve(&self, path: &str) -> Option<FileId> {
        if let Some(id) = path.strip_prefix('&') {
            let id: FileId = id.parse().ok()?;
            return self
                .objects
                .contains_key(&id)
                .then_some(id)
                .or_else(|| self.owner(id));
        }
        let mut level: Vec<FileId> = self.roots.clone();
        let mut found = None;
        for segment in split_path(path) {
            let (name, index) = parse_segment(&segment);
            let matches: Vec<FileId> = level
                .iter()
                .copied()
                .filter(|id| self.objects.get(id).is_some_and(|o| o.name == name))
                .collect();
            let id = *matches.get(index.unwrap_or(1).checked_sub(1)?)?;
            if index.is_none() && matches.len() > 1 {
                // Ambiguous without an index: take the first, as the picker shows.
            }
            found = Some(id);
            level = self.objects.get(&id)?.children.clone();
        }
        found
    }

    /// The object and all its descendants.
    pub fn subtree(&self, id: FileId) -> BTreeSet<FileId> {
        let mut result = BTreeSet::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if !result.insert(current) {
                continue;
            }
            if let Some(object) = self.objects.get(&current) {
                stack.extend(object.children.iter().copied());
            }
        }
        result
    }

    /// Every document belonging to the object or its descendants.
    pub fn subtree_docs(&self, id: FileId) -> BTreeSet<FileId> {
        let objects = self.subtree(id);
        self.owner
            .iter()
            .filter(|(_, owner)| objects.contains(owner))
            .map(|(doc, _)| *doc)
            .chain(objects.iter().copied())
            .collect()
    }
}

/// Splits at `/`, keeping `\/` inside names.
fn split_path(path: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    parts.last_mut().unwrap().push(next);
                }
            }
            '/' => parts.push(String::new()),
            c => parts.last_mut().unwrap().push(c),
        }
    }
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// `Enemy[2]` → `("Enemy", Some(2))`.
fn parse_segment(segment: &str) -> (String, Option<usize>) {
    if let Some(stripped) = segment.strip_suffix(']') {
        if let Some(pos) = stripped.rfind('[') {
            if let Ok(index) = stripped[pos + 1..].parse() {
                return (stripped[..pos].to_owned(), Some(index));
            }
        }
    }
    (segment.to_owned(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    pub(crate) const SCENE: &str = "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!1 &100
GameObject:
  m_Component:
  - component: {fileID: 101}
  m_Name: Environment
--- !u!4 &101
Transform:
  m_GameObject: {fileID: 100}
  m_Children:
  - {fileID: 201}
  - {fileID: 301}
  - {fileID: 501}
  m_Father: {fileID: 0}
--- !u!1 &200
GameObject:
  m_Component:
  - component: {fileID: 201}
  - component: {fileID: 202}
  m_Name: Enemy
--- !u!4 &201
Transform:
  m_GameObject: {fileID: 200}
  m_Children: []
  m_Father: {fileID: 101}
--- !u!114 &202
MonoBehaviour:
  m_GameObject: {fileID: 200}
  m_Script: {fileID: 11500000, guid: aaaa, type: 3}
--- !u!1 &300
GameObject:
  m_Component:
  - component: {fileID: 301}
  m_Name: Enemy
--- !u!4 &301
Transform:
  m_GameObject: {fileID: 300}
  m_Children: []
  m_Father: {fileID: 101}
--- !u!1001 &500
PrefabInstance:
  m_Modification:
    m_TransformParent: {fileID: 101}
    m_Modifications:
    - target: {fileID: 9, guid: bbbb, type: 3}
      propertyPath: m_Name
      value: Bridge
      objectReference: {fileID: 0}
  m_SourcePrefab: {fileID: 100100000, guid: bbbb, type: 3}
--- !u!4 &501 stripped
Transform:
  m_CorrespondingSourceObject: {fileID: 9, guid: bbbb, type: 3}
  m_PrefabInstance: {fileID: 500}
--- !u!1 &600
GameObject:
  m_Component:
  - component: {fileID: 601}
  m_Name: Sun/Moon
--- !u!4 &601
Transform:
  m_GameObject: {fileID: 600}
  m_Children: []
  m_Father: {fileID: 0}
--- !u!1660057539 &9223372036854775807
SceneRoots:
  m_Roots:
  - {fileID: 601}
  - {fileID: 101}
";

    #[test]
    fn builds_the_tree() {
        let file = parse(SCENE).unwrap();
        let h = Hierarchy::build(&file);
        assert_eq!(h.roots(), &[600, 100]);
        let env = h.object(100).unwrap();
        assert_eq!(env.children, vec![200, 300, 500]);
        assert_eq!(h.object(500).unwrap().name, "Bridge");
        assert_eq!(h.object(500).unwrap().parent, Some(100));
        assert_eq!(h.owner(202), Some(200));
        assert_eq!(h.owner(501), Some(500));
    }

    #[test]
    fn paths() {
        let file = parse(SCENE).unwrap();
        let h = Hierarchy::build(&file);
        assert_eq!(h.path(200).as_deref(), Some("Environment/Enemy[1]"));
        assert_eq!(h.path(300).as_deref(), Some("Environment/Enemy[2]"));
        assert_eq!(h.path(600).as_deref(), Some("Sun\\/Moon"));
        assert_eq!(h.resolve("Environment/Enemy[2]"), Some(300));
        assert_eq!(h.resolve("Environment/Bridge"), Some(500));
        assert_eq!(h.resolve("Sun\\/Moon"), Some(600));
        assert_eq!(h.resolve("&202"), Some(200));
        assert_eq!(h.resolve("Environment/Nope"), None);
        for object in h.objects() {
            assert_eq!(h.resolve(&h.path(object.id).unwrap()), Some(object.id));
        }
    }

    #[test]
    fn subtrees() {
        let file = parse(SCENE).unwrap();
        let h = Hierarchy::build(&file);
        assert_eq!(h.subtree(100), BTreeSet::from([100, 200, 300, 500]));
        let docs = h.subtree_docs(200);
        assert_eq!(docs, BTreeSet::from([200, 201, 202]));
        assert!(h.subtree_docs(100).contains(&501));
    }
}
