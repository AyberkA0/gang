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
    let team = Team::with(&["alice", "bob", "carol"]);
    base_tags(&team, &["alice", "bob", "carol"], None);
    assert_eq!(team.sync("alice", "tags").code, Some(0));
    for user in ["bob", "carol"] {
        assert_eq!(team.sync(user, "-").code, Some(0));
    }
    // Bob shares UI3D = 8 on a branch; Carol picks the branch up.
    team.gang("bob", &["switch", "-c", "menu"]);
    base_tags(&team, &["bob"], Some("UI3D"));
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

    // Meanwhile Pickup = 8 lands on main; Bob merges main into the branch.
    base_tags(&team, &["alice"], Some("Pickup"));
    assert_eq!(team.sync("alice", "Pickup").code, Some(0));
    team.gang("bob", &["fetch"]);
    let merge = team.gang("bob", &["merge", "origin/main", "-m", "main"]);
    assert_eq!(merge.code, Some(0), "{merge:?}");
    assert_eq!(team.gang("bob", &["push"]).code, Some(0));

    // Carol syncs: her MenuPanel must end up on UI3D (9), not on Pickup (8).
    let sync = team.sync("carol", "-");
    assert_eq!(sync.code, Some(0), "{sync:?}");
    let text = team.read("carol", SCENE);
    let panel = text.split("m_Name: MenuPanel").next().unwrap();
    assert!(
        panel.trim_end().ends_with("m_Layer: 9"),
        "MenuPanel silently moved to Pickup: {text}"
    );
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

    assert_eq!(team.gang("bob", &["presence-publish", "--force"]).code, Some(0));
    let who = team.gang("alice", &["--json", "who"]);
    let bob = who.stdout.split("\"user\":").find(|p| p.contains("\"bob\"")).unwrap_or("");
    assert!(bob.contains(SCENE), "bob did change the scene: {}", who.stdout);
    assert!(!bob.contains(new_script), "Alice's file shown as Bob's: {}", who.stdout);
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
    assert_eq!(team.gang("bob", &["bang", "--list"]).stdout, "No conflicts to resolve.\n");

    let sync = team.gang("bob", &["sync"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(sync.stdout.contains("Finished the merge"), "{sync:?}");
    let log = team.sandbox.run_in("bob", "git", &["log", "-1", "--format=%s"]);
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
    assert!(team.read("bob", "Assets/Scripts/Draft.cs").contains("Draft"));
}
