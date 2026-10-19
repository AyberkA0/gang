//! Reserved numbers in Unity's indexed tables (DESIGN.md §14.6): the first
//! person to name a new layer reserves its number; whoever comes second is
//! told before pushing, and `gang save` moves their layer with what uses it.

mod common;

use std::fs;

use common::{GANG, Sandbox};

const TAGS: &str = "ProjectSettings/TagManager.asset";
const SCENE: &str = "Assets/Scenes/Menu.unity";
const SCRIPT: &str = "Assets/Scripts/MenuCamera.cs";

fn tag_manager(extra: &[(usize, &str)]) -> String {
    let mut names = vec![
        "Default",
        "TransparentFX",
        "Ignore Raycast",
        "",
        "Water",
        "UI",
        "",
        "",
    ];
    names.resize(32, "");
    for (i, name) in extra {
        names[*i] = name;
    }
    let mut text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!78 &1\nTagManager:\n  serializedVersion: 3\n  tags: []\n  layers:\n".to_owned();
    for name in names {
        if name.is_empty() {
            text.push_str("  - \n");
        } else {
            text.push_str(&format!("  - {name}\n"));
        }
    }
    text.push_str("  m_SortingLayers:\n  - name: Default\n    uniqueID: 0\n    locked: 0\n");
    text
}

fn scene(layer: u32) -> String {
    format!(
        "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Component:\n  - component: {{fileID: 101}}\n  m_Layer: {layer}\n  m_Name: Button\n--- !u!4 &101\nTransform:\n  m_GameObject: {{fileID: 100}}\n  m_Children: []\n  m_Father: {{fileID: 0}}\n"
    )
}

struct Team {
    sandbox: Sandbox,
}

impl Team {
    fn new() -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        sandbox.write(TAGS, &tag_manager(&[]));
        sandbox.write(SCENE, &scene(0));
        sandbox.write(
            &format!("{SCENE}.meta"),
            "fileFormatVersion: 2\nguid: 22223333444455556666777788889999\n",
        );
        sandbox.write(SCRIPT, "class MenuCamera\n{\n    int mask = 0;\n}\n");
        sandbox.write(
            &format!("{SCRIPT}.meta"),
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000cccc\n",
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
            sandbox.run_in(user, "git", &["config", "core.autocrlf", "false"]);
        }
        Team { sandbox }
    }

    fn gang(&self, user: &str, args: &[&str]) -> common::Outcome {
        self.sandbox.run_in(user, GANG, args)
    }

    fn git(&self, user: &str, args: &[&str]) -> common::Outcome {
        self.sandbox.run_in(user, "git", args)
    }

    fn write(&self, user: &str, path: &str, text: &str) {
        let full = self.sandbox.dir.path().join(user).join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    }

    fn read(&self, user: &str, path: &str) -> String {
        fs::read_to_string(self.sandbox.dir.path().join(user).join(path)).unwrap()
    }
}

#[test]
fn the_second_new_layer_on_a_number_moves_before_it_is_shared() {
    let team = Team::new();

    // Alice names layer 8 and runs any GANG command: it is reserved.
    team.write("alice", TAGS, &tag_manager(&[(8, "Projectile")]));
    let alice = team.gang("alice", &["layers"]);
    assert_eq!(alice.code, Some(0), "{alice:?}");
    assert!(
        alice
            .stdout
            .contains("Layer 8 \"Projectile\" is reserved for you"),
        "{alice:?}"
    );

    // Bob names the same number differently and starts using it, in a
    // scene, an existing script and a new one.
    team.write("bob", TAGS, &tag_manager(&[(8, "Interactable")]));
    team.write("bob", SCENE, &scene(8));
    team.write(
        "bob",
        SCRIPT,
        "class MenuCamera\n{\n    int mask = 1 << 8;\n}\n",
    );
    team.write(
        "bob",
        "Assets/Scripts/Door.cs",
        "class Door\n{\n    void Start() { gameObject.layer = 8; }\n}\n",
    );
    team.write(
        "bob",
        "Assets/Scripts/Door.cs.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\n",
    );
    let list = team.gang("bob", &["layers"]);
    assert!(
        list.stdout.contains("reserved for \"Projectile\" by alice"),
        "{list:?}"
    );

    // A plain commit is stopped, with the fix.
    team.git("bob", &["add", "-A"]);
    let commit = team.git("bob", &["commit", "-q", "-m", "menu"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit
            .stderr
            .contains("Layer 8 is reserved for \"Projectile\" by alice"),
        "{commit:?}"
    );
    assert!(commit.stderr.contains("Fix: gang save"), "{commit:?}");
    team.git("bob", &["reset", "-q"]);

    // `gang save` moves Bob's layer with everything that uses it.
    let save = team.gang("bob", &["save", "-m", "menu"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(
        save.stdout
            .contains("Moved your layer \"Interactable\" from 8 to 9"),
        "{save:?}"
    );
    let tags = team.read("bob", TAGS);
    assert!(tags.contains("  - \n  - Interactable\n"), "{tags}");
    assert!(team.read("bob", SCENE).contains("m_Layer: 9"));
    assert!(team.read("bob", SCRIPT).contains("1 << 9"));
    assert!(
        team.read("bob", "Assets/Scripts/Door.cs")
            .contains("gameObject.layer = 9")
    );
    let log = team.git("bob", &["log", "-1", "--format=%s"]);
    assert_eq!(log.stdout.trim(), "menu");

    // Bob now holds 9; both push and the TagManagers merge without a word.
    let layers = team.gang("bob", &["layers", "--help"]);
    assert_eq!(layers.code, Some(0));
    let json = team.gang("bob", &["--json", "layers"]);
    assert!(json.stdout.contains("\"index\": 9"), "{json:?}");
    assert!(json.stdout.contains("\"owner\": \"bob\""), "{json:?}");
    assert_eq!(team.gang("bob", &["push", "-q"]).code, Some(0));
    assert_eq!(
        team.gang("alice", &["sync", "-m", "projectile"]).code,
        Some(0)
    );
    let tags = team.read("alice", TAGS);
    assert!(
        tags.contains("  - Projectile\n  - Interactable\n"),
        "{tags}"
    );
    // Both layers reached the main branch: their reservations are gone.
    assert_eq!(team.gang("alice", &["layers"]).code, Some(0));
    let json = team.gang("alice", &["--json", "layers"]);
    assert_eq!(json.stdout.trim(), "[]", "{json:?}");
}

#[test]
fn unity_open_stops_the_move_and_says_why() {
    let team = Team::new();
    team.write("alice", TAGS, &tag_manager(&[(8, "Projectile")]));
    assert_eq!(team.gang("alice", &["layers"]).code, Some(0));
    team.write("bob", TAGS, &tag_manager(&[(8, "Interactable")]));

    let temp = team.sandbox.dir.path().join("bob").join("Temp");
    fs::create_dir_all(&temp).unwrap();
    let file = fs::File::create(temp.join("UnityLockfile")).unwrap();
    let mut lock = fd_lock::RwLock::new(file);
    let held = lock.write().unwrap();
    let save = team.gang("bob", &["save", "-m", "tags"]);
    assert_eq!(save.code, Some(1), "{save:?}");
    assert!(save.stderr.contains("Unity is open"), "{save:?}");
    assert!(save.stderr.contains("LayerMask.NameToLayer"), "{save:?}");
    // Nothing was staged.
    let staged = team.git("bob", &["diff", "--cached", "--name-only"]);
    assert_eq!(staged.stdout, "", "{staged:?}");
    drop(held);

    let save = team.gang("bob", &["save", "-m", "tags"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(team.read("bob", TAGS).contains("  - Interactable\n"));
}

#[test]
fn the_same_name_on_the_same_number_is_one_layer() {
    let team = Team::new();
    team.write("alice", TAGS, &tag_manager(&[(8, "Enemy")]));
    assert_eq!(team.gang("alice", &["layers"]).code, Some(0));
    team.write("bob", TAGS, &tag_manager(&[(8, "Enemy")]));
    let save = team.gang("bob", &["save", "-m", "enemy layer"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(!save.stdout.contains("Moved"), "{save:?}");
    assert!(team.read("bob", TAGS).contains("  - Enemy\n"));
}

#[test]
fn a_released_layer_frees_its_number() {
    let team = Team::new();
    team.write("alice", TAGS, &tag_manager(&[(8, "Projectile")]));
    assert_eq!(team.gang("alice", &["layers"]).code, Some(0));
    // Alice changes her mind and removes the layer: the next command
    // releases it.
    team.write("alice", TAGS, &tag_manager(&[]));
    assert_eq!(team.gang("alice", &["layers"]).code, Some(0));
    let json = team.gang("alice", &["--json", "layers"]);
    assert_eq!(json.stdout.trim(), "[]", "{json:?}");

    team.write("bob", TAGS, &tag_manager(&[(8, "Interactable")]));
    let save = team.gang("bob", &["save", "-m", "tags"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(!save.stdout.contains("Moved"), "{save:?}");
    assert!(team.read("bob", TAGS).contains("  - Interactable\n"));
}

/// With a Unity editor on the machine, the compiler finds the layer numbers
/// that no wording gives away: a constant without "layer" in its name passed
/// through the project's own method, a bare mask, and a serialized field
/// whose value lives in a scene. A number that never reaches Unity as a
/// layer stays as it is.
#[test]
fn the_compiler_finds_layer_numbers_the_wording_hides() {
    if gang_core::unity::editor_data_dirs(None).is_empty() {
        eprintln!("skipped: no Unity editor installed");
        return;
    }
    let team = Team::new();
    team.write("alice", TAGS, &tag_manager(&[(8, "Projectile")]));
    assert_eq!(team.gang("alice", &["layers"]).code, Some(0));

    team.write("bob", TAGS, &tag_manager(&[(8, "Door")]));
    let door = "using UnityEngine;\n\
\n\
public class Door : MonoBehaviour\n\
{\n\
    const int Interact = 8;\n\
    [SerializeField] int target = 8;\n\
    [SerializeField] int count = 8;\n\
\n\
    static void Put(GameObject go, int slot) { go.layer = slot; }\n\
\n\
    void Start()\n\
    {\n\
        Put(gameObject, Interact);\n\
        Physics.Raycast(transform.position, Vector3.forward, 10f, 256);\n\
        gameObject.layer = target;\n\
        int unrelated = 8;\n\
        Debug.Log(unrelated + count);\n\
    }\n\
}\n";
    team.write("bob", "Assets/Scripts/Door.cs", door);
    team.write(
        "bob",
        "Assets/Scripts/Door.cs.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000d00d\n",
    );
    let scene = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Component:\n  - component: {fileID: 101}\n  - component: {fileID: 102}\n  m_Layer: 0\n  m_Name: Button\n--- !u!4 &101\nTransform:\n  m_GameObject: {fileID: 100}\n  m_Children: []\n  m_Father: {fileID: 0}\n--- !u!114 &102\nMonoBehaviour:\n  m_GameObject: {fileID: 100}\n  m_Script: {fileID: 11500000, guid: 0000000000000000000000000000d00d, type: 3}\n  target: 8\n  count: 8\n";
    team.write("bob", SCENE, scene);

    let code = team.gang("bob", &["layers", "code"]);
    assert!(
        code.stdout.contains("Assets/Scripts/Door.cs:5  layer 8"),
        "{code:?}"
    );
    assert!(
        code.stdout.contains("GameObject.layer ← slot ← Interact"),
        "{code:?}"
    );
    let save = team.gang("bob", &["save", "-m", "door"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(
        save.stdout
            .contains("Moved your layer \"Door\" from 8 to 9"),
        "{save:?}"
    );
    let code = team.read("bob", "Assets/Scripts/Door.cs");
    assert!(code.contains("const int Interact = 9;"), "{code}");
    assert!(code.contains("Vector3.forward, 10f, 512)"), "{code}");
    assert!(code.contains("int target = 9;"), "{code}");
    assert!(code.contains("int count = 8;"), "{code}");
    assert!(code.contains("int unrelated = 8;"), "{code}");
    let scene = team.read("bob", SCENE);
    assert!(scene.contains("  target: 9\n"), "{scene}");
    assert!(scene.contains("  count: 8\n"), "{scene}");
}
