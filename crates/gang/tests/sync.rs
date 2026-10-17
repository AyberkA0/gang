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
        for user in ["alice", "bob"] {
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

    let bang = team.gang("bob", &["bang", "--prefer", "theirs"]);
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
    let bang = team.gang("bob", &["bang", "--prefer", "mine"]);
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
        team.gang("bob", &["bang", "--prefer", "mine"]).code,
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

#[test]
fn layer_collision_is_remapped_automatically() {
    let team = Team::new();
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

fn gang_core_layer_names(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.starts_with("  layers:"))
        .skip(1)
        .take_while(|l| l.starts_with("  -"))
        .map(|l| l.trim_start_matches("  -").trim().to_owned())
        .collect()
}
