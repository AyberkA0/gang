//! Field test A1: a prefab rebuilt with the same GUID and new object ids.
//! Every instance's overrides point at the old ids; GANG points them at the
//! new objects, for the person who rebuilt it and for teammates.

mod common;

use common::{GANG, Sandbox};

const GUID: &str = "1234567890abcdef1234567890abcdef";
const PREFAB: &str = "Assets/Prefabs/Imp.prefab";
const SCENE: &str = "Assets/Scenes/Level.unity";

fn imp(ids: [i64; 3]) -> String {
    let [go, transform, script] = ids;
    format!(
        "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &{go}\nGameObject:\n  m_Component:\n  - component: {{fileID: {transform}}}\n  - component: {{fileID: {script}}}\n  m_Name: Imp\n--- !u!4 &{transform}\nTransform:\n  m_GameObject: {{fileID: {go}}}\n  m_LocalPosition: {{x: 0, y: 0, z: 0}}\n  m_Children: []\n  m_Father: {{fileID: 0}}\n--- !u!114 &{script}\nMonoBehaviour:\n  m_GameObject: {{fileID: {go}}}\n  speed: 3\n"
    )
}

/// A scene with one Imp per `(instance id, x)`, its overrides pointing at
/// the prefab's GameObject and Transform ids.
fn level(go: i64, transform: i64, imps: &[(i64, &str)]) -> String {
    let mut text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n".to_owned();
    for (id, x) in imps {
        text.push_str(&format!(
            "--- !u!1001 &{id}\nPrefabInstance:\n  m_Modification:\n    m_TransformParent: {{fileID: 0}}\n    m_Modifications:\n    - target: {{fileID: {transform}, guid: {GUID}, type: 3}}\n      propertyPath: m_LocalPosition.x\n      value: {x}\n      objectReference: {{fileID: 0}}\n    - target: {{fileID: {go}, guid: {GUID}, type: 3}}\n      propertyPath: m_Name\n      value: Imp{id}\n      objectReference: {{fileID: 0}}\n    m_RemovedComponents: []\n  m_SourcePrefab: {{fileID: 100100000, guid: {GUID}, type: 3}}\n"
        ));
    }
    text
}

struct Team {
    sandbox: Sandbox,
}

impl Team {
    fn new() -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        sandbox.write(PREFAB, &imp([1, 2, 3]));
        sandbox.write(
            &format!("{PREFAB}.meta"),
            &format!("fileFormatVersion: 2\nguid: {GUID}\n"),
        );
        sandbox.write(
            "Assets/Prefabs.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000aaaa\nfolderAsset: yes\n",
        );
        sandbox.write(SCENE, &level(1, 2, &[(500, "5")]));
        sandbox.write(
            &format!("{SCENE}.meta"),
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000bbbb\n",
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
            sandbox.run_in(user, "git", &["config", "core.autocrlf", "false"]);
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
}

#[test]
fn a_rebuilt_prefab_keeps_every_instance_override() {
    let team = Team::new();

    // Bob places a second Imp; not pushed yet.
    team.write("bob", SCENE, &level(1, 2, &[(500, "5"), (600, "9")]));
    assert_eq!(
        team.gang("bob", &["save", "-m", "second imp"]).code,
        Some(0)
    );

    // Alice rebuilds the prefab: same GUID, new ids. Her scene still points
    // at the old ones until she saves.
    team.write("alice", PREFAB, &imp([10, 20, 30]));
    let save = team.gang("alice", &["save", "-m", "rebuild imp"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(
        save.stderr
            .contains("Imp.prefab was rebuilt: its objects have new ids (3 found again, 0 gone)"),
        "{save:?}"
    );
    assert!(
        save.stdout
            .contains("Assets/Scenes/Level.unity: 2 references now point at the new objects"),
        "{save:?}"
    );
    assert_eq!(team.read("alice", SCENE), level(10, 20, &[(500, "5")]));
    assert_eq!(team.gang("alice", &["push", "-q"]).code, Some(0));

    // Bob syncs: the merge brings the rebuild; his own Imp follows it.
    let sync = team.gang("bob", &["sync"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(
        sync.stdout
            .contains("gang: point references at the rebuilt Imp.prefab"),
        "{sync:?}"
    );
    let scene = team.read("bob", SCENE);
    assert!(
        !scene.contains(&format!("fileID: 2, guid: {GUID}")),
        "{scene}"
    );
    assert!(
        !scene.contains(&format!("fileID: 1, guid: {GUID}")),
        "{scene}"
    );
    assert_eq!(scene, level(10, 20, &[(500, "5"), (600, "9")]));
}

#[test]
fn objects_that_are_gone_are_reported() {
    let team = Team::new();
    // The rebuild drops the script: the Transform and GameObject are found
    // again, the overrides on them move; nothing pointed at the script.
    let rebuilt = imp([10, 20, 30]).replace("  - component: {fileID: 30}\n", "");
    let rebuilt = rebuilt[..rebuilt.find("--- !u!114").unwrap()].to_owned();
    team.write("alice", PREFAB, &rebuilt);
    let save = team.gang("alice", &["save", "-m", "slimmer imp"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(save.stderr.contains("(2 found again, 1 gone)"), "{save:?}");
    assert_eq!(team.read("alice", SCENE), level(10, 20, &[(500, "5")]));
}
