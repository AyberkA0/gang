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
