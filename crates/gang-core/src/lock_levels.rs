//! Lock levels (DESIGN.md §10.2–§10.4): what a lock on a file or an object
//! covers besides itself.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use gang_csharp::TypeIndex;
use unicode_normalization::UnicodeNormalization;
use unity_yaml::{File, FileId, GuidIndex, Hierarchy, Map, Value};

use crate::locks::{Derived, Target};
use crate::repo::Repo;
use crate::{semantic, unity};

/// Fields that are structure, not interaction.
const NOT_INTERACTION: &[&str] = &[
    "m_GameObject",
    "m_Script",
    "m_Father",
    "m_Children",
    "m_Component",
    "m_CorrespondingSourceObject",
    "m_PrefabInstance",
    "m_PrefabAsset",
    "m_EditorClassIdentifier",
];

/// Everything lock expansion needs, loaded once.
pub struct Expander<'a> {
    repo: &'a Repo,
    guids: GuidIndex,
    types: Option<TypeIndex>,
    files: HashMap<String, Option<File>>,
}

impl<'a> Expander<'a> {
    pub fn new(repo: &'a Repo) -> Expander<'a> {
        Expander {
            repo,
            guids: semantic::guid_index(repo),
            types: None,
            files: HashMap::new(),
        }
    }

    pub fn guids(&self) -> &GuidIndex {
        &self.guids
    }

    /// The working-tree version of a Unity file, parsed.
    pub fn file(&mut self, path: &str) -> Option<&File> {
        if !self.files.contains_key(path) {
            let parsed = semantic::worktree(self.repo, path)
                .or_else(|| {
                    semantic::blob(self.repo, &format!("HEAD:{path}"))
                        .ok()
                        .flatten()
                })
                .and_then(|t| unity_yaml::parse(&t).ok());
            self.files.insert(path.to_owned(), parsed);
        }
        self.files.get(path).and_then(Option::as_ref)
    }

    pub fn hierarchy(&mut self, path: &str) -> Option<Hierarchy> {
        let guids = self.guids.clone();
        let lookup = move |guid: &str| guids.name(guid);
        self.file(path).map(|f| Hierarchy::build_with(f, &lookup))
    }

    fn types(&mut self) -> &TypeIndex {
        if self.types.is_none() {
            let mut index = TypeIndex::default();
            for project in unity::find_projects(self.repo.root()) {
                let assets = project.path(self.repo.root()).join("Assets");
                walk_scripts(&assets, &mut |path| {
                    if let Ok(source) = std::fs::read_to_string(path) {
                        let relative = path
                            .strip_prefix(self.repo.root())
                            .unwrap_or(path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        index.add(&relative, &source);
                    }
                });
            }
            self.types = Some(index);
        }
        self.types.as_ref().unwrap()
    }

    /// A script's path for a GUID, when the script is in the project.
    fn script(&self, guid: &str) -> Option<String> {
        let path = self.guids.path(guid)?;
        path.ends_with(".cs").then(|| path.to_owned())
    }

    fn name_of(&self, path: &str) -> String {
        let file = path.rsplit('/').next().unwrap_or(path);
        file.rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(file)
            .to_owned()
    }

    /// Scripts on the given documents, and the prefabs nested there.
    fn direct(
        &mut self,
        path: &str,
        docs: &BTreeSet<FileId>,
    ) -> (BTreeSet<String>, BTreeSet<String>) {
        let mut scripts = BTreeSet::new();
        let mut prefabs = BTreeSet::new();
        let Some(file) = self.file(path).cloned() else {
            return (scripts, prefabs);
        };
        for doc in &file.docs {
            let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
                continue;
            };
            if !docs.contains(&id) {
                continue;
            }
            if let Some(guid) = body
                .get("m_Script")
                .and_then(Value::as_ref)
                .and_then(|r| r.guid)
            {
                if let Some(script) = self.script(&guid) {
                    scripts.insert(script);
                }
            }
            if doc.class_id == Some(unity_yaml::hierarchy::PREFAB_INSTANCE) {
                if let Some(source) = body
                    .get("m_SourcePrefab")
                    .and_then(Value::as_ref)
                    .and_then(|r| r.guid)
                {
                    if let Some(prefab) = self.guids.path(&source) {
                        prefabs.insert(prefab.to_owned());
                    }
                }
            }
        }
        (scripts, prefabs)
    }

    /// Level 1 of a whole file: scripts on all its objects and nested
    /// prefab sources, with their scripts (DESIGN.md §10.2).
    fn file_level1(&mut self, path: &str, seen: &mut BTreeSet<String>, out: &mut Vec<Derived>) {
        if !seen.insert(path.to_owned()) {
            return;
        }
        let Some(file) = self.file(path).cloned() else {
            return;
        };
        let all: BTreeSet<FileId> = file.docs.iter().filter_map(|d| d.file_id).collect();
        let (scripts, prefabs) = self.direct(path, &all);
        for script in scripts {
            push(out, Target::File { path: script }, None);
        }
        for prefab in prefabs {
            push(
                out,
                Target::File {
                    path: prefab.clone(),
                },
                None,
            );
            self.file_level1(&prefab, seen, out);
        }
    }

    fn object_level1(
        &mut self,
        path: &str,
        object: FileId,
        out: &mut Vec<Derived>,
        why: Option<String>,
    ) -> Result<()> {
        let hierarchy = self.hierarchy(path).context("cannot read the file")?;
        let display = hierarchy.path(object).unwrap_or_default();
        push(
            out,
            Target::Object {
                file: path.to_owned(),
                file_id: object,
                path: display,
            },
            why,
        );
        let docs = hierarchy.subtree_docs(object);
        let (scripts, prefabs) = self.direct(path, &docs);
        for script in scripts {
            push(out, Target::File { path: script }, None);
        }
        let mut seen = BTreeSet::new();
        for prefab in prefabs {
            push(
                out,
                Target::File {
                    path: prefab.clone(),
                },
                None,
            );
            self.file_level1(&prefab, &mut seen, out);
        }
        Ok(())
    }

    /// What a lock on `root` at `level` covers besides `root` itself.
    pub fn expand(&mut self, root: &Target, level: u8) -> Result<Vec<Derived>> {
        let mut out = Vec::new();
        if level == 0 {
            return Ok(out);
        }
        match root {
            Target::Object { file, file_id, .. } => {
                self.object_level1(file, *file_id, &mut out, None)?;
                out.retain(|d| &d.target != root);
                if level >= 2 {
                    self.object_interactions(file, *file_id, &mut out)?;
                }
            }
            Target::File { path } if path.ends_with(".unity") || path.ends_with(".prefab") => {
                let mut seen = BTreeSet::new();
                self.file_level1(path, &mut seen, &mut out);
                if level >= 2 {
                    let all: BTreeSet<FileId> = self
                        .file(path)
                        .map(|f| f.docs.iter().filter_map(|d| d.file_id).collect())
                        .unwrap_or_default();
                    for (asset, why) in self.asset_refs(path, &all) {
                        push(
                            &mut out,
                            Target::File {
                                path: asset.clone(),
                            },
                            Some(why),
                        );
                        self.file_level1(&asset, &mut seen, &mut out);
                    }
                }
            }
            Target::File { path } if path.ends_with(".cs") && level >= 2 => {
                let neighbors = self.types().neighbors(path);
                for other in neighbors {
                    let why = format!("type used with {}", self.name_of(path));
                    push(&mut out, Target::File { path: other }, Some(why));
                }
            }
            _ => {}
        }
        out.retain(|d| &d.target != root);
        Ok(out)
    }

    /// Level-2 interactions of an object (DESIGN.md §10.3).
    fn object_interactions(
        &mut self,
        path: &str,
        object: FileId,
        out: &mut Vec<Derived>,
    ) -> Result<()> {
        let hierarchy = self.hierarchy(path).context("cannot read the file")?;
        let file = self.file(path).cloned().context("cannot read the file")?;
        let inside = hierarchy.subtree_docs(object);
        let root_name = hierarchy
            .object(object)
            .map(|o| o.name.clone())
            .unwrap_or_default();
        let mut interacting: BTreeMap<FileId, String> = BTreeMap::new();

        // E1: serialized references, both directions.
        for doc in &file.docs {
            let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
                continue;
            };
            if doc.class_id != Some(unity_yaml::hierarchy::MONO_BEHAVIOUR) {
                continue;
            }
            let script_name = body
                .get("m_Script")
                .and_then(Value::as_ref)
                .and_then(|r| r.guid)
                .and_then(|g| self.guids.name(&g))
                .unwrap_or_else(|| "MonoBehaviour".to_owned());
            let Some(owner) = hierarchy.owner(id) else {
                continue;
            };
            for (field, target, method) in local_refs(body) {
                let Some(target_owner) = hierarchy.owner(target) else {
                    continue;
                };
                let target_name = hierarchy
                    .object(target_owner)
                    .map(|o| o.name.clone())
                    .unwrap_or_default();
                let arrow = match method {
                    Some(method) => format!("{target_name}.{method}()"),
                    None => target_name.clone(),
                };
                if inside.contains(&id) && !inside.contains(&target) {
                    interacting
                        .entry(target_owner)
                        .or_insert(format!("{script_name}.{field} → {arrow}"));
                } else if !inside.contains(&id) && inside.contains(&target) {
                    let arrow = if arrow == target_name {
                        root_name.clone()
                    } else {
                        arrow
                    };
                    interacting
                        .entry(owner)
                        .or_insert(format!("{script_name}.{field} → {arrow}"));
                }
            }
        }

        // E2: code references between scripts inside and outside.
        let all_docs: BTreeSet<FileId> = file.docs.iter().filter_map(|d| d.file_id).collect();
        let (inside_scripts, _) = self.direct(path, &inside);
        let objects: Vec<(FileId, Vec<FileId>)> = hierarchy
            .objects()
            .map(|o| (o.id, o.components.clone()))
            .collect();
        for (other, components) in objects {
            if inside.contains(&other) || interacting.contains_key(&other) {
                continue;
            }
            let docs: BTreeSet<FileId> = components
                .into_iter()
                .filter(|c| all_docs.contains(c))
                .collect();
            let (their_scripts, _) = self.direct(path, &docs);
            'search: for a in &their_scripts {
                for b in &inside_scripts {
                    let types = self.types();
                    if types.uses(a, b) || types.uses(b, a) {
                        let user = if types.uses(a, b) { (a, b) } else { (b, a) };
                        let why = format!("{} uses {}", self.name_of(user.0), self.name_of(user.1));
                        interacting.insert(other, why);
                        break 'search;
                    }
                }
            }
        }

        for (other, why) in interacting {
            self.object_level1(path, other, out, Some(why))?;
        }

        // E3: prefabs and ScriptableObjects the subtree's scripts reference.
        let mut seen = BTreeSet::new();
        for (asset, why) in self.asset_refs(path, &inside) {
            push(
                out,
                Target::File {
                    path: asset.clone(),
                },
                Some(why),
            );
            self.file_level1(&asset, &mut seen, out);
        }
        Ok(())
    }

    /// Prefab and ScriptableObject assets referenced by script fields in the
    /// given documents.
    fn asset_refs(&mut self, path: &str, docs: &BTreeSet<FileId>) -> Vec<(String, String)> {
        let Some(file) = self.file(path).cloned() else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for doc in &file.docs {
            let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
                continue;
            };
            if !docs.contains(&id) || doc.class_id != Some(unity_yaml::hierarchy::MONO_BEHAVIOUR) {
                continue;
            }
            let script_name = body
                .get("m_Script")
                .and_then(Value::as_ref)
                .and_then(|r| r.guid)
                .and_then(|g| self.guids.name(&g))
                .unwrap_or_else(|| "MonoBehaviour".to_owned());
            for (field, guid) in guid_refs(body) {
                let Some(asset) = self.guids.path(&guid).map(str::to_owned) else {
                    continue;
                };
                let is_prefab = asset.ends_with(".prefab");
                let is_scriptable = asset.ends_with(".asset")
                    && self
                        .file(&asset)
                        .and_then(|f| f.docs.first())
                        .and_then(|d| d.class_id)
                        == Some(unity_yaml::hierarchy::MONO_BEHAVIOUR);
                if (is_prefab || is_scriptable) && !found.iter().any(|(a, _)| *a == asset) {
                    found.push((asset, format!("{script_name}.{field}")));
                }
            }
        }
        found
    }
}

fn push(out: &mut Vec<Derived>, target: Target, why: Option<String>) {
    if !out.iter().any(|d| d.target == target) {
        out.push(Derived { target, why });
    }
}

fn walk_scripts(dir: &std::path::Path, found: &mut dyn FnMut(&std::path::Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_scripts(&path, found);
        } else if path.extension().is_some_and(|e| e == "cs") {
            found(&path);
        }
    }
}

/// `(field, target fileID, UnityEvent method)` for local references in a
/// script's fields.
fn local_refs(body: &Map) -> Vec<(String, FileId, Option<String>)> {
    let mut out = Vec::new();
    for entry in body.entries() {
        if NOT_INTERACTION.contains(&entry.key.as_str()) {
            continue;
        }
        collect_local(entry.value(), &entry.key, None, &mut out);
    }
    out
}

fn collect_local(
    value: &Value,
    field: &str,
    method: Option<String>,
    out: &mut Vec<(String, FileId, Option<String>)>,
) {
    match value {
        Value::Scalar(_) => {
            if let Some(r) = value.as_ref() {
                if r.guid.is_none() && r.file_id != 0 {
                    out.push((field.to_owned(), r.file_id, method));
                }
            }
        }
        Value::Map(map) => {
            let method = map
                .get("m_MethodName")
                .and_then(Value::text)
                .filter(|m| !m.is_empty())
                .or(method);
            for entry in map.entries() {
                collect_local(entry.value(), field, method.clone(), out);
            }
        }
        Value::Seq(items) => {
            for item in items {
                collect_local(item.value(), field, method.clone(), out);
            }
        }
        Value::Empty => {}
    }
}

fn guid_refs(body: &Map) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in body.entries() {
        if NOT_INTERACTION.contains(&entry.key.as_str()) {
            continue;
        }
        collect_guids(entry.value(), &entry.key, &mut out);
    }
    out
}

fn collect_guids(value: &Value, field: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::Scalar(_) => {
            if let Some(guid) = value.as_ref().and_then(|r| r.guid) {
                out.push((field.to_owned(), guid));
            }
        }
        Value::Map(map) => {
            for entry in map.entries() {
                collect_guids(entry.value(), field, out);
            }
        }
        Value::Seq(items) => {
            for item in items {
                collect_guids(item.value(), field, out);
            }
        }
        Value::Empty => {}
    }
}

/// Why a target could not be resolved.
#[derive(Debug)]
pub enum Resolve {
    Found(Target),
    /// Several assets have this name.
    Ambiguous(Vec<String>),
}

/// Turns what someone typed into a lock target (DESIGN.md §10.1).
/// `prefix` is the current folder relative to the repository root.
pub fn resolve_target(
    repo: &Repo,
    expander: &mut Expander<'_>,
    prefix: &str,
    text: &str,
) -> Result<Resolve> {
    let text: String = text.nfc().collect();
    if let Some((file, object)) = text.split_once("::") {
        let file = repo_path(prefix, file);
        let hierarchy = expander
            .hierarchy(&file)
            .with_context(|| format!("cannot read {file} as a scene or prefab"))?;
        let id = hierarchy
            .resolve(object)
            .with_context(|| format!("no object {object:?} in {file} (see `gang tree {file}`)"))?;
        let path = hierarchy.path(id).unwrap_or_default();
        return Ok(Resolve::Found(Target::Object {
            file,
            file_id: id,
            path,
        }));
    }
    if text.ends_with('/') || text.ends_with('\\') {
        let folder = repo_path(prefix, &text);
        return Ok(Resolve::Found(Target::Folder {
            path: format!("{folder}/"),
        }));
    }
    if text.contains(['*', '?', '[']) {
        return Ok(Resolve::Found(Target::Glob {
            pattern: repo_path(prefix, &text),
        }));
    }
    let path = repo_path(prefix, &text);
    let path = path.strip_suffix(".meta").unwrap_or(&path).to_owned();
    let tracked = repo.tracked_files()?;
    if let Some(exact) = tracked.iter().find(|t| **t == path) {
        return Ok(Resolve::Found(Target::File {
            path: exact.clone(),
        }));
    }
    if let Some(same) = tracked
        .iter()
        .find(|t| t.to_lowercase() == path.to_lowercase())
    {
        return Ok(Resolve::Found(Target::File { path: same.clone() }));
    }
    if repo.root().join(&path).is_file() {
        return Ok(Resolve::Found(Target::File { path }));
    }
    // A name: `Hero` finds Assets/Art/Hero.psd.
    let wanted = text.to_lowercase();
    let matches: Vec<String> = tracked
        .into_iter()
        .filter(|t| !t.ends_with(".meta"))
        .filter(|t| {
            let file = t.rsplit('/').next().unwrap_or(t).to_lowercase();
            let stem = file
                .rsplit_once('.')
                .map(|(s, _)| s.to_owned())
                .unwrap_or_else(|| file.clone());
            file == wanted || stem == wanted
        })
        .collect();
    match matches.len() {
        0 => bail!("nothing called {text:?} in the repository"),
        1 => Ok(Resolve::Found(Target::File {
            path: matches.into_iter().next().unwrap(),
        })),
        _ => Ok(Resolve::Ambiguous(matches)),
    }
}

/// `path` typed in the folder `prefix` → path from the repository root.
pub fn repo_path(prefix: &str, path: &str) -> String {
    let joined = format!("{prefix}{}", path.replace('\\', "/"));
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}
