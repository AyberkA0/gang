//! P4: guards in the pre-commit hook, and the safe working-tree commands.

mod common;

use std::fs;

use common::{GANG, Sandbox};

/// A GANG repository with a Unity project and one commit.
fn project() -> Sandbox {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    sandbox.write("Assets/Art/Hero.png", "png");
    sandbox.write(
        "Assets/Art/Hero.png.meta",
        "fileFormatVersion: 2\nguid: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
    sandbox.write(
        "Assets/Art.meta",
        "fileFormatVersion: 2\nguid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nfolderAsset: yes\n",
    );
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "start"]);
    sandbox
}

fn tracked(sandbox: &Sandbox) -> String {
    sandbox.run("git", &["ls-files"]).stdout
}

#[test]
fn commit_without_meta_is_blocked_even_with_plain_git() {
    let sandbox = project();
    sandbox.write("Assets/Art/Villain.png", "png");
    sandbox.git(&["add", "Assets/Art/Villain.png"]);
    let commit = sandbox.run("git", &["commit", "-m", "villain"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit
            .stderr
            .contains("Assets/Art/Villain.png: has no .meta"),
        "{commit:?}"
    );
    assert!(commit.stderr.contains("Commit stopped by GANG"));
}

#[test]
fn add_brings_the_meta_along_and_leaves_junk_out() {
    let sandbox = project();
    sandbox.write("Assets/Art/Villain.png", "png");
    sandbox.write(
        "Assets/Art/Villain.png.meta",
        "fileFormatVersion: 2\nguid: cccccccccccccccccccccccccccccccc\n",
    );
    sandbox.write("Assets/Art/.DS_Store", "junk");
    sandbox.write(
        ".gitignore",
        &(sandbox.read(".gitignore").replace(".DS_Store\n", "")),
    );

    let add = sandbox.run(
        GANG,
        &["add", "Assets/Art/Villain.png", "Assets/Art/.DS_Store"],
    );
    assert_eq!(add.code, Some(0), "{add:?}");
    assert!(
        add.stdout
            .contains("also added Assets/Art/Villain.png.meta"),
        "{add:?}"
    );
    assert!(
        add.stderr.contains("left out Assets/Art/.DS_Store"),
        "{add:?}"
    );
    let staged = sandbox
        .run("git", &["diff", "--cached", "--name-only"])
        .stdout;
    assert_eq!(
        staged,
        "Assets/Art/Villain.png\nAssets/Art/Villain.png.meta\n"
    );
    assert_eq!(
        sandbox.run(GANG, &["commit", "-q", "-m", "villain"]).code,
        Some(0)
    );
}

#[test]
fn new_folder_needs_its_meta() {
    let sandbox = project();
    sandbox.write("Assets/Audio/Theme.wav", "wav");
    sandbox.write(
        "Assets/Audio/Theme.wav.meta",
        "fileFormatVersion: 2\nguid: dddddddddddddddddddddddddddddddd\n",
    );
    sandbox.git(&[
        "add",
        "Assets/Audio/Theme.wav",
        "Assets/Audio/Theme.wav.meta",
    ]);
    let commit = sandbox.run("git", &["commit", "-m", "audio"]);
    assert_eq!(commit.code, Some(1));
    assert!(
        commit
            .stderr
            .contains("Assets/Audio: is a new folder without a .meta"),
        "{commit:?}"
    );
}

#[test]
fn case_only_differences_and_windows_names_are_blocked() {
    let sandbox = project();
    // Staged directly: on macOS and Windows the file system cannot hold both
    // spellings, which is exactly the problem. Git for Windows refuses
    // NTFS-invalid names in the index unless told otherwise.
    sandbox.git(&["config", "core.protectNTFS", "false"]);
    for path in ["Assets/art/Logo.png", "Assets/Bad:Name.txt"] {
        let blob = sandbox
            .command("git")
            .args(["hash-object", "-w", "--stdin"])
            .output()
            .unwrap();
        let blob = String::from_utf8(blob.stdout).unwrap();
        let info = format!("100644,{},{path}", blob.trim());
        sandbox.git(&["update-index", "--add", "--cacheinfo", &info]);
    }
    let commit = sandbox.run("git", &["commit", "-m", "bad"]);
    assert_eq!(commit.code, Some(1));
    assert!(
        commit
            .stderr
            .contains("Assets/art differs only in case from Assets/Art"),
        "{commit:?}"
    );
    assert!(
        commit.stderr.contains("which Windows does not allow"),
        "{commit:?}"
    );
}

#[test]
fn duplicate_guids_are_blocked() {
    let sandbox = project();
    sandbox.write("Assets/Art/Copy.png", "png");
    sandbox.write(
        "Assets/Art/Copy.png.meta",
        "fileFormatVersion: 2\nguid: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
    sandbox.git(&["add", "-A"]);
    let commit = sandbox.run("git", &["commit", "-m", "copy"]);
    assert_eq!(commit.code, Some(1));
    assert!(
        commit
            .stderr
            .contains("already used by Assets/Art/Hero.png.meta"),
        "{commit:?}"
    );
}

#[test]
fn unity_version_change_needs_permission() {
    let sandbox = project();
    sandbox.write(
        "ProjectSettings/ProjectVersion.txt",
        "m_EditorVersion: 6000.1.0f1\n",
    );
    sandbox.git(&["add", "-A"]);
    let commit = sandbox.run("git", &["commit", "-m", "upgrade"]);
    assert_eq!(commit.code, Some(1));
    assert!(commit.stderr.contains("different Unity version"));
    let allowed = sandbox
        .command("git")
        .env("GANG_ALLOW_UNITY_UPGRADE", "1")
        .args(["commit", "-q", "-m", "upgrade"])
        .status()
        .unwrap();
    assert!(allowed.success());
}

#[test]
fn rm_and_mv_take_the_meta_along() {
    let sandbox = project();
    let mv = sandbox.run(
        GANG,
        &["mv", "Assets/Art/Hero.png", "Assets/Art/Player.png"],
    );
    assert_eq!(mv.code, Some(0), "{mv:?}");
    let files = tracked(&sandbox);
    assert!(
        files.contains("Assets/Art/Player.png\n") && files.contains("Assets/Art/Player.png.meta\n"),
        "{files}"
    );
    assert!(!files.contains("Hero"));
    assert_eq!(
        sandbox.run(GANG, &["commit", "-q", "-m", "rename"]).code,
        Some(0)
    );

    let rm = sandbox.run(GANG, &["rm", "-q", "Assets/Art/Player.png"]);
    assert_eq!(rm.code, Some(0), "{rm:?}");
    let files = tracked(&sandbox);
    assert!(!files.contains("Player"), "{files}");
    assert!(
        !files.contains("Assets/Art.meta"),
        "the emptied folder's .meta goes too: {files}"
    );
    assert_eq!(
        sandbox.run(GANG, &["commit", "-q", "-m", "remove"]).code,
        Some(0)
    );
}

#[test]
fn case_only_rename_works() {
    let sandbox = project();
    let mv = sandbox.run(GANG, &["mv", "Assets/Art/Hero.png", "Assets/Art/hero.png"]);
    assert_eq!(mv.code, Some(0), "{mv:?}");
    let files = tracked(&sandbox);
    assert!(
        files.contains("Assets/Art/hero.png\n") && files.contains("Assets/Art/hero.png.meta\n"),
        "{files}"
    );
}

#[test]
fn restore_brings_back_the_meta() {
    let sandbox = project();
    fs::remove_file(sandbox.repo.join("Assets/Art/Hero.png")).unwrap();
    fs::remove_file(sandbox.repo.join("Assets/Art/Hero.png.meta")).unwrap();
    let restore = sandbox.run(GANG, &["restore", "Assets/Art/Hero.png"]);
    assert_eq!(restore.code, Some(0), "{restore:?}");
    assert!(sandbox.repo.join("Assets/Art/Hero.png.meta").exists());
}

#[test]
fn reset_hard_can_be_undone() {
    let sandbox = project();
    sandbox.write("Assets/Art/Hero.png", "edited");
    assert_eq!(sandbox.run(GANG, &["reset", "--hard", "-q"]).code, Some(0));
    assert_eq!(sandbox.read("Assets/Art/Hero.png"), "png");
    let undo = sandbox.run(GANG, &["undo"]);
    assert_eq!(undo.code, Some(0), "{undo:?}");
    assert_eq!(sandbox.read("Assets/Art/Hero.png"), "edited");
}

#[test]
fn clean_keeps_library() {
    let sandbox = project();
    sandbox.write("Library/ArtifactDB", "hours of importing");
    sandbox.write("Temp/scratch", "x");
    assert_eq!(sandbox.run(GANG, &["clean", "-fdxq"]).code, Some(0));
    assert!(sandbox.repo.join("Library/ArtifactDB").exists());
    assert!(!sandbox.repo.join("Temp/scratch").exists());
    assert_eq!(
        sandbox
            .run(GANG, &["clean", "-fdxq", "--include-library"])
            .code,
        Some(0)
    );
    assert!(!sandbox.repo.join("Library").exists());
}

#[test]
fn save_and_undo() {
    let sandbox = project();
    sandbox.write("Assets/Art/Hero.png", "v2");
    let save = sandbox.run(GANG, &["save", "-m", "Hero v2"]);
    assert_eq!(save.code, Some(0), "{save:?}");
    assert!(save.stdout.contains("Saved"), "{save:?}");
    assert_eq!(
        sandbox.run("git", &["log", "-1", "--format=%s"]).stdout,
        "Hero v2\n"
    );

    assert_eq!(
        sandbox.run(GANG, &["save", "-m", "nothing"]).stdout,
        "Nothing to save.\n"
    );

    assert_eq!(sandbox.run(GANG, &["undo"]).code, Some(0));
    assert_eq!(
        sandbox.run("git", &["log", "-1", "--format=%s"]).stdout,
        "start\n"
    );
    assert_eq!(
        sandbox
            .run("git", &["diff", "--cached", "--name-only"])
            .stdout,
        "Assets/Art/Hero.png\n"
    );
}

#[test]
fn unity_open_blocks_switching_branches() {
    let sandbox = project();
    sandbox.git(&["branch", "other"]);
    fs::create_dir_all(sandbox.repo.join("Temp")).unwrap();
    let file = fs::File::create(sandbox.repo.join("Temp/UnityLockfile")).unwrap();
    let mut lock = fd_lock::RwLock::new(file);
    let _held = lock.write().unwrap();

    let switch = sandbox.run(GANG, &["switch", "other"]);
    assert_eq!(switch.code, Some(1));
    assert!(switch.stderr.contains("Unity is open"), "{switch:?}");
    assert_eq!(
        sandbox.run(GANG, &["switch", "-q", "-c", "fresh"]).code,
        Some(0)
    );
    assert_eq!(
        sandbox
            .run(GANG, &["--ignore-editor", "switch", "-q", "other"])
            .code,
        Some(0)
    );
}

/// Field test C4 and C6: renaming a scene into a folder that does not exist
/// yet. GANG creates the folder with its .meta, moves the scene's bake
/// folder with it, points Build Settings at the new path, and the commit
/// needs no bake lock: nothing in the bake data changed.
#[test]
fn renaming_a_scene_takes_its_folder_and_build_settings_along() {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    let files = [
        ("Assets/Scenes/E1M1.unity", "%YAML 1.1\n"),
        (
            "Assets/Scenes/E1M1.unity.meta",
            "fileFormatVersion: 2\nguid: 44444444444444444444444444444444\n",
        ),
        (
            "Assets/Scenes/E1M1.meta",
            "fileFormatVersion: 2\nguid: 22222222222222222222222222222222\nfolderAsset: yes\n",
        ),
        ("Assets/Scenes/E1M1/LightingData.asset", "baked"),
        (
            "Assets/Scenes/E1M1/LightingData.asset.meta",
            "fileFormatVersion: 2\nguid: 33333333333333333333333333333333\n",
        ),
        (
            "ProjectSettings/EditorBuildSettings.asset",
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1045 &1\nEditorBuildSettings:\n  m_Scenes:\n  - enabled: 1\n    path: Assets/Scenes/E1M1.unity\n    guid: 44444444444444444444444444444444\n",
        ),
    ];
    for (path, text) in files {
        sandbox.write(path, text);
    }
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    sandbox.git(&["add", "-A"]);
    // The starting point, bake data included (the default lighting policy
    // asks for a bake lock to commit bake outputs).
    sandbox.git(&["commit", "-q", "--no-verify", "-m", "start"]);

    let mv = sandbox.run(
        GANG,
        &[
            "mv",
            "Assets/Scenes/E1M1.unity",
            "Assets/Levels/Level01.unity",
        ],
    );
    assert_eq!(mv.code, Some(0), "{mv:?}");
    assert!(
        mv.stdout.contains("created Assets/Levels (with its .meta)"),
        "{mv:?}"
    );
    let files = tracked_after_add(&sandbox);
    for expected in [
        "Assets/Levels.meta",
        "Assets/Levels/Level01.unity",
        "Assets/Levels/Level01.unity.meta",
        "Assets/Levels/Level01.meta",
        "Assets/Levels/Level01/LightingData.asset",
        "Assets/Levels/Level01/LightingData.asset.meta",
    ] {
        assert!(files.contains(expected), "{expected}\n{files}");
    }
    assert!(!files.contains("Assets/Scenes/E1M1"), "{files}");
    let meta = sandbox.read("Assets/Levels.meta");
    assert!(meta.contains("folderAsset: yes"), "{meta}");
    assert!(
        sandbox
            .read("ProjectSettings/EditorBuildSettings.asset")
            .contains("path: Assets/Levels/Level01.unity\n")
    );
    let commit = sandbox.run(GANG, &["commit", "-q", "-m", "rename the level"]);
    assert_eq!(commit.code, Some(0), "{commit:?}");
}

fn tracked_after_add(sandbox: &Sandbox) -> String {
    sandbox.run("git", &["ls-files"]).stdout
}

/// Thought experiment R33: a conflict finished by hand in an editor, with a
/// marker left behind. Unity cannot open the scene, and a script with one
/// stops compiling for the whole team.
#[test]
fn leftover_conflict_markers_are_blocked() {
    let sandbox = project();
    sandbox.write(
        "Assets/Player.cs",
        "class Player\n{\n<<<<<<< HEAD\n    int speed = 2;\n=======\n    int speed = 3;\n>>>>>>> main\n}\n",
    );
    sandbox.write(
        "Assets/Player.cs.meta",
        "fileFormatVersion: 2\nguid: cccccccccccccccccccccccccccccccc\n",
    );
    sandbox.write("Assets/Notes.md", "<<<<<<< an example in the docs\n");
    sandbox.write(
        "Assets/Notes.md.meta",
        "fileFormatVersion: 2\nguid: dddddddddddddddddddddddddddddddd\n",
    );
    sandbox.git(&["add", "-A"]);
    let commit = sandbox.run("git", &["commit", "-m", "merged by hand"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit.stderr.contains(
            "Assets/Player.cs: still has conflict markers (<<<<<<< … >>>>>>>); the team's scripts would stop compiling"
        ),
        "{commit:?}"
    );
    assert!(!commit.stderr.contains("Notes.md: still"), "{commit:?}");

    sandbox.write(
        "Assets/Player.cs",
        "class Player\n{\n    int speed = 3;\n}\n",
    );
    sandbox.git(&["add", "-A"]);
    let commit = sandbox.run("git", &["commit", "-q", "-m", "merged by hand"]);
    assert_eq!(commit.code, Some(0), "{commit:?}");
}

/// Thought experiment R34: a new material assigned in a scene, the scene
/// committed and the material not. It works here and is Missing for
/// everyone else.
#[test]
fn a_commit_using_an_asset_only_this_machine_has_is_blocked() {
    let sandbox = project();
    let scene = |guids: &[&str]| {
        let mut text = String::from(
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!23 &1\nMeshRenderer:\n  m_Materials:\n",
        );
        for guid in guids {
            text.push_str(&format!("  - {{fileID: 2100000, guid: {guid}, type: 2}}\n"));
        }
        text
    };
    let new_mat = "1234567890abcdef1234567890abcdef";
    let package_mat = "fedcba0987654321fedcba0987654321";
    sandbox.write(
        "Assets/Art/New.mat",
        "%YAML 1.1\n--- !u!21 &2100000\nMaterial:\n  m_Name: New\n",
    );
    sandbox.write(
        "Assets/Art/New.mat.meta",
        &format!("fileFormatVersion: 2\nguid: {new_mat}\n"),
    );
    sandbox.write("Assets/Level.unity", &scene(&[new_mat, package_mat]));
    sandbox.write(
        "Assets/Level.unity.meta",
        "fileFormatVersion: 2\nguid: 99999999999999999999999999999999\n",
    );
    sandbox.git(&["add", "Assets/Level.unity", "Assets/Level.unity.meta"]);
    let commit = sandbox.run("git", &["commit", "-m", "level"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit.stderr.contains(
            "Assets/Level.unity: uses Assets/Art/New.mat, which is not in the commit: teammates would get a Missing reference"
        ),
        "{commit:?}"
    );
    assert!(
        commit
            .stderr
            .contains("Fix: gang add Assets/Art/New.mat Assets/Art/New.mat.meta"),
        "{commit:?}"
    );
    assert!(!commit.stderr.contains(package_mat), "{commit:?}");

    // Ignored: the fix says so.
    fs::create_dir_all(sandbox.repo.join("Assets/Local")).unwrap();
    fs::rename(
        sandbox.repo.join("Assets/Art/New.mat"),
        sandbox.repo.join("Assets/Local/New.mat"),
    )
    .unwrap();
    fs::rename(
        sandbox.repo.join("Assets/Art/New.mat.meta"),
        sandbox.repo.join("Assets/Local/New.mat.meta"),
    )
    .unwrap();
    let ignore = fs::read_to_string(sandbox.repo.join(".gitignore")).unwrap();
    sandbox.write(".gitignore", &format!("{ignore}Assets/Local/\n"));
    let commit = sandbox.run("git", &["commit", "-m", "level"]);
    assert_eq!(commit.code, Some(1), "{commit:?}");
    assert!(
        commit
            .stderr
            .contains("uses Assets/Local/New.mat, which is ignored (.gitignore)"),
        "{commit:?}"
    );
    assert!(
        commit.stderr.contains("gang add -f Assets/Local/New.mat"),
        "{commit:?}"
    );

    // In the commit: fine.
    sandbox.write(".gitignore", &ignore);
    fs::write(
        sandbox.repo.join("Assets/Local.meta"),
        "fileFormatVersion: 2\nguid: 88888888888888888888888888888888\nfolderAsset: yes\n",
    )
    .unwrap();
    sandbox.git(&["add", "-A"]);
    let commit = sandbox.run("git", &["commit", "-q", "-m", "level"]);
    assert_eq!(commit.code, Some(0), "{commit:?}");
}
