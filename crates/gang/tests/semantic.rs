//! P6: Unity-aware commands — tree, log --unity, object restore, noise.

mod common;

use common::{GANG, Sandbox};

const SCENE_PATH: &str = "Assets/Scenes/Level1.unity";

const SCENE: &str = "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!104 &2
RenderSettings:
  m_IndirectSpecularColor: {r: 0, g: 0, b: 0, a: 1}
--- !u!1 &100
GameObject:
  m_Component:
  - component: {fileID: 101}
  m_Name: Environment
--- !u!4 &101
Transform:
  m_GameObject: {fileID: 100}
  m_LocalPosition: {x: 0, y: 0, z: 0}
  m_Children:
  - {fileID: 201}
  m_Father: {fileID: 0}
--- !u!1 &200
GameObject:
  m_Component:
  - component: {fileID: 201}
  - component: {fileID: 202}
  m_Name: Player
--- !u!4 &201
Transform:
  m_GameObject: {fileID: 200}
  m_LocalPosition: {x: 0, y: 1, z: 0}
  m_Children: []
  m_Father: {fileID: 101}
--- !u!114 &202
MonoBehaviour:
  m_GameObject: {fileID: 200}
  m_Script: {fileID: 11500000, guid: 0000000000000000000000000000c0de, type: 3}
  jumpHeight: 2.5
";

fn project() -> Sandbox {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    sandbox.write(SCENE_PATH, SCENE);
    sandbox.write(
        &format!("{SCENE_PATH}.meta"),
        "fileFormatVersion: 2\nguid: 11112222333344445555666677778888\n",
    );
    sandbox.write(
        "Assets/Scripts/PlayerController.cs",
        "class PlayerController {}\n",
    );
    sandbox.write(
        "Assets/Scripts/PlayerController.cs.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000c0de\n",
    );
    sandbox.write(
        "Assets/Scripts.meta",
        "fileFormatVersion: 2\nguid: 99998888777766665555444433332222\nfolderAsset: yes\n",
    );
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "start"]);
    sandbox
}

#[test]
fn tree_shows_objects_and_scripts() {
    let sandbox = project();
    let tree = sandbox.run(GANG, &["tree", SCENE_PATH]);
    assert_eq!(tree.code, Some(0), "{tree:?}");
    assert!(tree.stdout.contains("Environment"), "{}", tree.stdout);
    assert!(tree.stdout.contains("└─ Player"), "{}", tree.stdout);
    assert!(tree.stdout.contains("&200"), "{}", tree.stdout);
    assert!(tree.stdout.contains("PlayerController"), "{}", tree.stdout);

    let json = sandbox.run(
        GANG,
        &[
            "--json",
            "tree",
            &format!("{SCENE_PATH}::Environment/Player"),
        ],
    );
    let objects: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(objects[0]["path"], "Environment/Player");
}

#[test]
fn log_unity_summarizes_object_changes() {
    let sandbox = project();
    sandbox.write(
        SCENE_PATH,
        &SCENE
            .replace("{x: 0, y: 1, z: 0}", "{x: 2, y: 1, z: 0}")
            .replace("jumpHeight: 2.5", "jumpHeight: 3"),
    );
    sandbox.git(&["commit", "-q", "-am", "Tune jump"]);
    let log = sandbox.run(GANG, &["log", "--unity", "-1"]);
    assert_eq!(log.code, Some(0), "{log:?}");
    assert!(log.stdout.contains("Tune jump"), "{}", log.stdout);
    assert!(
        log.stdout
            .contains("~ Environment/Player › Transform › Position.x  0 → 2"),
        "{}",
        log.stdout
    );
    assert!(
        log.stdout
            .contains("~ Environment/Player › PlayerController › Jump Height  2.5 → 3"),
        "{}",
        log.stdout
    );
}

#[test]
fn restore_one_object_keeps_other_changes() {
    let sandbox = project();
    let edited = SCENE
        .replace(
            "m_LocalPosition: {x: 0, y: 1, z: 0}",
            "m_LocalPosition: {x: 9, y: 1, z: 0}",
        )
        .replace("m_Name: Environment", "m_Name: World");
    sandbox.write(SCENE_PATH, &edited);
    let restore = sandbox.run(GANG, &["restore", &format!("{SCENE_PATH}::World/Player")]);
    assert_eq!(restore.code, Some(0), "{restore:?}");
    assert!(restore.stdout.contains("Your other changes"), "{restore:?}");
    assert_eq!(
        sandbox.read(SCENE_PATH),
        SCENE.replace("m_Name: Environment", "m_Name: World")
    );
}

#[test]
fn noise_is_dropped_and_not_published() {
    let sandbox = project();
    sandbox.write(
        SCENE_PATH,
        &SCENE.replace("{r: 0, g: 0, b: 0, a: 1}", "{r: 0.4, g: 0.4, b: 0.5, a: 1}"),
    );
    sandbox.write("Assets/Other.txt", "real change");

    let who = sandbox.run(GANG, &["--json", "who"]);
    let team: serde_json::Value = serde_json::from_str(&who.stdout).unwrap();
    let files = team[0]["files"].as_object().unwrap();
    assert!(
        !files.contains_key(SCENE_PATH),
        "noise is not a change: {files:?}"
    );
    assert!(files.contains_key("Assets/Other.txt"));

    let restore = sandbox.run(GANG, &["restore", "--noise"]);
    assert_eq!(restore.code, Some(0), "{restore:?}");
    assert!(
        restore
            .stdout
            .contains("restored Assets/Scenes/Level1.unity"),
        "{restore:?}"
    );
    assert_eq!(sandbox.read(SCENE_PATH), SCENE);
}
