//! Inspector names for Unity properties (DESIGN.md §12.1).

/// Built-in names that differ from what nicifying would give.
const INSPECTOR: &[(&str, &str)] = &[
    ("m_LocalPosition", "Position"),
    ("m_LocalRotation", "Rotation"),
    ("m_LocalScale", "Scale"),
    ("m_LocalEulerAnglesHint", "Rotation"),
    ("m_IsActive", "Active"),
    ("m_TagString", "Tag"),
    ("m_Layer", "Layer"),
    ("m_StaticEditorFlags", "Static"),
    ("m_AnchorMin", "Anchor Min"),
    ("m_AnchorMax", "Anchor Max"),
    ("m_AnchoredPosition", "Position"),
    ("m_SizeDelta", "Size"),
    ("m_Materials", "Materials"),
    ("m_Mesh", "Mesh"),
    ("m_Sprite", "Sprite"),
];

/// Unity's built-in UI scripts (com.unity.ugui), which are not in the
/// project's own `.meta` files. GUIDs confirmed against real projects.
pub const LAYOUT_GROUPS: &[&str] = &[
    "59f8146938fff824cb5fd77236b75775", // VerticalLayoutGroup
    "30649d3a9faa99c48a7b1166b86bf2a0", // HorizontalLayoutGroup
    "8a8695521f0d02e499659fee002a26c2", // GridLayoutGroup
];
pub const SIZE_FITTERS: &[&str] = &[
    "3245ec927659c4140ac4f8d17403cc18", // ContentSizeFitter
];

pub fn builtin_script_name(guid: &str) -> Option<&'static str> {
    Some(match guid {
        "59f8146938fff824cb5fd77236b75775" => "VerticalLayoutGroup",
        "30649d3a9faa99c48a7b1166b86bf2a0" => "HorizontalLayoutGroup",
        "8a8695521f0d02e499659fee002a26c2" => "GridLayoutGroup",
        "3245ec927659c4140ac4f8d17403cc18" => "ContentSizeFitter",
        _ => return None,
    })
}

/// The name the Inspector shows for a serialized field.
pub fn display(field: &str) -> String {
    if let Some((_, name)) = INSPECTOR.iter().find(|(key, _)| *key == field) {
        return (*name).to_owned();
    }
    nicify(field)
}

/// Unity's `ObjectNames.NicifyVariableName`: `m_moveSpeed` → `Move Speed`,
/// `_HTTPPort` → `HTTP Port`, `kMaxCount` → `Max Count`.
pub fn nicify(field: &str) -> String {
    let mut name = field;
    if let Some(rest) = name.strip_prefix("m_") {
        name = rest;
    }
    name = name.trim_start_matches('_');
    if name.len() > 1
        && name.starts_with('k')
        && name[1..].starts_with(|c: char| c.is_ascii_uppercase())
    {
        name = &name[1..];
    }
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        if i > 0 && !out.ends_with(' ') {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let boundary = (c.is_uppercase() && (prev.is_lowercase() || prev.is_ascii_digit()))
                || (c.is_uppercase()
                    && prev.is_uppercase()
                    && next.is_some_and(char::is_lowercase))
                || (c.is_ascii_digit() && prev.is_alphabetic());
            if boundary {
                out.push(' ');
            }
        }
        if out.is_empty() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nicifies_like_unity() {
        assert_eq!(nicify("m_moveSpeed"), "Move Speed");
        assert_eq!(nicify("moveSpeed"), "Move Speed");
        assert_eq!(nicify("_HTTPPort"), "HTTP Port");
        assert_eq!(nicify("kMaxCount"), "Max Count");
        assert_eq!(nicify("jumpHeight2"), "Jump Height 2");
        assert_eq!(nicify("m_Intensity"), "Intensity");
        assert_eq!(nicify("snake_case_name"), "Snake case name");
    }

    #[test]
    fn inspector_names() {
        assert_eq!(display("m_LocalPosition"), "Position");
        assert_eq!(display("m_Color"), "Color");
    }
}
