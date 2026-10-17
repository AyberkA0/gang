//! Words for a conflict: where it is and what changed (DESIGN.md §15.2).

use unity_yaml::{File, GuidIndex, Hierarchy};

use crate::diff::label;
use crate::merge::{Conflict, ConflictKind, Side};
use crate::render::Context;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    /// `Environment › Player › Transform › Position.x`
    pub place: String,
    /// One sentence: why GANG is asking.
    pub why: String,
    /// Names of objects added to a deleted object.
    pub orphans: Vec<String>,
    /// The deleted object's parent, for "move it to …".
    pub parent: Option<String>,
}

pub fn describe(
    conflict: &Conflict,
    base: &File,
    ours: &File,
    theirs: &File,
    guids: Option<&GuidIndex>,
    theirs_name: &str,
) -> Description {
    let lookup = |guid: &str| guids.and_then(|g| g.name(guid));
    let hierarchies = [
        Hierarchy::build_with(ours, &lookup),
        Hierarchy::build_with(theirs, &lookup),
        Hierarchy::build_with(base, &lookup),
    ];
    let object_path = |id: i64| {
        hierarchies
            .iter()
            .find_map(|h| h.owner(id).and_then(|o| h.path(o)))
    };
    let pretty = |path: String| path.replace('/', " › ");

    if let ConflictKind::DeleteModify { deleted_by } = conflict.kind {
        let base_h = &hierarchies[2];
        let name = base_h
            .path(conflict.doc)
            .map(pretty)
            .unwrap_or_else(|| format!("&{}", conflict.doc));
        let short = base_h
            .object(conflict.doc)
            .map(|o| o.name.clone())
            .unwrap_or_else(|| name.clone());
        let orphans: Vec<String> = conflict
            .orphans
            .iter()
            .filter_map(|id| {
                hierarchies.iter().find_map(|h| {
                    h.owner(*id)
                        .and_then(|o| h.object(o).map(|o| o.name.clone()))
                })
            })
            .collect();
        let parent = base_h
            .object(conflict.doc)
            .and_then(|o| o.parent)
            .and_then(|p| base_h.path(p))
            .map(pretty);
        let (deleter, other) = match deleted_by {
            Side::Ours => ("You", theirs_name.to_owned()),
            Side::Theirs => (theirs_name, "you".to_owned()),
        };
        let what = if orphans.is_empty() {
            format!("{other} changed it")
        } else {
            format!("{other} added {} under it", orphans.join(", "))
        };
        return Description {
            place: name,
            why: format!("{deleter} deleted {short}. {}.", capitalize(&what)),
            orphans,
            parent,
        };
    }

    let ctx = Context::new(ours, theirs, guids);
    let property_path: Vec<String> = conflict.path.iter().skip(1).cloned().collect();
    let property = pretty_property(&property_path);
    let object = object_path(conflict.doc).map(pretty);
    let component = ctx.component_name(conflict.doc);
    let is_object = hierarchies.iter().any(|h| h.object(conflict.doc).is_some());
    let place = match (object, is_object) {
        (Some(object), true) => format!("{object} › {property}"),
        (Some(object), false) => format!("{object} › {component} › {property}"),
        (None, _) => format!("{component} › {property}"),
    };
    let why = match conflict.kind {
        ConflictKind::Move => "You both moved it, to different parents.".to_owned(),
        ConflictKind::Order => {
            "You both reordered these UI objects; their order is their drawing order.".to_owned()
        }
        _ => format!("You both changed {}.", property.to_lowercase_first()),
    };
    Description {
        place,
        why,
        orphans: Vec::new(),
        parent: None,
    }
}

fn pretty_property(path: &[String]) -> String {
    // Prefab overrides: `m_Modification/m_Modifications/<property>@<target>`.
    if let Some(pos) = path.iter().position(|p| p == "m_Modifications") {
        if let Some(key) = path.get(pos + 1) {
            let property = key.split('@').next().unwrap_or(key);
            return format!("Override {property}");
        }
    }
    if path.last().is_some_and(|p| p == "(layout)") {
        return "Layout".to_owned();
    }
    label(path)
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

trait LowerFirst {
    fn to_lowercase_first(&self) -> String;
}

impl LowerFirst for String {
    /// `Position.x` stays, `Jump Height` → `jump height`.
    fn to_lowercase_first(&self) -> String {
        if self.contains('.') || self.contains('[') {
            return format!("the {self}");
        }
        format!("the {}", self.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Rules;
    use crate::merge::merge;

    #[test]
    fn describes_a_value_conflict() {
        let text = |v: &str| {
            format!(
                "--- !u!1 &1\nGameObject:\n  m_Component:\n  - component: {{fileID: 2}}\n  m_Name: Player\n--- !u!114 &2\nMonoBehaviour:\n  m_GameObject: {{fileID: 1}}\n  jumpHeight: {v}\n"
            )
        };
        let (b, o, t) = (
            unity_yaml::parse(&text("1")).unwrap(),
            unity_yaml::parse(&text("2")).unwrap(),
            unity_yaml::parse(&text("3")).unwrap(),
        );
        let merged = merge(&b, &o, &t, &Rules::default(), &Default::default());
        let d = describe(&merged.conflicts[0], &b, &o, &t, None, "mehmet");
        assert_eq!(d.place, "Player › MonoBehaviour › Jump Height");
        assert_eq!(d.why, "You both changed the jump height.");
    }
}
