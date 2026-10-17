//! Merge scenarios, one per root cause in DESIGN.md §13 and the scene
//! problems in the design discussion. Every result must also be a valid
//! scene: references resolve and parents and children agree.

use std::collections::{HashMap, HashSet};

use gang_merge::diff::Rules;
use gang_merge::merge::{Choice, ConflictKind, Decisions, Merged, Side, merge};
use unity_yaml::{File, Hierarchy, Value};

fn game_object(id: i64, name: &str, components: &[i64]) -> String {
    let mut text = format!("--- !u!1 &{id}\nGameObject:\n  m_ObjectHideFlags: 0\n  m_Component:\n");
    for c in components {
        text.push_str(&format!("  - component: {{fileID: {c}}}\n"));
    }
    text.push_str(&format!(
        "  m_Layer: 0\n  m_Name: {name}\n  m_IsActive: 1\n"
    ));
    text
}

fn transform(id: i64, object: i64, father: i64, children: &[i64]) -> String {
    let mut text = format!(
        "--- !u!4 &{id}\nTransform:\n  m_ObjectHideFlags: 0\n  m_GameObject: {{fileID: {object}}}\n  m_LocalRotation: {{x: 0, y: 0, z: 0, w: 1}}\n  m_LocalPosition: {{x: 0, y: 0, z: 0}}\n  m_LocalScale: {{x: 1, y: 1, z: 1}}\n"
    );
    if children.is_empty() {
        text.push_str("  m_Children: []\n");
    } else {
        text.push_str("  m_Children:\n");
        for c in children {
            text.push_str(&format!("  - {{fileID: {c}}}\n"));
        }
    }
    text.push_str(&format!("  m_Father: {{fileID: {father}}}\n"));
    text
}

fn behaviour(id: i64, object: i64, fields: &str) -> String {
    format!(
        "--- !u!114 &{id}\nMonoBehaviour:\n  m_GameObject: {{fileID: {object}}}\n  m_Script: {{fileID: 11500000, guid: aaaa, type: 3}}\n{fields}"
    )
}

const HEADER: &str = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n";

/// Environment (10/11) with Player (20/21, PlayerController 22) under it.
fn base_scene() -> String {
    [
        HEADER.to_owned(),
        game_object(10, "Environment", &[11]),
        transform(11, 10, 0, &[21]),
        game_object(20, "Player", &[21, 22]),
        transform(21, 20, 11, &[]),
        behaviour(22, 20, "  jumpHeight: 2.5\n  speed: 4\n"),
    ]
    .concat()
}

fn parse(text: &str) -> File {
    unity_yaml::parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn run(base: &str, ours: &str, theirs: &str, decisions: &Decisions) -> Merged {
    let merged = merge(
        &parse(base),
        &parse(ours),
        &parse(theirs),
        &Rules::default(),
        decisions,
    );
    let text = merged.file.to_text();
    assert_valid(&parse(&text));
    merged
}

fn auto(base: &str, ours: &str, theirs: &str) -> (String, Merged) {
    let merged = run(base, ours, theirs, &HashMap::new());
    (merged.file.to_text(), merged)
}

/// References resolve, children lists match fathers, components match.
fn assert_valid(file: &File) {
    let ids: HashSet<i64> = file.docs.iter().filter_map(|d| d.file_id).collect();
    for doc in &file.docs {
        let (Some(id), Some(body)) = (doc.file_id, doc.body()) else {
            continue;
        };
        for field in ["m_Father", "m_GameObject"] {
            if let Some(r) = body.get(field).and_then(Value::as_ref) {
                assert!(
                    r.file_id == 0 || ids.contains(&r.file_id),
                    "{id}.{field} → missing {}\n{}",
                    r.file_id,
                    file.to_text()
                );
            }
        }
        if let Some(children) = body.get("m_Children").and_then(Value::as_seq) {
            for child in children.iter().filter_map(|c| c.value().as_ref()) {
                let father = file
                    .doc(child.file_id)
                    .and_then(|d| d.body())
                    .and_then(|b| b.get("m_Father"))
                    .and_then(Value::as_ref);
                assert_eq!(
                    father.map(|f| f.file_id),
                    Some(id),
                    "child {} of {id} has another father\n{}",
                    child.file_id,
                    file.to_text()
                );
            }
        }
    }
}

fn names(text: &str, parent: &str) -> Vec<String> {
    let file = parse(text);
    let h = Hierarchy::build(&file);
    let id = h.resolve(parent).unwrap();
    h.object(id)
        .unwrap()
        .children
        .iter()
        .map(|c| h.object(*c).unwrap().name.clone())
        .collect()
}

fn field(text: &str, doc: i64, key: &str) -> String {
    parse(text)
        .doc(doc)
        .unwrap()
        .body()
        .unwrap()
        .get(key)
        .unwrap()
        .text()
        .unwrap()
}

#[test]
fn both_add_children_to_the_same_parent() {
    let base = base_scene();
    let ours = base.replace("  - {fileID: 21}\n", "  - {fileID: 21}\n  - {fileID: 31}\n")
        + &game_object(30, "Tree", &[31])
        + &transform(31, 30, 11, &[]);
    let theirs = base.replace("  - {fileID: 21}\n", "  - {fileID: 21}\n  - {fileID: 41}\n")
        + &game_object(40, "Enemy", &[41])
        + &transform(41, 40, 11, &[]);
    let (text, merged) = auto(&base, &ours, &theirs);
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    assert_eq!(names(&text, "Environment"), vec!["Player", "Tree", "Enemy"]);
}

#[test]
fn different_properties_of_one_object_merge() {
    let base = base_scene();
    let ours = base.replace("  m_LocalPosition: {x: 0, y: 0, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []", "  m_LocalPosition: {x: 3, y: 0, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []");
    let theirs = base.replace(
        "  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []",
        "  m_LocalScale: {x: 2, y: 2, z: 2}\n  m_Children: []",
    );
    let (text, merged) = auto(&base, &ours, &theirs);
    assert!(merged.conflicts.is_empty());
    assert_eq!(field(&text, 21, "m_LocalPosition"), "{x: 3, y: 0, z: 0}");
    assert_eq!(field(&text, 21, "m_LocalScale"), "{x: 2, y: 2, z: 2}");
}

#[test]
fn different_fields_of_one_vector_merge() {
    let base = base_scene();
    let player = |pos: &str| {
        base.replace("  m_LocalPosition: {x: 0, y: 0, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []", &format!("  m_LocalPosition: {pos}\n  m_LocalScale: {{x: 1, y: 1, z: 1}}\n  m_Children: []"))
    };
    let (text, merged) = auto(
        &base,
        &player("{x: 5, y: 0, z: 0}"),
        &player("{x: 0, y: 0, z: 7}"),
    );
    assert!(merged.conflicts.is_empty());
    assert_eq!(field(&text, 21, "m_LocalPosition"), "{x: 5, y: 0, z: 7}");
}

#[test]
fn the_same_value_changed_twice_is_a_conflict() {
    let base = base_scene();
    let ours = base.replace("jumpHeight: 2.5", "jumpHeight: 3");
    let theirs = base.replace("jumpHeight: 2.5", "jumpHeight: 4");
    let (text, merged) = auto(&base, &ours, &theirs);
    assert_eq!(merged.conflicts.len(), 1);
    let conflict = &merged.conflicts[0];
    assert_eq!(conflict.kind, ConflictKind::Value);
    assert_eq!(
        (
            conflict.base.as_deref(),
            conflict.ours.as_deref(),
            conflict.theirs.as_deref()
        ),
        (Some("2.5"), Some("3"), Some("4"))
    );
    assert_eq!(field(&text, 22, "jumpHeight"), "3", "ours by default");

    let decisions = HashMap::from([(conflict.id.clone(), Choice::Theirs)]);
    assert_eq!(
        field(
            &run(&base, &ours, &theirs, &decisions).file.to_text(),
            22,
            "jumpHeight"
        ),
        "4"
    );
    let decisions = HashMap::from([(conflict.id.clone(), Choice::Value("4.5".into()))]);
    assert_eq!(
        field(
            &run(&base, &ours, &theirs, &decisions).file.to_text(),
            22,
            "jumpHeight"
        ),
        "4.5"
    );
}

#[test]
fn rotation_fields_are_coupled() {
    let base = base_scene();
    let rot = |r: &str| {
        base.replace("  m_LocalRotation: {x: 0, y: 0, z: 0, w: 1}\n  m_LocalPosition: {x: 0, y: 0, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []", &format!("  m_LocalRotation: {r}\n  m_LocalPosition: {{x: 0, y: 0, z: 0}}\n  m_LocalScale: {{x: 1, y: 1, z: 1}}\n  m_Children: []"))
    };
    let (_, merged) = auto(
        &base,
        &rot("{x: 0.7071, y: 0, z: 0, w: 0.7071}"),
        &rot("{x: 0, y: 0.7071, z: 0, w: 0.7071}"),
    );
    assert_eq!(merged.conflicts.len(), 1);
    assert_eq!(
        merged.conflicts[0].path.last().map(String::as_str),
        Some("m_LocalRotation")
    );
}

#[test]
fn deleted_object_with_changes_on_the_other_side() {
    let base = base_scene();
    // Ours deletes Player; theirs changes its script.
    let ours = [
        HEADER.to_owned(),
        game_object(10, "Environment", &[11]),
        transform(11, 10, 0, &[]),
    ]
    .concat();
    let theirs = base.replace("jumpHeight: 2.5", "jumpHeight: 9");
    let (text, merged) = auto(&base, &ours, &theirs);
    assert_eq!(merged.conflicts.len(), 1);
    assert_eq!(
        merged.conflicts[0].kind,
        ConflictKind::DeleteModify {
            deleted_by: Side::Ours
        }
    );
    assert!(
        names(&text, "Environment").is_empty(),
        "ours deleted it, so by default it stays deleted"
    );

    let decisions = HashMap::from([(merged.conflicts[0].id.clone(), Choice::Keep)]);
    let kept = run(&base, &ours, &theirs, &decisions).file.to_text();
    assert_eq!(names(&kept, "Environment"), vec!["Player"]);
    assert_eq!(field(&kept, 22, "jumpHeight"), "9");
}

#[test]
fn child_added_under_an_object_the_other_side_deleted() {
    let base = base_scene();
    // Theirs deletes Player; ours adds Weapon under Player.
    let theirs = [
        HEADER.to_owned(),
        game_object(10, "Environment", &[11]),
        transform(11, 10, 0, &[]),
    ]
    .concat();
    let ours = base.replace(
        "  m_Children: []\n  m_Father: {fileID: 11}",
        "  m_Children:\n  - {fileID: 51}\n  m_Father: {fileID: 11}",
    ) + &game_object(50, "Weapon", &[51])
        + &transform(51, 50, 21, &[]);
    let (text, merged) = auto(&base, &ours, &theirs);
    assert_eq!(merged.conflicts.len(), 1);
    let conflict = &merged.conflicts[0];
    assert_eq!(
        conflict.kind,
        ConflictKind::DeleteModify {
            deleted_by: Side::Theirs
        }
    );
    assert_eq!(conflict.orphans, vec![51]);
    assert_eq!(
        names(&text, "Environment/Player"),
        vec!["Weapon"],
        "kept by default: ours did not delete it"
    );

    let delete = run(
        &base,
        &ours,
        &theirs,
        &HashMap::from([(conflict.id.clone(), Choice::Delete)]),
    )
    .file
    .to_text();
    assert!(!delete.contains("Weapon") && !delete.contains("m_Name: Player"));

    let reparent = run(
        &base,
        &ours,
        &theirs,
        &HashMap::from([(conflict.id.clone(), Choice::Reparent)]),
    )
    .file
    .to_text();
    assert_eq!(names(&reparent, "Environment"), vec!["Weapon"]);
    assert!(!reparent.contains("m_Name: Player"));
}

#[test]
fn moved_to_different_parents() {
    // Player, plus two folders A (60/61) and B (70/71) under Environment.
    let base = [
        HEADER.to_owned(),
        game_object(10, "Environment", &[11]),
        transform(11, 10, 0, &[21, 61, 71]),
        game_object(20, "Player", &[21]),
        transform(21, 20, 11, &[]),
        game_object(60, "A", &[61]),
        transform(61, 60, 11, &[]),
        game_object(70, "B", &[71]),
        transform(71, 70, 11, &[]),
    ]
    .concat();
    let move_to = |folder: i64| {
        [
            HEADER.to_owned(),
            game_object(10, "Environment", &[11]),
            transform(11, 10, 0, &[61, 71]),
            game_object(20, "Player", &[21]),
            transform(21, 20, folder, &[]),
            game_object(60, "A", &[61]),
            transform(61, 60, 11, if folder == 61 { &[21] } else { &[] }),
            game_object(70, "B", &[71]),
            transform(71, 70, 11, if folder == 71 { &[21] } else { &[] }),
        ]
        .concat()
    };
    let (text, merged) = auto(&base, &move_to(61), &move_to(71));
    assert_eq!(merged.conflicts.len(), 1);
    assert_eq!(merged.conflicts[0].kind, ConflictKind::Move);
    assert_eq!(names(&text, "Environment/A"), vec!["Player"]);
    assert!(
        names(&text, "Environment/B").is_empty(),
        "Player is in one place only"
    );

    let theirs = run(
        &base,
        &move_to(61),
        &move_to(71),
        &HashMap::from([(merged.conflicts[0].id.clone(), Choice::Theirs)]),
    )
    .file
    .to_text();
    assert_eq!(names(&theirs, "Environment/B"), vec!["Player"]);
    assert!(names(&theirs, "Environment/A").is_empty());
}

#[test]
fn both_add_components_to_one_object() {
    let base = base_scene();
    let ours = base.replace(
        "  - component: {fileID: 22}\n",
        "  - component: {fileID: 22}\n  - component: {fileID: 23}\n",
    ) + &behaviour(23, 20, "  a: 1\n");
    let theirs = base.replace(
        "  - component: {fileID: 22}\n",
        "  - component: {fileID: 22}\n  - component: {fileID: 24}\n",
    ) + &behaviour(24, 20, "  b: 2\n");
    let (text, merged) = auto(&base, &ours, &theirs);
    assert!(merged.conflicts.is_empty());
    let file = parse(&text);
    let h = Hierarchy::build(&file);
    assert_eq!(h.object(20).unwrap().components, vec![21, 22, 23, 24]);
}

#[test]
fn prefab_overrides_merge_by_target_and_property() {
    let instance = |mods: &str| {
        format!(
            "{HEADER}--- !u!1001 &900\nPrefabInstance:\n  m_Modification:\n    m_TransformParent: {{fileID: 0}}\n    m_Modifications:\n{mods}  m_SourcePrefab: {{fileID: 100100000, guid: bbbb, type: 3}}\n"
        )
    };
    let m = |prop: &str, value: &str| {
        format!(
            "    - target: {{fileID: 5, guid: bbbb, type: 3}}\n      propertyPath: {prop}\n      value: {value}\n      objectReference: {{fileID: 0}}\n"
        )
    };
    let base = instance(&m("m_Name", "Bridge"));
    let ours = instance(&(m("m_Name", "Bridge") + &m("m_LocalPosition.x", "4")));
    let theirs = instance(&(m("m_Name", "Bridge") + &m("m_LocalPosition.y", "2")));
    let (text, merged) = auto(&base, &ours, &theirs);
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    assert!(text.contains("m_LocalPosition.x") && text.contains("m_LocalPosition.y"));

    let ours = instance(&m("m_Name", "Old Bridge"));
    let theirs = instance(&m("m_Name", "Rope Bridge"));
    let (_, merged) = auto(&base, &ours, &theirs);
    assert_eq!(merged.conflicts.len(), 1);
}

#[test]
fn noise_never_conflicts() {
    let settings = |color: &str| {
        format!("{HEADER}--- !u!104 &2\nRenderSettings:\n  m_IndirectSpecularColor: {color}\n")
    };
    let (_, merged) = auto(
        &settings("{r: 0, g: 0, b: 0, a: 1}"),
        &settings("{r: 0.1, g: 0.2, b: 0.3, a: 1}"),
        &settings("{r: 0.4, g: 0.5, b: 0.6, a: 1}"),
    );
    assert!(merged.conflicts.is_empty());
}

#[test]
fn ui_sibling_order_conflicts() {
    let rect = |order: &[i64]| {
        let mut text = format!(
            "{HEADER}--- !u!224 &11\nRectTransform:\n  m_GameObject: {{fileID: 10}}\n  m_Children:\n"
        );
        for c in order {
            text.push_str(&format!("  - {{fileID: {c}}}\n"));
        }
        text.push_str("  m_Father: {fileID: 0}\n");
        for c in [21, 31, 41] {
            text.push_str(&format!("--- !u!224 &{c}\nRectTransform:\n  m_GameObject: {{fileID: {}}}\n  m_Children: []\n  m_Father: {{fileID: 11}}\n", c - 1));
        }
        for (go, t) in [(10, 11), (20, 21), (30, 31), (40, 41)] {
            text.push_str(&game_object(go, &format!("UI{go}"), &[t]));
        }
        text
    };
    let (_, merged) = auto(
        &rect(&[21, 31, 41]),
        &rect(&[31, 21, 41]),
        &rect(&[21, 41, 31]),
    );
    assert_eq!(merged.conflicts.len(), 1);
    assert_eq!(merged.conflicts[0].kind, ConflictKind::Order);
}

#[test]
fn layers_merge_index_by_index() {
    let tags = |l8: &str, l9: &str| {
        format!(
            "{HEADER}--- !u!78 &1\nTagManager:\n  tags: []\n  layers:\n  - Default\n  - \n  - {l8}\n  - {l9}\n"
        )
    };
    let (text, merged) = auto(&tags("", ""), &tags("Pickup", ""), &tags("", "UI3D"));
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    assert!(text.contains("  - Pickup\n  - UI3D\n"), "{text}");
}

#[test]
fn same_file_id_added_by_both_sides() {
    let base = base_scene();
    let ours = base.replace("  - {fileID: 21}\n", "  - {fileID: 21}\n  - {fileID: 81}\n")
        + &game_object(80, "Rock", &[81])
        + &transform(81, 80, 11, &[]);
    let theirs = base.replace("  - {fileID: 21}\n", "  - {fileID: 21}\n  - {fileID: 81}\n")
        + &game_object(80, "Bush", &[81])
        + &transform(81, 80, 11, &[]);
    let (text, merged) = auto(&base, &ours, &theirs);
    assert!(merged.conflicts.is_empty(), "{:?}", merged.conflicts);
    let mut children = names(&text, "Environment");
    children.sort();
    assert_eq!(children, vec!["Bush", "Player", "Rock"]);
}

#[test]
fn meta_files_merge_by_key() {
    let meta = |filter: &str, size: &str| {
        format!(
            "fileFormatVersion: 2\nguid: abc\nTextureImporter:\n  filterMode: {filter}\n  maxTextureSize: {size}\n"
        )
    };
    let (text, merged) = auto(&meta("1", "2048"), &meta("0", "2048"), &meta("1", "1024"));
    assert!(merged.conflicts.is_empty());
    assert_eq!(text, meta("0", "1024"));
}

#[test]
fn untouched_parts_keep_their_bytes() {
    let base = base_scene();
    let ours = base.replace("speed: 4", "speed: 5");
    let theirs = base.replace("m_Name: Environment", "m_Name: World");
    let (text, merged) = auto(&base, &ours, &theirs);
    assert!(merged.conflicts.is_empty());
    assert_eq!(
        text,
        base.replace("speed: 4", "speed: 5")
            .replace("m_Name: Environment", "m_Name: World")
    );
}
