//! P9–P10: file, object, auto and bake locks between clones.

mod common;

use std::thread;

use common::{GANG, Sandbox};

const SCENE: &str = "Assets/Scenes/Level1.unity";
const PLAYER_SCRIPT: &str = "0000000000000000000000000000aaaa";
const CAMERA_SCRIPT: &str = "0000000000000000000000000000cccc";

fn scene(player_x: &str, tree_x: &str) -> String {
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
  m_LocalPosition: {{x: 0, y: 0, z: 0}}
  m_Children:
  - {{fileID: 201}}
  - {{fileID: 401}}
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
  m_Script: {{fileID: 11500000, guid: {PLAYER_SCRIPT}, type: 3}}
  jumpHeight: 2.5
--- !u!1 &300
GameObject:
  m_Component:
  - component: {{fileID: 301}}
  - component: {{fileID: 302}}
  m_Name: Main Camera
--- !u!4 &301
Transform:
  m_GameObject: {{fileID: 300}}
  m_LocalPosition: {{x: 0, y: 5, z: -10}}
  m_Children: []
  m_Father: {{fileID: 0}}
--- !u!114 &302
MonoBehaviour:
  m_GameObject: {{fileID: 300}}
  m_Script: {{fileID: 11500000, guid: {CAMERA_SCRIPT}, type: 3}}
  target: {{fileID: 201}}
--- !u!1 &400
GameObject:
  m_Component:
  - component: {{fileID: 401}}
  m_Name: Tree
--- !u!4 &401
Transform:
  m_GameObject: {{fileID: 400}}
  m_LocalPosition: {{x: {tree_x}, y: 0, z: 0}}
  m_Children: []
  m_Father: {{fileID: 101}}
"
    )
}

struct Team {
    sandbox: Sandbox,
}

impl Team {
    fn new(users: &[&str]) -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        sandbox.write(SCENE, &scene("0", "0"));
        sandbox.write(
            &format!("{SCENE}.meta"),
            "fileFormatVersion: 2\nguid: 11112222333344445555666677778888\n",
        );
        sandbox.write(
            "Assets/Scripts/PlayerController.cs",
            "class PlayerController {}\n",
        );
        sandbox.write(
            "Assets/Scripts/PlayerController.cs.meta",
            &format!("fileFormatVersion: 2\nguid: {PLAYER_SCRIPT}\n"),
        );
        sandbox.write(
            "Assets/Scripts/CameraFollow.cs",
            "class CameraFollow { UnityEngine.Transform target; }\n",
        );
        sandbox.write(
            "Assets/Scripts/CameraFollow.cs.meta",
            &format!("fileFormatVersion: 2\nguid: {CAMERA_SCRIPT}\n"),
        );
        sandbox.write(
            "Assets/Scripts.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000bbbb\nfolderAsset: yes\n",
        );
        sandbox.write("Assets/Art/Notes.txt", "notes\n");
        sandbox.write(
            "Assets/Art/Notes.txt.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\n",
        );
        sandbox.write(
            "Assets/Art.meta",
            "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\nfolderAsset: yes\n",
        );
        assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
        sandbox.git(&["add", "-A"]);
        sandbox.git(&["commit", "-q", "-m", "start"]);
        let remote = sandbox.dir.path().join("remote.git");
        sandbox.git(&["clone", "-q", "--bare", ".", remote.to_str().unwrap()]);
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        for user in users {
            assert_eq!(
                sandbox.run_in("", GANG, &["clone", "-q", &url, user]).code,
                Some(0)
            );
            sandbox.run_in(user, "git", &["config", "gang.user", user]);
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
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
}

#[test]
fn a_locked_file_cannot_be_committed_by_others() {
    let team = Team::new(&["alice", "bob"]);
    let lock = team.gang(
        "alice",
        &["lock", "Assets/Art/Notes.txt", "-m", "rewriting"],
    );
    assert_eq!(lock.code, Some(0), "{lock:?}");
    assert!(
        lock.stdout
            .contains("🔒 Locked Assets/Art/Notes.txt (+ .meta)"),
        "{lock:?}"
    );

    let locks = team.gang("bob", &["locks"]);
    assert!(
        locks.stdout.contains("Assets/Art/Notes.txt") && locks.stdout.contains("alice"),
        "{locks:?}"
    );

    team.write("bob", "Assets/Art/Notes.txt", "bob's notes\n");
    team.git("bob", &["add", "-A"]);
    let commit = team.git("bob", &["commit", "-m", "notes"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit
            .stderr
            .contains("Assets/Art/Notes.txt is locked by alice (\"rewriting\""),
        "{commit:?}"
    );

    // Someone else cannot take it, and cannot release it without --force.
    let steal = team.gang("bob", &["lock", "Assets/Art/Notes.txt"]);
    assert_eq!(steal.code, Some(1));
    assert!(steal.stderr.contains("locked by alice"), "{steal:?}");
    assert_eq!(
        team.gang("bob", &["unlock", "Assets/Art/Notes.txt"]).code,
        Some(1)
    );

    assert_eq!(
        team.gang("alice", &["unlock", "Assets/Art/Notes.txt"]).code,
        Some(0)
    );
    team.gang("bob", &["locks"]);
    assert_eq!(
        team.git("bob", &["commit", "-q", "-m", "notes"]).code,
        Some(0)
    );
}

#[test]
fn a_bypassed_commit_is_stopped_at_push() {
    let team = Team::new(&["alice", "bob"]);
    team.write("bob", "Assets/Art/Notes.txt", "bob's notes\n");
    team.git("bob", &["commit", "-q", "-am", "notes"]);
    assert_eq!(
        team.gang("alice", &["lock", "Assets/Art/Notes.txt"]).code,
        Some(0)
    );
    let push = team.git("bob", &["push", "-q"]);
    assert_eq!(push.code, Some(1), "{push:?}");
    assert!(push.stderr.contains("Push stopped by GANG"), "{push:?}");
}

#[test]
fn only_one_of_many_gets_the_lock() {
    let users = ["u1", "u2", "u3", "u4"];
    let team = Team::new(&users);
    let results: Vec<Option<i32>> = thread::scope(|scope| {
        let handles: Vec<_> = users
            .iter()
            .map(|user| {
                let team = &team;
                scope.spawn(move || team.gang(user, &["lock", "Assets/Art/Notes.txt"]).code)
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        results.iter().filter(|c| **c == Some(0)).count(),
        1,
        "{results:?}"
    );
}

#[test]
fn object_lock_level_2_covers_scripts_and_interacting_objects() {
    let team = Team::new(&["alice", "bob"]);
    let preview = team.gang(
        "alice",
        &[
            "lock",
            &format!("{SCENE}::Environment/Player"),
            "--level",
            "2",
            "--dry-run",
        ],
    );
    assert_eq!(preview.code, Some(0), "{preview:?}");
    assert!(
        preview
            .stdout
            .contains("Assets/Scripts/PlayerController.cs"),
        "{}",
        preview.stdout
    );
    assert!(
        preview.stdout.contains("Level1.unity › Main Camera"),
        "{}",
        preview.stdout
    );
    assert!(
        preview.stdout.contains("CameraFollow.target → Player"),
        "{}",
        preview.stdout
    );
    assert!(
        preview.stdout.contains("Assets/Scripts/CameraFollow.cs"),
        "{}",
        preview.stdout
    );
    assert!(!preview.stdout.contains("Tree"), "{}", preview.stdout);

    let lock = team.gang(
        "alice",
        &[
            "lock",
            &format!("{SCENE}::Environment/Player"),
            "--level",
            "2",
            "-m",
            "player rework",
            "--yes",
        ],
    );
    assert_eq!(lock.code, Some(0), "{lock:?}");
    team.gang("bob", &["locks"]);

    // Moving the Tree in the same scene is fine.
    team.write("bob", SCENE, &scene("0", "7"));
    team.git("bob", &["add", "-A"]);
    assert_eq!(
        team.git("bob", &["commit", "-q", "-m", "tree"]).code,
        Some(0)
    );

    // Moving the Player is not.
    team.write("bob", SCENE, &scene("3", "7"));
    team.git("bob", &["add", "-A"]);
    let commit = team.git("bob", &["commit", "-m", "player"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit
            .stderr
            .contains("Level1.unity › Environment › Player is locked by alice (\"player rework\""),
        "{commit:?}"
    );
    assert!(
        commit
            .stderr
            .contains("you changed Environment › Player › Transform › Position.x"),
        "{commit:?}"
    );
    assert!(
        commit
            .stderr
            .contains("gang restore Assets/Scenes/Level1.unity::Environment/Player"),
        "{commit:?}"
    );

    // The script on it is locked too.
    team.git("bob", &["reset", "-q", "--hard"]);
    team.write(
        "bob",
        "Assets/Scripts/PlayerController.cs",
        "class PlayerController { int x; }\n",
    );
    team.git("bob", &["add", "-A"]);
    assert_eq!(team.git("bob", &["commit", "-m", "script"]).code, Some(1));

    let unlock = team.gang(
        "alice",
        &["unlock", &format!("{SCENE}::Environment/Player")],
    );
    assert!(
        unlock
            .stdout
            .contains("Released Level1.unity › Environment › Player and"),
        "{unlock:?}"
    );
}

#[test]
fn auto_locks_skip_code_and_go_away_after_push() {
    let team = Team::new(&["alice"]);
    team.write("alice", SCENE, &scene("1", "0"));
    team.write(
        "alice",
        "Assets/Scripts/PlayerController.cs",
        "class PlayerController { int y; }\n",
    );
    let auto = team.gang("alice", &["lock", "--auto"]);
    assert_eq!(auto.code, Some(0), "{auto:?}");
    assert!(
        auto.stdout.contains("skipped — code merges automatically"),
        "{}",
        auto.stdout
    );
    assert!(
        auto.stdout.contains("🔒 Assets/Scenes/Level1.unity"),
        "{}",
        auto.stdout
    );
    assert!(team.gang("alice", &["locks"]).stdout.contains("auto"));

    let sync = team.gang("alice", &["sync", "-m", "work"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(sync.stdout.contains("Released 1 auto lock"), "{sync:?}");
    assert_eq!(team.gang("alice", &["locks"]).stdout, "No locks.\n");
}

#[test]
fn bake_outputs_need_a_bake_lock() {
    let team = Team::new(&["alice", "bob"]);
    let lighting = "Assets/Scenes/Level1/LightingData.asset";
    team.write("bob", lighting, "baked");
    team.write(
        "bob",
        &format!("{lighting}.meta"),
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000ffff\n",
    );
    team.write(
        "bob",
        "Assets/Scenes/Level1.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000abcd\nfolderAsset: yes\n",
    );
    team.git("bob", &["add", "-A"]);
    let commit = team.git("bob", &["commit", "-m", "bake"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit.stderr.contains("bake outputs need a bake lock"),
        "{commit:?}"
    );
    assert!(
        commit
            .stderr
            .contains("gang lock Assets/Scenes/Level1.unity --bake"),
        "{commit:?}"
    );

    let lock = team.gang("bob", &["lock", SCENE, "--bake", "-m", "lighting pass"]);
    assert_eq!(lock.code, Some(0), "{lock:?}");
    assert!(
        lock.stdout
            .contains("The rest of the scene stays editable."),
        "{lock:?}"
    );
    assert_eq!(
        team.git("bob", &["commit", "-q", "-m", "bake"]).code,
        Some(0)
    );

    // Others may still edit the scene itself.
    team.gang("alice", &["locks"]);
    team.write("alice", SCENE, &scene("0", "4"));
    team.git("alice", &["add", "-A"]);
    assert_eq!(
        team.git("alice", &["commit", "-q", "-m", "tree"]).code,
        Some(0)
    );

    assert_eq!(team.gang("bob", &["unlock", SCENE, "--bake"]).code, Some(0));
}

#[test]
fn force_unlock_tells_the_team() {
    let team = Team::new(&["alice", "bob"]);
    assert_eq!(
        team.gang("alice", &["lock", "Assets/Art/Notes.txt"]).code,
        Some(0)
    );
    let force = team.gang("bob", &["unlock", "Assets/Art/Notes.txt", "--force"]);
    assert_eq!(force.code, Some(0), "{force:?}");
    assert!(force.stdout.contains("(held by alice)"), "{force:?}");
    team.gang("bob", &["presence-publish"]);
    let who = team.gang("alice", &["--json", "who"]);
    assert!(who.stdout.contains("force_unlock"), "{}", who.stdout);
}

/// Field test A2: a save refused by a lock must not stage anything.
#[test]
fn a_refused_save_leaves_the_index_alone() {
    let team = Team::new(&["alice", "bob"]);
    assert_eq!(
        team.gang("alice", &["lock", "Assets/Art/Notes.txt", "-m", "rewriting"])
            .code,
        Some(0)
    );
    team.gang("bob", &["locks"]);
    team.write("bob", "Assets/Art/Notes.txt", "bob's notes\n");
    team.write(
        "bob",
        "Assets/Scripts/CameraFollow.cs",
        "class CameraFollow { UnityEngine.Transform target; int x; }\n",
    );
    let save = team.gang("bob", &["save", "-m", "notes"]);
    assert_eq!(save.code, Some(1), "{save:?}");
    assert!(save.stderr.contains("locked by alice"), "{save:?}");
    let staged = team.git("bob", &["diff", "--cached", "--name-only"]);
    assert_eq!(staged.stdout, "", "a refused save staged files");
    let changed = team.git("bob", &["diff", "--name-only"]);
    assert!(changed.stdout.contains("Assets/Art/Notes.txt"), "{changed:?}");
}
