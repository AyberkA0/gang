//! Human-readable lines for a semantic diff (DESIGN.md §12.1):
//! `~ Player › Transform › Position.x  0 → 2`.

use std::collections::BTreeSet;

use unity_yaml::{File, FileId, GuidIndex, Hierarchy, Value};

use crate::diff::{DocChange, label};
use crate::names;

/// Both versions of a file, with what is needed to name things.
pub struct Context<'a> {
    pub old: &'a File,
    pub new: &'a File,
    pub old_hierarchy: Hierarchy,
    pub new_hierarchy: Hierarchy,
    pub guids: Option<&'a GuidIndex>,
    /// Layer names, for collision matrices (`Pickup × Water`).
    pub layer_names: Vec<String>,
}

impl<'a> Context<'a> {
    pub fn new(old: &'a File, new: &'a File, guids: Option<&'a GuidIndex>) -> Context<'a> {
        let lookup = |guid: &str| guids.and_then(|g| g.name(guid));
        Context {
            old,
            new,
            old_hierarchy: Hierarchy::build_with(old, &lookup),
            new_hierarchy: Hierarchy::build_with(new, &lookup),
            guids,
            layer_names: Vec::new(),
        }
    }

    fn object_path(&self, id: FileId) -> Option<String> {
        self.new_hierarchy
            .path(id)
            .or_else(|| self.old_hierarchy.path(id))
    }

    fn owner(&self, doc: FileId) -> Option<FileId> {
        self.new_hierarchy
            .owner(doc)
            .or_else(|| self.old_hierarchy.owner(doc))
    }

    fn doc(&self, id: FileId) -> Option<&unity_yaml::Doc> {
        self.new.doc(id).or_else(|| self.old.doc(id))
    }

    /// `Transform`, or a script's class name for a MonoBehaviour.
    pub fn component_name(&self, id: FileId) -> String {
        let Some(doc) = self.doc(id) else {
            return "Component".to_owned();
        };
        let type_name = doc.type_name().unwrap_or("Component").to_owned();
        if type_name != "MonoBehaviour" {
            return type_name;
        }
        doc.body()
            .and_then(|b| b.get("m_Script"))
            .and_then(Value::as_ref)
            .and_then(|r| r.guid)
            .and_then(|guid| {
                self.guids
                    .and_then(|g| g.name(&guid))
                    .or_else(|| names::builtin_script_name(&guid).map(str::to_owned))
            })
            .unwrap_or(type_name)
    }

    /// A value for display: vectors as `(x, y, z)`, references by name.
    pub fn show(&self, value: &str) -> String {
        let scalar = unity_yaml::Scalar::raw(value);
        if let Some(entries) = scalar.flow_entries() {
            let get = |k: &str| {
                entries
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            };
            if let Some(file_id) = get("fileID") {
                if file_id == "0" {
                    return "None".to_owned();
                }
                if let Some(guid) = get("guid") {
                    return self
                        .guids
                        .and_then(|g| g.name(&guid))
                        .unwrap_or_else(|| format!("asset {}", &guid[..guid.len().min(8)]));
                }
                if let Ok(id) = file_id.parse::<FileId>() {
                    if let Some(path) = self.owner(id).and_then(|o| self.object_path(o)) {
                        return path;
                    }
                }
                return format!("&{file_id}");
            }
            let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
            if keys == ["x", "y"]
                || keys == ["x", "y", "z"]
                || keys == ["x", "y", "z", "w"]
                || keys == ["r", "g", "b", "a"]
            {
                let values: Vec<&str> = entries.iter().map(|(_, v)| v.as_str()).collect();
                return format!("({})", values.join(", "));
            }
        }
        if value.is_empty() {
            "\"\"".to_owned()
        } else {
            value.to_owned()
        }
    }
}

/// Fields that are bookkeeping, shown through other lines.
const HIDDEN: &[&str] = &[
    "m_Component",
    "m_Children",
    "m_RootOrder",
    "m_Roots",
    "serializedVersion",
    "m_ObjectHideFlags",
];

/// One line per change, grouped by object. Noise is left out unless asked.
pub fn render(changes: &[DocChange], ctx: &Context<'_>, include_noise: bool) -> Vec<String> {
    let added: BTreeSet<FileId> = changes
        .iter()
        .filter_map(|c| match c {
            DocChange::Added(id) => Some(*id),
            _ => None,
        })
        .collect();
    let removed: BTreeSet<FileId> = changes
        .iter()
        .filter_map(|c| match c {
            DocChange::Removed(id) => Some(*id),
            _ => None,
        })
        .collect();
    let mut lines = Vec::new();

    for change in changes {
        match change {
            DocChange::Added(id) | DocChange::Removed(id) => {
                let (sign, hierarchy, others) = match change {
                    DocChange::Added(_) => ("+", &ctx.new_hierarchy, &added),
                    _ => ("−", &ctx.old_hierarchy, &removed),
                };
                if hierarchy.object(*id).is_some() {
                    // Show an added or removed object once, not its children.
                    let parent_too = hierarchy
                        .object(*id)
                        .and_then(|o| o.parent)
                        .is_some_and(|p| others.contains(&p));
                    if !parent_too {
                        lines.push(format!(
                            "{sign} {}",
                            hierarchy.path(*id).unwrap_or_default()
                        ));
                    }
                } else if let Some(owner) = hierarchy.owner(*id) {
                    if !others.contains(&owner) && owner != *id {
                        let what = ctx.component_name(*id);
                        lines.push(format!(
                            "{sign} {} › {what}",
                            hierarchy.path(owner).unwrap_or_default()
                        ));
                    }
                } else if let Some(name) = ctx.doc(*id).and_then(|d| d.type_name()) {
                    lines.push(format!("{sign} {name}"));
                }
            }
            DocChange::Changed { id, props } => {
                let object = ctx.owner(*id).and_then(|o| ctx.object_path(o));
                let is_object = ctx.new_hierarchy.object(*id).is_some();
                let place = match (&object, is_object) {
                    (Some(path), true) => path.clone(),
                    (Some(path), false) => format!("{path} › {}", ctx.component_name(*id)),
                    (None, _) => ctx
                        .doc(*id)
                        .and_then(|d| d.type_name())
                        .unwrap_or("Settings")
                        .to_owned(),
                };
                if props
                    .iter()
                    .any(|p| p.path.first().is_some_and(|f| f == "m_Father"))
                {
                    let parent = ctx
                        .owner(*id)
                        .and_then(|o| ctx.new_hierarchy.object(o))
                        .and_then(|o| o.parent)
                        .and_then(|p| ctx.new_hierarchy.path(p))
                        .unwrap_or_else(|| "the scene root".to_owned());
                    lines.push(format!(
                        "~ {} moved under {parent}",
                        object.clone().unwrap_or_default()
                    ));
                }
                for prop in props {
                    let field = prop.path.first().map(String::as_str).unwrap_or("");
                    if HIDDEN.contains(&field)
                        || field == "m_Father"
                        || field == "m_GameObject"
                        || (prop.noise && !include_noise)
                    {
                        continue;
                    }
                    if field == crate::matrix::KEY {
                        let rows = |v: &Option<String>| v.as_deref().and_then(crate::matrix::parse);
                        if let (Some(old), Some(new)) = (rows(&prop.old), rows(&prop.new)) {
                            for cell in crate::matrix::describe(&old, &new, &ctx.layer_names) {
                                lines.push(format!("~ {place} › Collision  {cell}"));
                            }
                            continue;
                        }
                    }
                    let noise = if prop.noise { "  (noise)" } else { "" };
                    let text = match (&prop.old, &prop.new) {
                        (Some(old), Some(new)) => format!("{} → {}", ctx.show(old), ctx.show(new)),
                        (None, Some(new)) => format!("+ {}", ctx.show(new)),
                        (Some(old), None) => format!("− {}", ctx.show(old)),
                        (None, None) => String::new(),
                    };
                    lines.push(format!("~ {place} › {}  {text}{noise}", label(&prop.path)));
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Rules, diff};

    const OLD: &str = "--- !u!1 &1
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
";

    #[test]
    fn renders_objects_components_and_values() {
        let new = format!(
            "{}--- !u!1 &3\nGameObject:\n  m_Component:\n  - component: {{fileID: 4}}\n  m_Name: Tree\n--- !u!4 &4\nTransform:\n  m_GameObject: {{fileID: 3}}\n  m_Children: []\n  m_Father: {{fileID: 0}}\n",
            OLD.replace("{x: 0, y: 1, z: 0}", "{x: 2, y: 1, z: 0}")
        );
        let (old, new) = (
            unity_yaml::parse(OLD).unwrap(),
            unity_yaml::parse(&new).unwrap(),
        );
        let ctx = Context::new(&old, &new, None);
        let lines = render(&diff(&old, &new, &Rules::default()), &ctx, false);
        assert_eq!(
            lines,
            vec!["~ Player › Transform › Position.x  0 → 2", "+ Tree"]
        );
    }
}
