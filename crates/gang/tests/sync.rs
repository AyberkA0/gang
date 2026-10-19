//! P7–P8: semantic merges through git, gang bang and gang sync, between
//! two clones of one remote.

mod common;

use common::{GANG, Sandbox};

const SCENE: &str = "Assets/Scenes/Level1.unity";

fn scene(player_x: &str, jump: &str, extra: &str) -> String {
    format!(
        "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!1 &100
GameObject:
  m_Component:
  - component: {{fileID: 101}}
  m_Name: Environment
--- !u!4 &101
Transform:
  m_GameObject: {{fileID: 100}}
  m_Children:
  - {{fileID: 201}}
  m_Father: {{fileID: 0}}
--- !u!1 &200
GameObject:
  m_Component:
  - component: {{fileID: 201}}
  - component: {{fileID: 202}}
  m_Name: Player
--- !u!4 &201
Transform:
  m_GameObject: {{fileID: 200}}
  m_LocalPosition: {{x: {player_x}, y: 1, z: 0}}
  m_Children: []
  m_Father: {{fileID: 101}}
--- !u!114 &202
MonoBehaviour:
  m_GameObject: {{fileID: 200}}
  m_Script: {{fileID: 11500000, guid: aaaa, type: 3}}
  jumpHeight: {jump}
{extra}"
    )
}

struct Team {
    sandbox: Sandbox,
}

impl Team {
    fn new() -> Team {
        Team::with(&["alice", "bob"])
    }

    fn with(users: &[&str]) -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        sandbox.write(SCENE, &scene("0", "2.5", ""));
        sandbox.write(
            &format!("{SCENE}.meta"),
            "fileFormatVersion: 2\nguid: 11112222333344445555666677778888\n",
        );
        sandbox.write(
            "Packages/manifest.json",
            "{\n  \"dependencies\": {\n    \"com.unity.ugui\": \"2.0.0\"\n  }\n}\n",
        );
        sandbox.write(
            "Assets/Scripts/Player.cs",
            "class Player\n{\n    int speed = 1;\n}\n",
        );
        sandbox.write(
            "Assets/Scripts/Player.cs.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000aaaa\n",
        );
        sandbox.write(
            "Assets/Scripts.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000bbbb\nfolderAsset: yes\n",
        );
        assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
        sandbox.git(&["add", "-A"]);
        sandbox.git(&["commit", "-q", "-m", "start"]);
        let remote = sandbox.dir.path().join("remote.git");
        sandbox.git(&["clone", "-q", "--bare", ".", remote.to_str().unwrap()]);
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        for user in users {
            let clone = sandbox.run_in("", GANG, &["clone", "-q", &url, user]);
            assert_eq!(clone.code, Some(0), "{clone:?}");
            sandbox.run_in(user, "git", &["config", "gang.user", user]);
            sandbox.run_in(user, "git", &["config", "user.name", user]);
        }
        Team { sandbox }
    }

    fn gang(&self, user: &str, args: &[&str]) -> common::Outcome {
        self.sandbox.run_in(user, GANG, args)
    }

    fn write(&self, user: &str, path: &str, text: &str) {
        std::fs::write(self.sandbox.dir.path().join(user).join(path), text).unwrap();
    }

    fn read(&self, user: &str, path: &str) -> String {
        std::fs::read_to_string(self.sandbox.dir.path().join(user).join(path)).unwrap()
    }

    fn sync(&self, user: &str, message: &str) -> common::Outcome {
        self.gang(user, &["sync", "-m", message])
    }
}

#[test]
fn different_changes_to_one_scene_merge_without_questions() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("5", "2.5", ""));
    assert_eq!(team.sync("alice", "move player").code, Some(0));
    team.write("bob", SCENE, &scene("0", "3", ""));
    let sync = team.sync("bob", "jump");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stdout.contains("Merged automatically from Test"),
        "{sync:?}"
    );
    assert_eq!(team.read("bob", SCENE), scene("5", "3", ""));
    assert!(!team.read("bob", SCENE).contains("<<<<<<<"));

    assert_eq!(team.sync("alice", "-").code, Some(0));
    assert_eq!(team.read("alice", SCENE), scene("5", "3", ""));
}

/// Every conflict is answered on its own: `--answer` settles exactly one
/// question per run, and there is no option that answers them all.
#[test]
fn every_question_is_answered_on_its_own() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("1", "3", ""));
    assert_eq!(team.sync("alice", "alice tuning").code, Some(0));
    team.write("bob", SCENE, &scene("2", "4", ""));
    assert_eq!(team.sync("bob", "bob tuning").code, Some(1));

    let first = team.gang("bob", &["bang", "--answer", "theirs"]);
    assert_eq!(first.code, Some(1), "{first:?}");
    assert!(first.stdout.contains("conflict 1/2"), "{first:?}");
    assert!(first.stdout.contains("1 question left"), "{first:?}");
    let list = team.gang("bob", &["bang", "--list"]);
    assert_eq!(list.stdout.matches("(open)").count(), 1, "{list:?}");
    assert_eq!(list.stdout.matches("(answered)").count(), 1, "{list:?}");

    let wrong = team.gang("bob", &["bang", "--answer", "delete"]);
    assert_ne!(wrong.code, Some(0), "{wrong:?}");
    assert!(wrong.stderr.contains("does not fit"), "{wrong:?}");

    let second = team.gang("bob", &["bang", "--answer", "mine"]);
    assert_eq!(second.code, Some(0), "{second:?}");
    assert!(second.stdout.contains("conflict 2/2"), "{second:?}");
    let text = team.read("bob", SCENE);
    assert!(!text.contains("<<<<<<<"), "{text}");
    let both: Vec<&str> = [("x: 1", "x: 2"), ("jumpHeight: 3", "jumpHeight: 4")]
        .iter()
        .map(|(a, b)| {
            if text.contains(a) {
                "theirs"
            } else if text.contains(b) {
                "mine"
            } else {
                "?"
            }
        })
        .collect();
    assert_eq!(both, ["theirs", "mine"], "{text}");

    let help = team.gang("bob", &["help", "bang"]);
    assert!(!help.stdout.contains("--prefer"), "{help:?}");
}

#[test]
fn same_property_asks_and_bang_resolves() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("0", "3", ""));
    assert_eq!(team.sync("alice", "alice jump").code, Some(0));
    team.write("bob", SCENE, &scene("0", "4", ""));
    let sync = team.sync("bob", "bob jump");
    assert_eq!(sync.code, Some(1), "{sync:?}");
    assert!(sync.stderr.contains("Run `gang bang`"), "{sync:?}");
    // The file is valid YAML with bob's value meanwhile, never conflict markers.
    assert_eq!(team.read("bob", SCENE), scene("0", "4", ""));

    let list = team.gang("bob", &["bang", "--list"]);
    assert!(
        list.stdout.contains("Player › MonoBehaviour › Jump Height"),
        "{list:?}"
    );
    assert!(
        list.stdout.contains("You both changed the jump height."),
        "{list:?}"
    );

    let bang = team.gang("bob", &["bang", "--answer", "theirs"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    assert_eq!(team.read("bob", SCENE), scene("0", "3", ""));
    let log = team
        .sandbox
        .run_in("bob", "git", &["log", "-1", "--format=%s"]);
    assert!(log.stdout.starts_with("Merge"), "{log:?}");
    assert_eq!(team.sync("bob", "-").code, Some(0));
}

#[test]
fn packages_added_by_both_merge() {
    let team = Team::new();
    team.write("alice", "Packages/manifest.json", "{\n  \"dependencies\": {\n    \"com.unity.ugui\": \"2.0.0\",\n    \"com.unity.cinemachine\": \"3.1.0\"\n  }\n}\n");
    assert_eq!(team.sync("alice", "cinemachine").code, Some(0));
    team.write("bob", "Packages/manifest.json", "{\n  \"dependencies\": {\n    \"com.unity.addressables\": \"2.2.2\",\n    \"com.unity.ugui\": \"2.0.0\"\n  }\n}\n");
    let sync = team.sync("bob", "addressables");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    let manifest = team.read("bob", "Packages/manifest.json");
    assert!(
        manifest.contains("cinemachine") && manifest.contains("addressables"),
        "{manifest}"
    );
}

#[test]
fn code_conflicts_resolve_hunk_by_hunk() {
    let team = Team::new();
    team.write(
        "alice",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 2;\n}\n",
    );
    assert_eq!(team.sync("alice", "speed 2").code, Some(0));
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 3;\n}\n",
    );
    assert_eq!(team.sync("bob", "speed 3").code, Some(1));
    let bang = team.gang("bob", &["bang", "--answer", "mine"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    assert_eq!(
        team.read("bob", "Assets/Scripts/Player.cs"),
        "class Player\n{\n    int speed = 3;\n}\n"
    );
}

#[test]
fn deleted_object_with_a_new_child_is_kept_by_default() {
    let team = Team::new();
    // Alice deletes Player; Bob adds a child under it.
    let without_player = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Component:\n  - component: {fileID: 101}\n  m_Name: Environment\n--- !u!4 &101\nTransform:\n  m_GameObject: {fileID: 100}\n  m_Children: []\n  m_Father: {fileID: 0}\n";
    team.write("alice", SCENE, without_player);
    assert_eq!(team.sync("alice", "remove player").code, Some(0));
    let with_child = scene("0", "2.5", "--- !u!1 &300\nGameObject:\n  m_Component:\n  - component: {fileID: 301}\n  m_Name: Weapon\n--- !u!4 &301\nTransform:\n  m_GameObject: {fileID: 300}\n  m_Children: []\n  m_Father: {fileID: 201}\n")
        .replace("  m_Children: []\n  m_Father: {fileID: 101}", "  m_Children:\n  - {fileID: 301}\n  m_Father: {fileID: 101}");
    team.write("bob", SCENE, &with_child);
    assert_eq!(team.sync("bob", "weapon").code, Some(1));
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(
        list.stdout
            .contains("Test deleted Player. You added Weapon under it."),
        "{list:?}"
    );
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "keep"]).code,
        Some(0)
    );
    let text = team.read("bob", SCENE);
    assert!(
        text.contains("m_Name: Weapon") && text.contains("m_Name: Player"),
        "{text}"
    );
}

fn tag_manager(layers: &[&str]) -> String {
    let mut text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!78 &1\nTagManager:\n  serializedVersion: 2\n  tags: []\n  layers:\n".to_owned();
    for i in 0..32 {
        let name = layers.get(i).copied().unwrap_or("");
        if name.is_empty() {
            text.push_str("  - \n");
        } else {
            text.push_str(&format!("  - {name}\n"));
        }
    }
    text
}

/// Alice pushes layer 8 as "Pickup"; Bob names layer 8 "UI3D" (not synced).
fn clash_on_layer_8(team: &Team) {
    let base_layers = [
        "Default",
        "TransparentFX",
        "Ignore Raycast",
        "",
        "Water",
        "UI",
    ];
    // The starting TagManager.
    for user in ["alice", "bob"] {
        team.write(
            user,
            "ProjectSettings/TagManager.asset",
            &tag_manager(&base_layers),
        );
    }
    assert_eq!(team.sync("alice", "tags").code, Some(0));
    assert_eq!(team.sync("bob", "tags").code, Some(0));

    let mut alice_layers = base_layers.to_vec();
    alice_layers.resize(9, "");
    alice_layers[8] = "Pickup";
    team.write(
        "alice",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&alice_layers),
    );
    assert_eq!(team.sync("alice", "Pickup layer").code, Some(0));

    let mut bob_layers = base_layers.to_vec();
    bob_layers.resize(9, "");
    bob_layers[8] = "UI3D";
    team.write(
        "bob",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&bob_layers),
    );
}

#[test]
fn layer_collision_is_remapped_automatically() {
    let team = Team::new();
    clash_on_layer_8(&team);
    team.write(
        "bob",
        SCENE,
        &scene("0", "2.5", "").replace("  m_Name: Player\n", "  m_Name: Player\n  m_Layer: 8\n"),
    );
    team.write("bob", "Assets/Scripts/Player.cs", "class Player\n{\n    int speed = 1;\n    int mask = 1 << 8;\n    void Set() { SetLayer(8); }\n}\n");
    let sync = team.sync("bob", "UI3D layer");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stdout.contains("Moving \"UI3D\" to layer 9"),
        "{sync:?}"
    );
    assert!(sync.stdout.contains("please review"), "{sync:?}");

    let tags = team.read("bob", "ProjectSettings/TagManager.asset");
    let names = gang_core_layer_names(&tags);
    assert_eq!(names[8], "Pickup");
    assert_eq!(names[9], "UI3D");
    assert!(team.read("bob", SCENE).contains("  m_Layer: 9\n"));
    let script = team.read("bob", "Assets/Scripts/Player.cs");
    assert!(
        script.contains("1 << 9") && script.contains("SetLayer(8)"),
        "{script}"
    );
    let log = team.sandbox.run_in("bob", "git", &["log", "--format=%s"]);
    assert!(
        log.stdout.contains("gang: move layer UI3D from 8 to 9"),
        "{log:?}"
    );
}

/// Layer numbers in code: recognized forms move, everything else that may
/// be the layer is listed for review.
#[test]
fn layer_numbers_in_code_move_or_are_listed() {
    let team = Team::new();
    clash_on_layer_8(&team);
    team.write(
        "bob",
        "Assets/Scripts/Menu.cs",
        "\
using UnityEngine;
enum Layers
{
    Default = 0,
    UI3D = 8,
}
class Menu : MonoBehaviour
{
    const int Interact = 8;
    LayerMask hits = 256;
    void Start()
    {
        gameObject.layer = Interact;
        Physics.Raycast(transform.position, Vector3.forward, 100f, 0x100);
        int l = 8;
        gameObject.layer = l;
        int ui = LayerMask.NameToLayer(\"UI3D\"); // layer 8
    }
}
",
    );
    team.write(
        "bob",
        "Assets/Scripts/Menu.cs.meta",
        "fileFormatVersion: 2\nguid: c0ffee00c0ffee00c0ffee00c0ffee00\n",
    );
    let sync = team.sync("bob", "UI3D layer");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    let script = team.read("bob", "Assets/Scripts/Menu.cs");
    for expected in [
        "    UI3D = 9,",
        "const int Interact = 9;",
        "LayerMask hits = 512;",
        "100f, 0x200);",
        "NameToLayer(\"UI3D\"); // layer 8",
    ] {
        assert!(script.contains(expected), "{expected}\n{script}");
    }
    // The remap commit lists what changed.
    let log = team
        .sandbox
        .run_in("bob", "git", &["log", "--format=%B"])
        .stdout;
    for line in [
        "- Assets/Scripts/Menu.cs:5\n",
        "- Assets/Scripts/Menu.cs:9\n",
        "- Assets/Scripts/Menu.cs:10\n",
        "- Assets/Scripts/Menu.cs:14\n",
    ] {
        assert!(log.contains(line), "{line}\n{log}");
    }
    if gang_core::unity::editor_data_dirs(None).is_empty() {
        // The text rules alone cannot tell what `l` is for: listed.
        assert!(script.contains("int l = 8;"), "{script}");
        assert!(
            sync.stdout.contains(
                "Assets/Scripts/Menu.cs:15: 8 is stored in l, which is used as a layer but is not a constant; left as is — please review"
            ),
            "{sync:?}"
        );
        assert!(
            log.contains("- Assets/Scripts/Menu.cs:15: 8 is stored in l"),
            "{log}"
        );
    } else {
        // The compiler follows `l` into GameObject.layer and moves it.
        assert!(script.contains("int l = 9;"), "{script}");
        assert!(log.contains("- Assets/Scripts/Menu.cs:15\n"), "{log}");
        // The enum nothing uses yet moved by its wording, and says so.
        assert!(
            sync.stdout
                .contains("Menu.cs:5: changed because it reads like layer 8"),
            "{sync:?}"
        );
    }
}

fn gang_core_layer_names(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.starts_with("  layers:"))
        .skip(1)
        .take_while(|l| l.starts_with("  -"))
        .map(|l| l.trim_start_matches("  -").trim().to_owned())
        .collect()
}

fn object_on_layer(id: u32, name: &str, layer: u32) -> String {
    format!(
        "--- !u!1 &{id}
GameObject:
  m_Component:
  - component: {{fileID: {}}}
  m_Layer: {layer}
  m_Name: {name}
--- !u!4 &{}
Transform:
  m_GameObject: {{fileID: {id}}}
  m_Children: []
  m_Father: {{fileID: 0}}
",
        id + 1,
        id + 1
    )
}

fn base_tags(team: &Team, users: &[&str], extra: Option<&str>) -> Vec<&'static str> {
    let mut layers = vec![
        "Default",
        "TransparentFX",
        "Ignore Raycast",
        "",
        "Water",
        "UI",
        "",
        "",
        "",
    ];
    if let Some(extra) = extra {
        layers[8] = Box::leak(extra.to_owned().into_boxed_str());
    }
    for user in users {
        team.write(
            user,
            "ProjectSettings/TagManager.asset",
            &tag_manager(&layers),
        );
    }
    layers
}

/// Both renamed the same, already used layer: that is one layer with two
/// names, not two layers, so nothing may move.
#[test]
fn renaming_the_same_layer_twice_does_not_split_it() {
    let team = Team::new();
    base_tags(&team, &["alice", "bob"], Some("Enemy"));
    team.write(
        "alice",
        SCENE,
        &scene("0", "2.5", &object_on_layer(500, "Imp", 8)),
    );
    assert_eq!(team.sync("alice", "enemy layer").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));

    let mut alice = base_tags(&team, &[], Some("Enemies"));
    alice.truncate(9);
    team.write(
        "alice",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&alice),
    );
    assert_eq!(team.sync("alice", "rename").code, Some(0));

    let bob = base_tags(&team, &[], Some("Foes"));
    team.write(
        "bob",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&bob),
    );
    let scene_text = team.read("bob", SCENE) + &object_on_layer(600, "Zombie", 8);
    team.write("bob", SCENE, &scene_text);
    let sync = team.sync("bob", "rename too");
    assert!(
        !sync.stdout.contains("Moving \"Foes\""),
        "the layer was split in two: {sync:?}"
    );
    // Imp and Zombie must still share one layer; only the name is asked.
    let text = team.read("bob", SCENE);
    assert!(!text.contains("m_Layer: 9"), "{text}");
    assert_eq!(sync.code, Some(1), "{sync:?}");
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(list.stdout.contains("Layers[8]"), "{list:?}");
}

/// A layer that was already shared (here on a branch) moves: teammates who
/// started using it before the move must have their new work moved too.
#[test]
fn a_moved_layer_follows_teammates_work_in_progress() {
    // Without reservations (a teammate on an older GANG, or plain git
    // offline), the layer remap has to repair the clash after the fact.
    let team = Team::with(&["alice", "bob", "carol"]);
    team.sandbox
        .env
        .lock()
        .unwrap()
        .push(("GANG_NO_CLAIMS".into(), "1".into()));
    let carol = branch_layer_meets_main(&team);
    let text = team.read("carol", SCENE);
    let panel = text.split("m_Name: MenuPanel").next().unwrap();
    assert!(
        panel.trim_end().ends_with("m_Layer: 9"),
        "MenuPanel silently moved to Pickup: {text} {carol:?}"
    );
}

/// With reservations the clash never happens: UI3D was reserved on the
/// branch first, so Pickup takes the next number when Alice saves it, and
/// nothing on the branch has to move.
#[test]
fn a_layer_reserved_on_a_branch_keeps_its_number() {
    let team = Team::with(&["alice", "bob", "carol"]);
    branch_layer_meets_main(&team);
    let names = gang_core_layer_names(&team.read("carol", "ProjectSettings/TagManager.asset"));
    assert_eq!(names[8], "UI3D");
    assert_eq!(names[9], "Pickup");
    let text = team.read("carol", SCENE);
    let panel = text.split("m_Name: MenuPanel").next().unwrap();
    assert!(panel.trim_end().ends_with("m_Layer: 8"), "{text}");
    let log = team.sandbox.run_in("alice", "git", &["log", "--format=%s"]);
    assert!(!log.stdout.contains("gang: move layer"), "{log:?}");
}

/// Bob shares UI3D = 8 on a branch, Carol builds on it, Pickup = 8 lands
/// on main, Bob merges main into the branch, Carol syncs.
fn branch_layer_meets_main(team: &Team) -> common::Outcome {
    base_tags(team, &["alice", "bob", "carol"], None);
    assert_eq!(team.sync("alice", "tags").code, Some(0));
    for user in ["bob", "carol"] {
        assert_eq!(team.sync(user, "-").code, Some(0));
    }
    team.gang("bob", &["switch", "-c", "menu"]);
    base_tags(team, &["bob"], Some("UI3D"));
    assert_eq!(team.gang("bob", &["save", "-m", "UI3D"]).code, Some(0));
    assert_eq!(
        team.gang("bob", &["push", "-u", "origin", "menu"]).code,
        Some(0)
    );
    team.gang("carol", &["fetch"]);
    assert_eq!(team.gang("carol", &["switch", "menu"]).code, Some(0));
    let carol_scene = team.read("carol", SCENE) + &object_on_layer(700, "MenuPanel", 8);
    team.write("carol", SCENE, &carol_scene);
    assert_eq!(
        team.gang("carol", &["save", "-m", "menu panel"]).code,
        Some(0)
    );

    base_tags(team, &["alice"], Some("Pickup"));
    assert_eq!(team.sync("alice", "Pickup").code, Some(0));
    team.gang("bob", &["fetch"]);
    let merge = team.gang("bob", &["merge", "origin/main", "-m", "main"]);
    assert_eq!(merge.code, Some(0), "{merge:?}");
    assert_eq!(team.gang("bob", &["push"]).code, Some(0));

    let sync = team.sync("carol", "-");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    sync
}

/// Field test A2: `gang save` during a merge with open questions must not
/// mark them answered.
#[test]
fn save_refuses_while_conflicts_are_open() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("0", "3", ""));
    assert_eq!(team.sync("alice", "alice jump").code, Some(0));
    team.write("bob", SCENE, &scene("0", "4", ""));
    assert_eq!(team.sync("bob", "bob jump").code, Some(1));
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 2;\n}\n",
    );
    let save = team.gang("bob", &["save", "-m", "more"]);
    assert_eq!(save.code, Some(1), "{save:?}");
    assert!(save.stderr.contains("gang bang"), "{save:?}");
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(list.stdout.contains("Jump Height"), "{list:?}");
}

#[test]
fn several_messages_become_paragraphs() {
    let team = Team::new();
    team.write(
        "alice",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 2;\n}\n",
    );
    let save = team.gang(
        "alice",
        &["save", "-m", "Faster player", "-m", "Speed 1 felt slow."],
    );
    assert_eq!(save.code, Some(0), "{save:?}");
    let log = team
        .sandbox
        .run_in("alice", "git", &["log", "-1", "--format=%B"]);
    assert_eq!(log.stdout.trim_end(), "Faster player\n\nSpeed 1 felt slow.");
}

/// Field test A5: during Bob's unfinished merge, Alice's own files must not
/// show up as Bob's work.
#[test]
fn an_unfinished_merge_is_not_published_as_own_work() {
    let team = Team::new();
    let new_script = "Assets/Scripts/Door.cs";
    team.write("alice", new_script, "class Door {}\n");
    team.write(
        "alice",
        &format!("{new_script}.meta"),
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\n",
    );
    team.write("alice", SCENE, &scene("0", "3", ""));
    assert_eq!(team.sync("alice", "door").code, Some(0));
    team.write("bob", SCENE, &scene("0", "4", ""));
    assert_eq!(team.sync("bob", "bob jump").code, Some(1));

    assert_eq!(
        team.gang("bob", &["presence-publish", "--force"]).code,
        Some(0)
    );
    let who = team.gang("alice", &["--json", "who"]);
    let bob = who
        .stdout
        .split("\"user\":")
        .find(|p| p.contains("\"bob\""))
        .unwrap_or("");
    assert!(
        bob.contains(SCENE),
        "bob did change the scene: {}",
        who.stdout
    );
    assert!(
        !bob.contains(new_script),
        "Alice's file shown as Bob's: {}",
        who.stdout
    );
}

/// Field test A9: conflicts answered, merge commit never made. The next
/// sync finishes that merge instead of asking for a new message.
#[test]
fn sync_finishes_a_merge_left_open() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("0", "3", ""));
    assert_eq!(team.sync("alice", "alice jump").code, Some(0));
    team.write("bob", SCENE, &scene("0", "4", ""));
    assert_eq!(team.sync("bob", "bob jump").code, Some(1));
    // Answered by hand and staged, but not committed.
    team.write("bob", SCENE, &scene("0", "3", ""));
    team.sandbox.run_in("bob", "git", &["add", SCENE]);
    let list = team.gang("bob", &["bang", "--list"]).stdout;
    assert!(
        list.contains("was resolved by hand outside GANG")
            && list.ends_with("No conflicts to resolve.\n"),
        "{list}"
    );

    let sync = team.gang("bob", &["sync"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(sync.stdout.contains("Finished the merge"), "{sync:?}");
    let log = team
        .sandbox
        .run_in("bob", "git", &["log", "-1", "--format=%s"]);
    assert!(log.stdout.starts_with("Merge"), "{log:?}");
}

/// Field test C2: a new, untracked file does not stop a sync.
#[test]
fn an_untracked_file_does_not_stop_sync() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("1", "2.5", ""));
    assert_eq!(team.sync("alice", "move").code, Some(0));
    team.write("bob", "Assets/Scripts/Draft.cs", "class Draft {}\n");
    let sync = team.gang("bob", &["sync"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert_eq!(team.read("bob", SCENE), scene("1", "2.5", ""));
    assert!(
        team.read("bob", "Assets/Scripts/Draft.cs")
            .contains("Draft")
    );
}

/// Field test A6: an empty folder's .meta only warns its author, so it must
/// not block the teammate's merge commit either.
#[test]
fn an_empty_folder_meta_does_not_block_the_merge() {
    let team = Team::new();
    std::fs::create_dir_all(team.sandbox.dir.path().join("alice/Assets/Prefabs")).unwrap();
    team.write(
        "alice",
        "Assets/Prefabs.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\nfolderAsset: yes\n",
    );
    team.write("alice", SCENE, &scene("0", "3", ""));
    let save = team.sync("alice", "prefabs folder");
    assert_eq!(save.code, Some(0), "{save:?}");
    // Bob's merge needs an answer, so its commit runs the pre-commit checks.
    team.write("bob", SCENE, &scene("0", "4", ""));
    assert_eq!(team.sync("bob", "jump").code, Some(1));
    let bang = team.gang("bob", &["bang", "--answer", "theirs"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    assert!(!bang.stderr.contains("has no asset"), "{bang:?}");
}

/// Field test C1: both created the same folder. Its two .meta files differ
/// only in the GUID; the shared one is kept without a question.
#[test]
fn both_created_the_same_folder() {
    let team = Team::new();
    for (user, guid, file) in [("alice", "aa", "A"), ("bob", "bb", "B")] {
        team.write(
            user,
            "Assets/Audio.meta",
            &format!("fileFormatVersion: 2\nguid: {guid:0>32}\nfolderAsset: yes\n"),
        );
        std::fs::create_dir_all(team.sandbox.dir.path().join(user).join("Assets/Audio")).unwrap();
        team.write(user, &format!("Assets/Audio/{file}.txt"), "x\n");
        team.write(
            user,
            &format!("Assets/Audio/{file}.txt.meta"),
            &format!("fileFormatVersion: 2\nguid: {:0>32}\n", format!("{guid}1")),
        );
    }
    assert_eq!(team.sync("alice", "audio").code, Some(0));
    let sync = team.sync("bob", "audio too");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        team.read("bob", "Assets/Audio.meta")
            .contains(&format!("{:0>32}", "aa"))
    );
}

/// §1.2: during a rebase git's "ours" is the upstream commit. GANG's "mine"
/// must still be the user's own value.
#[test]
fn same_property_when_rebasing_mine_is_still_mine() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("0", "3", ""));
    assert_eq!(team.sync("alice", "alice jump").code, Some(0));
    team.write("bob", SCENE, &scene("0", "4", ""));
    let sync = team.gang("bob", &["sync", "--rebase", "-m", "bob jump"]);
    assert_eq!(sync.code, Some(1), "{sync:?}");
    // Bob's own value meanwhile, as in a merge.
    assert_eq!(team.read("bob", SCENE), scene("0", "4", ""));
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(list.stdout.contains("Jump Height"), "{list:?}");

    let bang = team.gang("bob", &["bang", "--answer", "mine"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    assert_eq!(team.read("bob", SCENE), scene("0", "4", ""));
    let log = team
        .sandbox
        .run_in("bob", "git", &["log", "--format=%s", "-3"]);
    assert_eq!(log.stdout, "bob jump\nalice jump\nstart\n", "{log:?}");
    assert_eq!(team.sync("bob", "-").code, Some(0));
}

#[test]
fn same_property_when_rebasing_theirs_is_the_teammates() {
    let team = Team::new();
    team.gang("bob", &["config", "--project", "sync.strategy", "rebase"]);
    team.write("alice", SCENE, &scene("0", "3", ""));
    assert_eq!(team.sync("alice", "alice jump").code, Some(0));
    team.write("bob", SCENE, &scene("0", "4", ""));
    assert_eq!(team.sync("bob", "bob jump").code, Some(1));
    let bang = team.gang("bob", &["bang", "--answer", "theirs"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    assert_eq!(team.read("bob", SCENE), scene("0", "3", ""));
}

#[test]
fn code_conflicts_when_rebasing_keep_mine() {
    let team = Team::new();
    team.write(
        "alice",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 2;\n}\n",
    );
    assert_eq!(team.sync("alice", "speed 2").code, Some(0));
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 3;\n}\n",
    );
    assert_eq!(
        team.gang("bob", &["sync", "--rebase", "-m", "speed 3"])
            .code,
        Some(1)
    );
    let bang = team.gang("bob", &["bang", "--answer", "mine"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    assert_eq!(
        team.read("bob", "Assets/Scripts/Player.cs"),
        "class Player\n{\n    int speed = 3;\n}\n"
    );
}

/// A plain `gang rebase`: the deleting teammate is named after the upstream
/// commit's author, and the user is "you".
#[test]
fn deleted_object_when_rebasing_names_the_teammate() {
    let team = Team::new();
    let without_player = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Component:\n  - component: {fileID: 101}\n  m_Name: Environment\n--- !u!4 &101\nTransform:\n  m_GameObject: {fileID: 100}\n  m_Children: []\n  m_Father: {fileID: 0}\n";
    team.write("alice", SCENE, without_player);
    team.sandbox.run_in("alice", "git", &["add", SCENE]);
    let commit = team.sandbox.run_in(
        "alice",
        "git",
        &[
            "commit",
            "-q",
            "--author=alice <alice@example.com>",
            "-m",
            "remove player",
        ],
    );
    assert_eq!(commit.code, Some(0), "{commit:?}");
    assert_eq!(team.gang("alice", &["push", "-q"]).code, Some(0));

    let with_child = scene("0", "2.5", "--- !u!1 &300\nGameObject:\n  m_Component:\n  - component: {fileID: 301}\n  m_Name: Weapon\n--- !u!4 &301\nTransform:\n  m_GameObject: {fileID: 300}\n  m_Children: []\n  m_Father: {fileID: 201}\n")
        .replace("  m_Children: []\n  m_Father: {fileID: 101}", "  m_Children:\n  - {fileID: 301}\n  m_Father: {fileID: 101}");
    team.write("bob", SCENE, &with_child);
    assert_eq!(team.gang("bob", &["save", "-m", "weapon"]).code, Some(0));
    team.gang("bob", &["fetch", "-q"]);
    let rebase = team.gang("bob", &["rebase", "origin/main"]);
    assert_ne!(rebase.code, Some(0), "{rebase:?}");
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(
        list.stdout
            .contains("alice deleted Player. You added Weapon under it."),
        "{list:?}"
    );
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "keep"]).code,
        Some(0)
    );
    let text = team.read("bob", SCENE);
    assert!(
        text.contains("m_Name: Weapon") && text.contains("m_Name: Player"),
        "{text}"
    );
}

/// L3: Alice and Bob both add layer 8; Bob brings Alice's work in with
/// `command` instead of `gang sync`.
fn layer_clash_through(command: &[&str]) -> (Team, common::Outcome) {
    let team = Team::new();
    base_tags(&team, &["alice", "bob"], None);
    assert_eq!(team.sync("alice", "tags").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));
    base_tags(&team, &["alice"], Some("Pickup"));
    assert_eq!(team.sync("alice", "Pickup layer").code, Some(0));

    base_tags(&team, &["bob"], Some("UI3D"));
    let scene_text = team.read("bob", SCENE) + &object_on_layer(600, "Panel", 8);
    team.write("bob", SCENE, &scene_text);
    assert_eq!(
        team.gang("bob", &["save", "-m", "UI3D layer"]).code,
        Some(0)
    );
    team.gang("bob", &["fetch", "-q"]);
    let outcome = team.gang("bob", command);
    (team, outcome)
}

fn assert_layer_moved(team: &Team, outcome: &common::Outcome) {
    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    assert!(
        outcome.stdout.contains("Moving \"UI3D\" to layer 9"),
        "{outcome:?}"
    );
    let names = gang_core_layer_names(&team.read("bob", "ProjectSettings/TagManager.asset"));
    assert_eq!((names[8].as_str(), names[9].as_str()), ("Pickup", "UI3D"));
    let text = team.read("bob", SCENE);
    let panel = text.split("m_Name: Panel").next().unwrap();
    assert!(panel.trim_end().ends_with("m_Layer: 9"), "{text}");
    let status = team
        .sandbox
        .run_in("bob", "git", &["status", "--porcelain"]);
    assert_eq!(status.stdout, "", "{status:?}");
}

#[test]
fn layer_clash_is_remapped_by_pull() {
    let (team, pull) = layer_clash_through(&["pull", "--no-rebase", "--no-edit"]);
    assert_layer_moved(&team, &pull);
}

#[test]
fn layer_clash_is_remapped_by_pull_rebase() {
    let (team, pull) = layer_clash_through(&["pull", "--rebase"]);
    assert_layer_moved(&team, &pull);
    let log = team
        .sandbox
        .run_in("bob", "git", &["log", "--format=%s", "-2"]);
    assert_eq!(
        log.stdout, "gang: move layer UI3D from 8 to 9\nUI3D layer\n",
        "{log:?}"
    );
    let rev = |rev: &str| {
        team.sandbox
            .run_in("bob", "git", &["rev-parse", rev])
            .stdout
    };
    assert_eq!(rev("HEAD~2"), rev("origin/main"), "not rebased");
}

#[test]
fn layer_clash_is_remapped_by_rebase() {
    let (team, rebase) = layer_clash_through(&["rebase", "origin/main"]);
    assert_layer_moved(&team, &rebase);
}

#[test]
fn layer_clash_is_remapped_by_sync_rebase() {
    let (team, sync) = layer_clash_through(&["sync", "--rebase"]);
    assert!(sync.stdout.contains("Pushed"), "{sync:?}");
    assert_layer_moved(&team, &sync);
}

#[test]
fn layer_clash_is_remapped_by_cherry_pick() {
    // origin/main is Alice's merge of Bob's work; its first parent is Pickup.
    let (team, pick) = layer_clash_through(&["cherry-pick", "origin/main^"]);
    assert_layer_moved(&team, &pick);
}

/// L3 with plain git: the pass cannot run, so the question explains what
/// either name would do and points to `gang sync`.
#[test]
fn layer_clash_through_plain_git_explains_itself() {
    let (team, _) = layer_clash_through(&["status"]);
    let pull = team
        .sandbox
        .run_in("bob", "git", &["pull", "--no-rebase", "--no-edit"]);
    assert_ne!(pull.code, Some(0), "{pull:?}");
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(list.stdout.contains("Layers[8]"), "{list:?}");
    assert!(
        list.stdout.contains("silently change layer") && list.stdout.contains("`gang sync`"),
        "{list:?}"
    );
    assert!(list.stdout.contains("gang merge --abort"), "{list:?}");
}

/// DESIGN G2 / HARDENING §1.3: a Unity file GANG cannot read (here: not
/// UTF-8) never gets conflict markers. A clean line merge is kept; an
/// overlapping one leaves the file as it was and asks for the whole file.
#[test]
fn unreadable_unity_files_never_get_conflict_markers() {
    let team = Team::new();
    let path = "Assets/Scenes/Broken.unity";
    let version = |a: &str, b: &str| -> Vec<u8> {
        let mut bytes = b"%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Name: Caf\xe9\n".to_vec();
        bytes.extend_from_slice(
            format!("  m_Layer: {a}\n  m_TagString: Untagged\n  m_StaticEditorFlags: {b}\n")
                .as_bytes(),
        );
        bytes
    };
    let write = |user: &str, bytes: &[u8]| {
        std::fs::write(team.sandbox.dir.path().join(user).join(path), bytes).unwrap();
    };
    for user in ["alice", "bob"] {
        write(user, &version("0", "0"));
        team.write(
            user,
            &format!("{path}.meta"),
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000beef\n",
        );
    }
    assert_eq!(team.sync("alice", "broken scene").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));

    // The same line changed on both sides: no markers, a whole-file question.
    write("alice", &version("3", "0"));
    assert_eq!(team.sync("alice", "alice layer").code, Some(0));
    write("bob", &version("4", "0"));
    let sync = team.sync("bob", "bob layer");
    assert_eq!(sync.code, Some(1), "{sync:?}");
    let bytes = std::fs::read(team.sandbox.dir.path().join("bob").join(path)).unwrap();
    assert!(
        !bytes.windows(7).any(|w| w == b"<<<<<<<"),
        "markers written"
    );
    assert_eq!(bytes, version("4", "0"));
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(
        list.stdout
            .contains("Broken.unity  (text or binary conflict)"),
        "{list:?}"
    );
    let bang = team.gang("bob", &["bang", "--answer", "theirs"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    let bytes = std::fs::read(team.sandbox.dir.path().join("bob").join(path)).unwrap();
    assert_eq!(bytes, version("3", "0"));
    assert_eq!(team.sync("bob", "-").code, Some(0));

    // Different lines: the clean line merge is kept.
    write("alice", &version("3", "1"));
    assert_eq!(team.sync("alice", "static").code, Some(0));
    write("bob", &version("5", "0"));
    assert_eq!(team.sync("bob", "layer 5").code, Some(0));
    let bytes = std::fs::read(team.sandbox.dir.path().join("bob").join(path)).unwrap();
    assert_eq!(bytes, version("5", "1"));
}

/// HARDENING §1.4: both sides add the same asset, each with its own GUID.
/// The merge keeps the GUID already shared without a question, and the
/// references Bob made to his own GUID follow it.
#[test]
fn an_asset_added_twice_keeps_one_guid_and_every_reference() {
    for same_content in [true, false] {
        let team = Team::new();
        let logo = "Assets/Art/Logo.png";
        let meta = |guid: &str| {
            format!(
                "fileFormatVersion: 2\nguid: {guid}\nTextureImporter:\n  maxTextureSize: 2048\n"
            )
        };
        let alice_guid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";
        let bob_guid = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";
        for (user, guid, bytes) in [
            ("alice", alice_guid, b"\x89PNG alice".as_slice()),
            (
                "bob",
                bob_guid,
                if same_content {
                    b"\x89PNG alice".as_slice()
                } else {
                    b"\x89PNG bob".as_slice()
                },
            ),
        ] {
            let dir = team.sandbox.dir.path().join(user);
            std::fs::create_dir_all(dir.join("Assets/Art")).unwrap();
            std::fs::write(dir.join(logo), bytes).unwrap();
            team.write(user, &format!("{logo}.meta"), &meta(guid));
            team.write(
                user,
                "Assets/Art.meta",
                &format!(
                    "fileFormatVersion: 2\nguid: {}\nfolderAsset: yes\n",
                    if user == "alice" {
                        "f".repeat(32)
                    } else {
                        "e".repeat(32)
                    }
                ),
            );
        }
        let with_logo = scene("0", "2.5", "").replace(
            "  jumpHeight: 2.5\n",
            &format!("  jumpHeight: 2.5\n  icon: {{fileID: 2800000, guid: {bob_guid}, type: 3}}\n"),
        );
        team.write("bob", SCENE, &with_logo);
        assert_eq!(team.sync("alice", "logo").code, Some(0));

        let sync = team.sync("bob", "logo too");
        if same_content {
            assert_eq!(sync.code, Some(0), "{sync:?}");
        } else {
            // The pictures differ: that is a real question, the GUID is not.
            assert_eq!(sync.code, Some(1), "{sync:?}");
            let list = team.gang("bob", &["bang", "--list"]);
            assert!(
                list.stdout.contains("Logo.png  (text or binary conflict)"),
                "{list:?}"
            );
            assert!(!list.stdout.contains("Logo.png.meta"), "{list:?}");
            let bang = team.gang("bob", &["bang", "--answer", "mine"]);
            assert_eq!(bang.code, Some(0), "{bang:?}");
        }
        assert_eq!(team.read("bob", &format!("{logo}.meta")), meta(alice_guid));
        let scene_text = team.read("bob", SCENE);
        assert!(
            scene_text.contains(&format!("guid: {alice_guid}")),
            "{scene_text}"
        );
        assert!(!scene_text.contains(bob_guid), "{scene_text}");
        let log = team
            .sandbox
            .run_in("bob", "git", &["log", "-1", "--format=%s"]);
        assert_eq!(
            log.stdout.trim(),
            "gang: point references at the shared GUIDs",
            "{log:?}"
        );
    }
}

/// HARDENING L5: the collision matrix moves with the layer, and the two
/// people's matrix settings merge cell by cell without a question.
#[test]
fn the_collision_matrix_moves_with_the_layer() {
    let team = Team::new();
    let dynamics = "ProjectSettings/DynamicsManager.asset";
    let matrix = |off: &[(usize, usize)]| -> String {
        let mut rows = [u32::MAX; 32];
        for (a, b) in off {
            rows[*a] &= !(1 << b);
            rows[*b] &= !(1 << a);
        }
        let hex: String = rows
            .iter()
            .flat_map(|r| r.to_le_bytes())
            .map(|b| format!("{b:02x}"))
            .collect();
        format!(
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!55 &1\nPhysicsManager:\n  m_ObjectHideFlags: 0\n  serializedVersion: 13\n  m_Gravity: {{x: 0, y: -9.81, z: 0}}\n  m_LayerCollisionMatrix: {hex}\n"
        )
    };
    for user in ["alice", "bob"] {
        team.write(user, dynamics, &matrix(&[]));
    }
    // Alice: Pickup = 8, pickups ignore each other. Bob: UI3D = 8, UI3D
    // ignores Water (4).
    clash_on_layer_8(&team);
    team.write("alice", dynamics, &matrix(&[(8, 8)]));
    assert_eq!(team.sync("alice", "pickups ignore pickups").code, Some(0));
    team.write("bob", dynamics, &matrix(&[(8, 4)]));
    let sync = team.sync("bob", "UI3D ignores water");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stdout
            .contains("ProjectSettings/DynamicsManager.asset: collision matrix (layer 8 → 9)"),
        "{sync:?}"
    );
    // Pickup (8) still ignores itself; UI3D (now 9) ignores Water.
    assert_eq!(team.read("bob", dynamics), matrix(&[(8, 8), (9, 4)]));
    let sync = team.sync("alice", "-");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stdout
            .contains("PhysicsManager › Collision  Water × UI3D: ignore each other"),
        "{sync:?}"
    );
    assert_eq!(team.read("alice", dynamics), matrix(&[(8, 8), (9, 4)]));
}

/// HARDENING L7: when no layer is free, the remap cannot move ours. The sync
/// stops before merging and says what to do, instead of letting a "which
/// name?" question silently move the other side's objects.
#[test]
fn a_clash_with_no_free_layer_stops_and_says_why() {
    let team = Team::new();
    team.sandbox
        .env
        .lock()
        .unwrap()
        .push(("GANG_NO_CLAIMS".into(), "1".into()));
    let mut full: Vec<String> = vec![
        "Default".into(),
        "TransparentFX".into(),
        "Ignore Raycast".into(),
        String::new(),
        "Water".into(),
        "UI".into(),
        String::new(),
        String::new(),
        String::new(),
    ];
    full.extend((9..32).map(|i| format!("Used{i}")));
    let names = |extra: &'static str| -> Vec<&str> {
        let mut list: Vec<&str> = full.iter().map(String::as_str).collect();
        list[8] = extra;
        list
    };
    for user in ["alice", "bob"] {
        team.write(
            user,
            "ProjectSettings/TagManager.asset",
            &tag_manager(&names("")),
        );
    }
    assert_eq!(team.sync("alice", "tags").code, Some(0));
    assert_eq!(team.sync("bob", "tags").code, Some(0));
    team.write(
        "alice",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&names("Pickup")),
    );
    assert_eq!(team.sync("alice", "Pickup").code, Some(0));
    team.write(
        "bob",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&names("UI3D")),
    );
    let sync = team.sync("bob", "UI3D");
    assert_ne!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr.contains("no layer is free to move \"UI3D\" to"),
        "{sync:?}"
    );
    assert!(sync.stderr.contains("Free a layer first"), "{sync:?}");
    // Nothing was merged.
    let merge_head = team.sandbox.dir.path().join("bob/.git/MERGE_HEAD");
    assert!(!merge_head.exists());
}

/// Thought experiment TE42: Alice removes layer 8 while Bob puts objects
/// on it. The removal merges silently and Bob's objects end up on a layer
/// with no name, so the sync says so.
#[test]
fn a_removed_layer_still_in_use_is_reported() {
    let team = Team::new();
    let mut layers = vec![
        "Default",
        "TransparentFX",
        "Ignore Raycast",
        "",
        "Water",
        "UI",
        "",
        "",
        "Pickup",
    ];
    for user in ["alice", "bob"] {
        team.write(
            user,
            "ProjectSettings/TagManager.asset",
            &tag_manager(&layers),
        );
    }
    assert_eq!(team.sync("alice", "tags").code, Some(0));
    assert_eq!(team.sync("bob", "tags").code, Some(0));
    layers[8] = "";
    team.write(
        "alice",
        "ProjectSettings/TagManager.asset",
        &tag_manager(&layers),
    );
    assert_eq!(team.sync("alice", "no more pickups").code, Some(0));
    team.write(
        "bob",
        SCENE,
        &scene("0", "2.5", "").replace("  m_Name: Player\n", "  m_Name: Player\n  m_Layer: 8\n"),
    );
    let sync = team.sync("bob", "player on pickup");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr.contains(
            "The incoming changes removed layer 8 \"Pickup\", but your changes put 1 object on it"
        ),
        "{sync:?}"
    );
    let doctor = team.gang("bob", &["doctor"]);
    assert!(
        doctor
            .stdout
            .contains("objects are on layer 8, which has no name"),
        "{doctor:?}"
    );
}

/// Thought experiment TE7: Alice renames a field with [FormerlySerializedAs]
/// and her scene now says `jump:`; Bob changes the old `jumpHeight:`. Bob's
/// value lands in the renamed field, without a question.
#[test]
fn a_renamed_field_keeps_the_other_side_s_change() {
    let team = Team::new();
    let guid = "0000000000000000000000000000aaaa";
    let level = |key: &str, value: &str| {
        format!(
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &200\nGameObject:\n  m_Component:\n  - component: {{fileID: 202}}\n  m_Name: Player\n--- !u!114 &202\nMonoBehaviour:\n  m_GameObject: {{fileID: 200}}\n  m_Script: {{fileID: 11500000, guid: {guid}, type: 3}}\n  speed: 1\n  {key}: {value}\n"
        )
    };
    let path = "Assets/Scenes/Renamed.unity";
    for user in ["alice", "bob"] {
        team.write(user, path, &level("jumpHeight", "2.5"));
        team.write(
            user,
            &format!("{path}.meta"),
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000abcd\n",
        );
    }
    assert_eq!(team.sync("alice", "renamed scene").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));

    team.write(
        "alice",
        "Assets/Scripts/Player.cs",
        "using UnityEngine.Serialization;\nclass Player\n{\n    int speed = 1;\n    [FormerlySerializedAs(\"jumpHeight\")] public float jump;\n}\n",
    );
    team.write("alice", path, &level("jump", "2.5"));
    assert_eq!(team.sync("alice", "rename jump").code, Some(0));
    team.write("bob", path, &level("jumpHeight", "4"));
    let sync = team.sync("bob", "higher jump");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert_eq!(team.read("bob", path), level("jump", "4"));
}

/// Thought experiment R3: Unity keeps the project settings in memory, so a
/// layer moved under an open editor would be written back the old way. The
/// sync stops and says why; --ignore-editor merges anyway.
#[test]
fn a_layer_move_waits_for_unity_to_close() {
    let team = Team::new();
    clash_on_layer_8(&team);
    let temp = team.sandbox.dir.path().join("bob").join("Temp");
    std::fs::create_dir_all(&temp).unwrap();
    let file = std::fs::File::create(temp.join("UnityLockfile")).unwrap();
    let mut lock = fd_lock::RwLock::new(file);
    let held = lock.write().unwrap();
    let sync = team.sync("bob", "UI3D layer");
    assert_ne!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr
            .contains("Unity is open, and one of your layers has to move"),
        "{sync:?}"
    );
    let merge_head = team.sandbox.dir.path().join("bob/.git/MERGE_HEAD");
    assert!(!merge_head.exists());
    drop(held);
    let sync = team.gang("bob", &["sync"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stdout.contains("Moving \"UI3D\" to layer 9"),
        "{sync:?}"
    );
}

/// Thought experiment R27: `gang checkout --theirs` during a merge would
/// settle every conflict in a file at once; GANG points to gang bang.
#[test]
fn whole_file_sides_during_a_merge_go_through_bang() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("1", "3", ""));
    assert_eq!(team.sync("alice", "alice tuning").code, Some(0));
    team.write("bob", SCENE, &scene("2", "4", ""));
    assert_eq!(team.sync("bob", "bob tuning").code, Some(1));
    for args in [
        vec!["checkout", "--theirs", "--", SCENE],
        vec!["restore", "--ours", "--", SCENE],
    ] {
        let refused = team.gang("bob", &args);
        assert_eq!(refused.code, Some(1), "{refused:?}");
        assert!(
            refused
                .stderr
                .contains("GANG asks every conflict on its own: 2 questions are open"),
            "{refused:?}"
        );
    }
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "theirs"]).code,
        Some(1)
    );
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "theirs"]).code,
        Some(0)
    );
}

/// Thought experiment R28: `gang merge -X theirs` would let git settle every
/// text conflict for one side before GANG could ask about any of them.
#[test]
fn one_side_for_every_conflict_is_refused() {
    let team = Team::new();
    let script = "Assets/Scripts/Player.cs";
    team.write("alice", script, "class Player\n{\n    int speed = 2;\n}\n");
    assert_eq!(team.sync("alice", "faster").code, Some(0));
    team.write("bob", script, "class Player\n{\n    int speed = 3;\n}\n");
    assert_eq!(
        team.gang("bob", &["git", "commit", "-q", "-am", "even faster"])
            .code,
        Some(0)
    );
    assert_eq!(team.gang("bob", &["fetch", "-q"]).code, Some(0));
    for args in [
        vec!["merge", "-X", "theirs", "origin/main"],
        vec!["pull", "-Xours"],
        vec!["rebase", "--strategy-option=theirs", "origin/main"],
    ] {
        let refused = team.gang("bob", &args);
        assert_eq!(refused.code, Some(1), "{refused:?}");
        assert!(
            refused
                .stderr
                .contains("would settle them all for one side"),
            "{refused:?}"
        );
    }
    assert!(team.read("bob", script).contains("speed = 3"));
    assert!(!team.sandbox.dir.path().join("bob/.git/MERGE_HEAD").exists());
    // Other strategy options still go through.
    let merge = team.gang(
        "bob",
        &["merge", "-X", "patience", "--no-edit", "origin/main"],
    );
    assert_eq!(merge.code, Some(1), "{merge:?}");
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "mine"]).code,
        Some(0)
    );
    assert!(team.read("bob", script).contains("speed = 3"));
}

const RED: &str = "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!21 &2100000
Material:
  m_Name: Red
";

impl Team {
    fn add_red(&self, user: &str) {
        let root = self.sandbox.dir.path().join(user);
        std::fs::create_dir_all(root.join("Assets/Materials")).unwrap();
        self.write(
            user,
            "Assets/Materials.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\nfolderAsset: yes\n",
        );
        self.write(user, "Assets/Materials/Red.mat", RED);
        self.write(
            user,
            "Assets/Materials/Red.mat.meta",
            "fileFormatVersion: 2\nguid: c0ffee00c0ffee00c0ffee00c0ffee00\n",
        );
    }

    fn remove(&self, user: &str, path: &str) {
        std::fs::remove_file(self.sandbox.dir.path().join(user).join(path)).unwrap();
    }
}

const RED_RENDERER: &str = "--- !u!23 &203
MeshRenderer:
  m_GameObject: {fileID: 200}
  m_Materials:
  - {fileID: 2100000, guid: c0ffee00c0ffee00c0ffee00c0ffee00, type: 2}
";

/// Thought experiment R29: alice deletes a material while bob puts it on a
/// new object. git merges the two without a conflict (different files);
/// Unity would show bob's renderer with a Missing material.
#[test]
fn an_asset_deleted_on_one_side_and_used_on_the_other_is_reported() {
    let team = Team::new();
    team.add_red("alice");
    assert_eq!(team.sync("alice", "red material").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));

    team.remove("alice", "Assets/Materials/Red.mat");
    team.remove("alice", "Assets/Materials/Red.mat.meta");
    assert_eq!(team.sync("alice", "no more red").code, Some(0));
    team.write("bob", SCENE, &scene("0", "2.5", RED_RENDERER));
    let sync = team.sync("bob", "red player");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr.contains(" deleted Assets/Materials/Red.mat (")
            && sync.stderr.contains("but your work uses it:"),
        "{sync:?}"
    );
    assert!(sync.stdout.contains(&format!("    {SCENE}")), "{sync:?}");
    assert!(
        sync.stderr.contains("To bring it back: gang git checkout "),
        "{sync:?}"
    );
    // Without a terminal nothing is brought back on its own.
    assert!(
        !team
            .sandbox
            .dir
            .path()
            .join("bob/Assets/Materials/Red.mat")
            .exists()
    );
    // gang doctor names where the missing asset went.
    std::fs::create_dir_all(team.sandbox.dir.path().join("bob/Library/PackageCache")).unwrap();
    let doctor = team.gang("bob", &["doctor"]);
    assert!(
        doctor
            .stdout
            .contains("Assets/Materials/Red.mat, deleted in "),
        "{doctor:?}"
    );
    assert!(
        doctor.stdout.contains("bring it back: gang git checkout "),
        "{doctor:?}"
    );

    // The other way round: bob deleted it, alice's new work uses it.
    team.add_red("bob");
    assert_eq!(team.sync("bob", "red again").code, Some(0));
    assert_eq!(team.sync("alice", "-").code, Some(0));
    team.write("alice", SCENE, &scene("7", "2.5", RED_RENDERER));
    assert_eq!(team.sync("alice", "red player moved").code, Some(0));
    team.remove("bob", "Assets/Materials/Red.mat");
    team.remove("bob", "Assets/Materials/Red.mat.meta");
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n    int speed = 9;\n}\n",
    );
    let sync = team.sync("bob", "red is gone");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr
            .contains("You deleted Assets/Materials/Red.mat ("),
        "{sync:?}"
    );
    assert!(
        sync.stderr.contains("but the incoming work uses it:"),
        "{sync:?}"
    );
}

/// R29 at the source: a commit that deletes an asset a scene still uses
/// says so (a warning: the scene may be fixed in the next commit). Moving
/// the asset keeps its GUID and says nothing.
#[test]
fn deleting_an_asset_a_scene_still_uses_warns() {
    let team = Team::new();
    team.add_red("alice");
    team.write("alice", SCENE, &scene("0", "2.5", RED_RENDERER));
    assert_eq!(team.sync("alice", "red player").code, Some(0));

    let root = team.sandbox.dir.path().join("alice");
    std::fs::create_dir_all(root.join("Assets/Art")).unwrap();
    team.write(
        "alice",
        "Assets/Art.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000ffff\nfolderAsset: yes\n",
    );
    std::fs::rename(
        root.join("Assets/Materials/Red.mat"),
        root.join("Assets/Art/Red.mat"),
    )
    .unwrap();
    std::fs::rename(
        root.join("Assets/Materials/Red.mat.meta"),
        root.join("Assets/Art/Red.mat.meta"),
    )
    .unwrap();
    let moved = team.gang("alice", &["save", "-m", "move red"]);
    assert_eq!(moved.code, Some(0), "{moved:?}");
    assert!(!moved.stderr.contains("Missing"), "{moved:?}");

    team.remove("alice", "Assets/Art/Red.mat");
    team.remove("alice", "Assets/Art/Red.mat.meta");
    let deleted = team.gang("alice", &["save", "-m", "no more red"]);
    assert_eq!(deleted.code, Some(0), "{deleted:?}");
    assert!(
        deleted.stderr.contains(&format!(
            "⚠ Assets/Art/Red.mat: deleted, but {SCENE} still uses it"
        )),
        "{deleted:?}"
    );
}

/// R30: `gang pull` (or `gang merge`) that merges cleanly gets the same
/// reference checks as `gang sync`.
#[test]
fn a_clean_pull_checks_references_like_sync() {
    let team = Team::new();
    team.add_red("alice");
    assert_eq!(team.sync("alice", "red material").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));
    team.remove("alice", "Assets/Materials/Red.mat");
    team.remove("alice", "Assets/Materials/Red.mat.meta");
    assert_eq!(team.sync("alice", "no more red").code, Some(0));

    team.write("bob", SCENE, &scene("0", "2.5", RED_RENDERER));
    assert_eq!(
        team.gang("bob", &["git", "commit", "-q", "-am", "red player"])
            .code,
        Some(0)
    );
    let pull = team.gang("bob", &["pull", "--no-rebase", "--no-edit"]);
    assert_eq!(pull.code, Some(0), "{pull:?}");
    assert!(
        pull.stderr.contains(" deleted Assets/Materials/Red.mat ("),
        "{pull:?}"
    );
}

/// Thought experiment R31: the last asset of a folder deleted. git keeps no
/// empty folders, so a teammate's Unity would find Materials.meta alone and
/// delete it, and the next save here would bring a new one: noise back and
/// forth. `gang save` takes the empty folder along; a plain commit warns.
#[test]
fn a_folder_emptied_by_a_save_goes_with_its_last_asset() {
    let team = Team::new();
    team.add_red("alice");
    assert_eq!(team.sync("alice", "red material").code, Some(0));
    assert_eq!(team.sync("bob", "-").code, Some(0));

    team.remove("alice", "Assets/Materials/Red.mat");
    team.remove("alice", "Assets/Materials/Red.mat.meta");
    let save = team.gang("alice", &["save", "-m", "no more red"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(
        save.stdout
            .contains("✓ The empty folder Assets/Materials goes too"),
        "{save:?}"
    );
    let root = team.sandbox.dir.path();
    assert!(!root.join("alice/Assets/Materials").exists());
    assert!(!root.join("alice/Assets/Materials.meta").exists());
    assert_eq!(
        team.gang("alice", &["git", "ls-files", "Assets/Materials.meta"])
            .stdout,
        ""
    );

    // Plain git: the commit goes through with a warning.
    team.remove("bob", "Assets/Materials/Red.mat");
    team.remove("bob", "Assets/Materials/Red.mat.meta");
    let commit = team.gang("bob", &["git", "commit", "-q", "-am", "no more red"]);
    assert_eq!(commit.code, Some(0), "{commit:?}");
    assert!(
        commit
            .stderr
            .contains("nothing is left in Assets/Materials"),
        "{commit:?}"
    );
}

/// Thought experiment R32: `gang stash pop` onto teammates' changes. git
/// calls the committed version "ours" and the stash "theirs", so "mine"
/// picked the teammate's value. The stash is the user's own work: "mine"
/// now means the stash, and once every conflict is answered the pop is
/// complete and the stash is dropped, as after a clean pop.
#[test]
fn a_stash_pop_asks_with_the_stash_as_mine() {
    let team = Team::new();
    team.write("bob", SCENE, &scene("0", "3", ""));
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player
{
    int speed = 3;
}
",
    );
    assert_eq!(team.gang("bob", &["stash"]).code, Some(0));
    team.write("alice", SCENE, &scene("0", "4", ""));
    team.write(
        "alice",
        "Assets/Scripts/Player.cs",
        "class Player
{
    int speed = 4;
}
",
    );
    assert_eq!(team.sync("alice", "alice tuning").code, Some(0));
    assert_eq!(team.gang("bob", &["pull", "-q"]).code, Some(0));

    let pop = team.gang("bob", &["stash", "pop"]);
    assert_eq!(pop.code, Some(1), "{pop:?}");
    let first = team.gang("bob", &["bang", "--answer", "mine"]);
    assert!(first.stdout.contains("the committed version"), "{first:?}");
    let second = team.gang("bob", &["bang", "--answer", "mine"]);
    assert_eq!(second.code, Some(0), "{second:?}");
    assert!(team.read("bob", SCENE).contains("jumpHeight: 3"));
    assert!(
        team.read("bob", "Assets/Scripts/Player.cs")
            .contains("speed = 3")
    );
    assert!(
        second.stderr.contains("The stash is applied and dropped")
            || second.stdout.contains("The stash is applied and dropped"),
        "{second:?}"
    );
    assert_eq!(team.gang("bob", &["git", "stash", "list"]).stdout, "");
    // Unstaged, as after a clean pop.
    let status = team.gang("bob", &["git", "status", "--porcelain"]).stdout;
    assert!(status.contains(" M Assets/Scenes/Level1.unity"), "{status}");

    // Plain git: the stash names its side in the markers; it stays in the list.
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player
{
    int speed = 5;
}
",
    );
    assert_eq!(team.gang("bob", &["git", "stash", "-q"]).code, Some(0));
    team.write(
        "bob",
        "Assets/Scripts/Player.cs",
        "class Player
{
    int speed = 6;
}
",
    );
    assert_eq!(
        team.gang("bob", &["git", "commit", "-q", "-am", "six"])
            .code,
        Some(0)
    );
    assert_eq!(
        team.gang("bob", &["git", "stash", "pop", "-q"]).code,
        Some(1)
    );
    let answer = team.gang("bob", &["bang", "--answer", "mine"]);
    assert_eq!(answer.code, Some(0), "{answer:?}");
    assert!(
        team.read("bob", "Assets/Scripts/Player.cs")
            .contains("speed = 5")
    );
    assert!(
        answer.stderr.contains("gang stash drop") || answer.stdout.contains("gang stash drop"),
        "{answer:?}"
    );
}

/// R33, the incoming side: a teammate committed a script with conflict
/// markers past GANG's hooks (`--no-verify`, or a clone without GANG).
#[test]
fn conflict_markers_that_come_in_are_reported() {
    let team = Team::new();
    team.write(
        "alice",
        "Assets/Scripts/Player.cs",
        "class Player\n{\n<<<<<<< HEAD\n    int speed = 2;\n=======\n    int speed = 3;\n>>>>>>> main\n}\n",
    );
    assert_eq!(
        team.gang(
            "alice",
            &[
                "git",
                "commit",
                "-q",
                "--no-verify",
                "-am",
                "merged by hand"
            ]
        )
        .code,
        Some(0)
    );
    assert_eq!(
        team.gang("alice", &["git", "push", "-q", "--no-verify"])
            .code,
        Some(0)
    );

    team.write("bob", SCENE, &scene("1", "2.5", ""));
    let sync = team.sync("bob", "move player");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr
            .contains("Assets/Scripts/Player.cs came in with conflict markers (")
            && sync.stderr.contains("the scripts will not compile"),
        "{sync:?}"
    );
    let doctor = team.gang("bob", &["doctor"]);
    assert!(
        doctor
            .stdout
            .contains("Assets/Scripts/Player.cs has conflict markers committed (since "),
        "{doctor:?}"
    );
}

/// Thought experiment R36: two people add a reference to the same assembly
/// definition. git saw two edits of the line before (its new comma) and
/// stopped; the JSON driver keeps both references, with Unity's indent.
#[test]
fn references_added_to_one_asmdef_on_both_sides_merge() {
    let team = Team::new();
    let asmdef =
        "{\n    \"name\": \"Game\",\n    \"references\": [\n        \"GUID:aaaa\"\n    ]\n}\n";
    team.write("alice", "Assets/Game.asmdef", asmdef);
    team.write(
        "alice",
        "Assets/Game.asmdef.meta",
        "fileFormatVersion: 2\nguid: a1b2c3d4e5f60718293a4b5c6d7e8f90\n",
    );
    let first = team.sync("alice", "game assembly");
    assert_eq!(first.code, Some(0), "{first:?}");
    assert_eq!(team.sync("bob", "-").code, Some(0));

    team.write(
        "alice",
        "Assets/Game.asmdef",
        &asmdef.replace("\"GUID:aaaa\"", "\"GUID:aaaa\",\n        \"GUID:bbbb\""),
    );
    assert_eq!(team.sync("alice", "input system").code, Some(0));
    team.write(
        "bob",
        "Assets/Game.asmdef",
        &asmdef.replace("\"GUID:aaaa\"", "\"GUID:aaaa\",\n        \"GUID:cccc\""),
    );
    let sync = team.sync("bob", "cinemachine");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert_eq!(
        team.read("bob", "Assets/Game.asmdef"),
        "{\n    \"name\": \"Game\",\n    \"references\": [\n        \"GUID:aaaa\",\n        \"GUID:cccc\",\n        \"GUID:bbbb\"\n    ]\n}\n"
    );
}

/// R37: a repository set up by an older GANG keeps its old `.gitattributes`
/// (no assembly definition rule) until someone notices; gang doctor does.
#[test]
fn doctor_notices_managed_files_an_older_gang_wrote() {
    let team = Team::new();
    let attributes = team.read("alice", ".gitattributes");
    let fresh = team.gang("alice", &["doctor"]);
    assert!(
        !fresh.stdout.contains("differs from what this GANG writes"),
        "{fresh:?}"
    );
    team.write(
        "alice",
        ".gitattributes",
        &attributes.replace("*.asmdef merge=gang-json\n", ""),
    );
    let doctor = team.gang("alice", &["doctor"]);
    assert!(
        doctor
            .stdout
            .contains(".gitattributes differs from what this GANG writes"),
        "{doctor:?}"
    );
    let fix = team.gang("alice", &["doctor", "--fix"]);
    assert!(
        fix.stdout
            .contains("rewrote GANG's block in .gitattributes"),
        "{fix:?}"
    );
    assert_eq!(team.read("alice", ".gitattributes"), attributes);
}

/// R38: main only takes pull requests (GitHub branch protection). The push
/// is refused; GANG says how the commits get in, GANG's way.
#[test]
fn a_protected_branch_push_explains_the_way_in() {
    let team = Team::new();
    let hook = team.sandbox.dir.path().join("remote.git/hooks/pre-receive");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho \"error: GH006: Protected branch update failed for refs/heads/main.\" >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    team.write("alice", SCENE, &scene("3", "2.5", ""));
    let sync = team.sync("alice", "move player");
    assert_ne!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stderr
            .contains("only takes changes through pull requests"),
        "{sync:?}"
    );
    assert!(sync.stderr.contains("gang pr merge <number>"), "{sync:?}");
}

/// Thought experiment R40: during a merge, `gang add -A` marked the scene
/// settled as the merge driver left it, every open question answered "mine"
/// without a word, and `gang bang` then said "No conflicts" and committed.
/// The GANG commands that would do that stop; a plain `git add` (an
/// editor's "mark resolved") of the untouched file is undone by gang bang,
/// which asks the questions; a file changed by hand counts as answered.
#[test]
fn open_conflicts_are_not_settled_by_add_reset_or_checkout() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("1", "3", ""));
    assert_eq!(team.sync("alice", "alice tuning").code, Some(0));
    team.write("bob", SCENE, &scene("2", "4", ""));
    assert_eq!(team.sync("bob", "bob tuning").code, Some(1));
    for args in [
        vec!["add", "-A"],
        vec!["add", "Assets"],
        vec!["reset", "--", SCENE],
        vec!["rm", "--cached", SCENE],
        vec!["checkout", "origin/main", "--", SCENE],
        vec!["restore", "."],
    ] {
        let refused = team.gang("bob", &args);
        assert_eq!(refused.code, Some(1), "{args:?} {refused:?}");
        assert!(
            refused
                .stderr
                .contains("would mark this file settled as it stands, with 2 questions unanswered"),
            "{args:?} {refused:?}"
        );
    }
    // Plain git, as an editor's "mark resolved" does it.
    assert_eq!(team.gang("bob", &["git", "add", "-A"]).code, Some(0));
    let list = team.gang("bob", &["bang", "--list"]);
    assert!(
        list.stdout.contains(
            "was marked resolved outside GANG with 2 questions unanswered; they are asked again"
        ),
        "{list:?}"
    );
    assert!(list.stdout.contains("Jump Height  (open)"), "{list:?}");
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "theirs"]).code,
        Some(1)
    );
    assert_eq!(
        team.gang("bob", &["bang", "--answer", "theirs"]).code,
        Some(0)
    );
    assert_eq!(team.read("bob", SCENE), scene("1", "3", ""));

    // Changed by hand, then added: the person's own answer.
    team.write("alice", SCENE, &scene("5", "6", ""));
    assert_eq!(team.sync("alice", "alice again").code, Some(0));
    team.write("bob", SCENE, &scene("7", "8", ""));
    assert_eq!(team.sync("bob", "bob again").code, Some(1));
    team.write("bob", SCENE, &scene("9", "9", ""));
    assert_eq!(team.gang("bob", &["git", "add", "-A"]).code, Some(0));
    let bang = team.gang("bob", &["bang"]);
    assert!(
        bang.stdout.contains("was resolved by hand outside GANG"),
        "{bang:?}"
    );
    assert_eq!(team.read("bob", SCENE), scene("9", "9", ""));
}

/// R40 with plain git all the way: `git commit -a` during a merge would
/// commit the scene as the driver left it, the open questions answered
/// "mine". The hook stops it; gang bang asks them.
#[test]
fn commit_all_during_a_merge_does_not_settle_open_questions() {
    let team = Team::new();
    team.write("alice", SCENE, &scene("1", "3", ""));
    assert_eq!(team.sync("alice", "alice tuning").code, Some(0));
    team.write("bob", SCENE, &scene("2", "4", ""));
    assert_eq!(team.sync("bob", "bob tuning").code, Some(1));
    let commit = team.gang("bob", &["git", "commit", "-q", "-a", "--no-edit"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit.stderr.contains("Assets/Scenes/Level1.unity: marked resolved with 2 questions GANG asked still unanswered"),
        "{commit:?}"
    );
    assert!(team.sandbox.dir.path().join("bob/.git/MERGE_HEAD").exists());
}
