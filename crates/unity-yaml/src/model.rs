//! The tree: files, documents, mappings, sequences and scalars.
//!
//! Every entry and sequence item remembers the exact text it was parsed from.
//! Reaching into an entry mutably (`get_mut`, `items_mut`, …) forgets that
//! text for the entry and all its ancestors, so they are formatted anew when
//! written; untouched siblings are written back byte for byte.

use crate::scalar::Scalar;

/// A Unity object's local identifier inside a file.
pub type FileId = i64;

/// A parsed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Everything before the first document (`%YAML 1.1`, `%TAG …`).
    pub prefix: String,
    pub docs: Vec<Doc>,
}

/// One document: a Unity object (`--- !u!1 &4821937561`), or, for plain
/// YAML files such as `.meta`, the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// The header line with its line break, as written; `None` for plain YAML.
    pub header: Option<String>,
    pub class_id: Option<u32>,
    pub file_id: Option<FileId>,
    pub stripped: bool,
    /// The document's top-level mapping. For Unity objects it has one entry,
    /// keyed by the type name (`GameObject`, `Transform`, …).
    pub root: Map,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Map {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    value: Value,
    raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    value: Value,
    raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// `key: value` (the value may continue on following lines).
    Scalar(Scalar),
    /// `key:` followed by nested `key: value` lines.
    Map(Map),
    /// `key:` followed by `- item` lines.
    Seq(Vec<Item>),
    /// `key:` with nothing after it.
    Empty,
}

/// A reference to an object: `{fileID: N}` or
/// `{fileID: N, guid: G, type: T}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjRef {
    pub file_id: FileId,
    pub guid: Option<String>,
    pub kind: Option<u32>,
}

impl File {
    /// The file's text. Unchanged parts are byte-for-byte what was parsed.
    pub fn to_text(&self) -> String {
        let mut out = self.prefix.clone();
        for doc in &self.docs {
            doc.write(&mut out);
        }
        out
    }

    /// Forgets all remembered text, so the whole file is formatted anew.
    /// Used to check that the writer formats like Unity.
    pub fn forget_raw(&mut self) {
        for doc in &mut self.docs {
            doc.root.forget_raw();
        }
    }

    /// The document with this fileID.
    pub fn doc(&self, file_id: FileId) -> Option<&Doc> {
        self.docs.iter().find(|d| d.file_id == Some(file_id))
    }

    pub fn doc_mut(&mut self, file_id: FileId) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.file_id == Some(file_id))
    }
}

impl Doc {
    /// A new Unity object document.
    pub fn new(class_id: u32, file_id: FileId, type_name: &str, body: Map) -> Doc {
        let mut root = Map::default();
        root.push(Entry::new(type_name, Value::Map(body)));
        Doc {
            header: Some(format!("--- !u!{class_id} &{file_id}\n")),
            class_id: Some(class_id),
            file_id: Some(file_id),
            stripped: false,
            root,
        }
    }

    pub fn write(&self, out: &mut String) {
        if let Some(header) = &self.header {
            out.push_str(header);
        }
        self.root.write(0, out);
    }

    /// `GameObject`, `Transform`, `MonoBehaviour`, …
    pub fn type_name(&self) -> Option<&str> {
        self.root.entries.first().map(|e| e.key.as_str())
    }

    /// The object's fields (the mapping under the type name).
    pub fn body(&self) -> Option<&Map> {
        self.root.entries.first()?.value().as_map()
    }

    pub fn body_mut(&mut self) -> Option<&mut Map> {
        self.root.entries.first_mut()?.value_mut().as_map_mut()
    }
}

impl Map {
    pub fn new(entries: Vec<Entry>) -> Map {
        Map { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|e| e.key == key).map(|e| &e.value)
    }

    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// The value for `key`, to change it.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .find(|e| e.key == key)
            .map(|e| e.value_mut())
    }

    /// Replaces the value of `key`, or appends the entry.
    pub fn set(&mut self, key: &str, value: Value) {
        match self.entries.iter_mut().find(|e| e.key == key) {
            Some(entry) => {
                entry.value = value;
                entry.raw = None;
            }
            None => self.entries.push(Entry::new(key, value)),
        }
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn insert(&mut self, index: usize, entry: Entry) {
        self.entries.insert(index.min(self.entries.len()), entry);
    }

    pub fn remove(&mut self, key: &str) -> Option<Entry> {
        let index = self.entries.iter().position(|e| e.key == key)?;
        Some(self.entries.remove(index))
    }

    /// All entries, to change them (each one is formatted anew).
    pub fn entries_mut(&mut self) -> &mut Vec<Entry> {
        &mut self.entries
    }

    pub fn write(&self, indent: usize, out: &mut String) {
        for entry in &self.entries {
            entry.write(indent, out);
        }
    }

    fn forget_raw(&mut self) {
        for entry in &mut self.entries {
            entry.raw = None;
            entry.value.forget_raw();
        }
    }
}

impl Entry {
    pub fn new(key: impl Into<String>, value: Value) -> Entry {
        Entry {
            key: key.into(),
            value,
            raw: None,
        }
    }

    pub(crate) fn parsed(key: String, value: Value, raw: String) -> Entry {
        Entry {
            key,
            value,
            raw: Some(raw),
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// The value, to change it: the entry will be formatted anew.
    pub fn value_mut(&mut self) -> &mut Value {
        self.raw = None;
        &mut self.value
    }

    /// The exact text the entry was parsed from, while unchanged.
    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }

    pub(crate) fn append_raw(&mut self, text: &str) {
        if let Some(raw) = &mut self.raw {
            raw.push_str(text);
        }
    }

    pub fn write(&self, indent: usize, out: &mut String) {
        if let Some(raw) = &self.raw {
            out.push_str(raw);
            return;
        }
        let pad = " ".repeat(indent);
        match &self.value {
            Value::Scalar(scalar) => {
                out.push_str(&format!("{pad}{}: {}\n", self.key, scalar.as_raw()));
            }
            Value::Empty => out.push_str(&format!("{pad}{}:\n", self.key)),
            Value::Map(map) => {
                out.push_str(&format!("{pad}{}:\n", self.key));
                map.write(indent + 2, out);
            }
            Value::Seq(items) => {
                out.push_str(&format!("{pad}{}:\n", self.key));
                for item in items {
                    item.write(indent, out);
                }
            }
        }
    }
}

impl Item {
    pub fn new(value: Value) -> Item {
        Item { value, raw: None }
    }

    pub(crate) fn parsed(value: Value, raw: String) -> Item {
        Item {
            value,
            raw: Some(raw),
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn value_mut(&mut self) -> &mut Value {
        self.raw = None;
        &mut self.value
    }

    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }

    pub(crate) fn append_raw(&mut self, text: &str) {
        if let Some(raw) = &mut self.raw {
            raw.push_str(text);
        }
    }

    pub fn write(&self, indent: usize, out: &mut String) {
        if let Some(raw) = &self.raw {
            out.push_str(raw);
            return;
        }
        let pad = " ".repeat(indent);
        match &self.value {
            Value::Scalar(scalar) => out.push_str(&format!("{pad}- {}\n", scalar.as_raw())),
            Value::Empty => out.push_str(&format!("{pad}-\n")),
            Value::Map(map) => {
                let mut text = String::new();
                map.write(indent + 2, &mut text);
                // The first entry goes on the dash line.
                let first = text.get(indent + 2..).unwrap_or("");
                out.push_str(&format!("{pad}- {first}"));
            }
            Value::Seq(items) if items.is_empty() => out.push_str(&format!("{pad}- []\n")),
            Value::Seq(items) => {
                let mut text = String::new();
                for item in items {
                    item.write(indent + 2, &mut text);
                }
                // The first nested item goes on the dash line: `- - item`.
                let first = text.get(indent + 2..).unwrap_or("");
                out.push_str(&format!("{pad}- {first}"));
            }
        }
    }
}

impl Value {
    fn forget_raw(&mut self) {
        match self {
            Value::Map(map) => map.forget_raw(),
            Value::Seq(items) => {
                for item in items {
                    item.raw = None;
                    item.value.forget_raw();
                }
            }
            Value::Scalar(_) | Value::Empty => {}
        }
    }

    pub fn as_scalar(&self) -> Option<&Scalar> {
        match self {
            Value::Scalar(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_map_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> Option<&[Item]> {
        match self {
            Value::Seq(items) => Some(items),
            // An empty list is often written inline as `[]`.
            Value::Scalar(s) if s.as_raw().trim() == "[]" => Some(&[]),
            _ => None,
        }
    }

    pub fn as_seq_mut(&mut self) -> Option<&mut Vec<Item>> {
        if matches!(self, Value::Scalar(s) if s.as_raw().trim() == "[]") {
            *self = Value::Seq(Vec::new());
        }
        match self {
            Value::Seq(items) => Some(items),
            _ => None,
        }
    }

    /// The value of a scalar, unquoted.
    pub fn text(&self) -> Option<String> {
        self.as_scalar().map(Scalar::value)
    }

    /// The object reference in `{fileID: …}`.
    pub fn as_ref(&self) -> Option<ObjRef> {
        let entries = self.as_scalar()?.flow_entries()?;
        let get = |k: &str| {
            entries
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        Some(ObjRef {
            file_id: get("fileID")?.parse().ok()?,
            guid: get("guid").map(str::to_owned),
            kind: get("type").and_then(|t| t.parse().ok()),
        })
    }
}

impl ObjRef {
    /// A reference to an object in the same file.
    pub fn local(file_id: FileId) -> ObjRef {
        ObjRef {
            file_id,
            guid: None,
            kind: None,
        }
    }

    pub fn is_null(&self) -> bool {
        self.file_id == 0 && self.guid.is_none()
    }

    pub fn to_scalar(&self) -> Scalar {
        let mut entries = vec![("fileID".to_owned(), self.file_id.to_string())];
        if let Some(guid) = &self.guid {
            entries.push(("guid".to_owned(), guid.clone()));
        }
        if let Some(kind) = self.kind {
            entries.push(("type".to_owned(), kind.to_string()));
        }
        Scalar::flow(&entries)
    }
}
