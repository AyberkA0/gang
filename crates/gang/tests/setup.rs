//! `gang init`, `gang clone`, `gang ignore` and the managed-file merge driver.

mod common;

use std::fs;

use common::{GANG, Sandbox};

fn init_unity_repo() -> Sandbox {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    let outcome = sandbox.run(GANG, &["init"]);
    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    sandbox
}

#[test]
fn init_sets_up_a_unity_repository() {
    let sandbox = init_unity_repo();
    let gitignore = sandbox.read(".gitignore");
    assert!(gitignore.starts_with("# >>> gang managed v1"));
    assert!(gitignore.contains("\n/[Ll]ibrary/\n"));
    assert!(gitignore.contains("\n.DS_Store\n"));
    assert!(
        sandbox
            .read(".gitattributes")
            .contains("*.unity merge=gang\n")
    );
    assert!(sandbox.read(".gang/config.toml").contains("[locks]"));

    let driver = sandbox.run("git", &["config", "--get", "merge.gang.driver"]);
    assert!(
        driver.stdout.contains("merge-driver unity %O %A %B %P"),
        "{driver:?}"
    );
    let hook = fs::read_to_string(sandbox.repo.join(".git/hooks/pre-commit")).unwrap();
    assert!(hook.contains("hook pre-commit"));

    let branch = sandbox.run("git", &["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(branch.stdout, "main\n");
}

#[test]
fn init_output_is_the_same_everywhere() {
    let a = init_unity_repo();
    let b = init_unity_repo();
    for file in [".gitignore", ".gitattributes", ".gang/config.toml"] {
        assert_eq!(a.read(file), b.read(file), "{file}");
    }
}

#[test]
fn init_keeps_existing_rules_and_hooks() {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    sandbox.git(&["init", "-q", "-b", "main"]);
    sandbox.write(".gitignore", "Library/\n*.log\n");
    sandbox.write(".git/hooks/pre-commit", "#!/bin/sh\necho mine\n");

    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    let gitignore = sandbox.read(".gitignore");
    assert!(
        gitignore.ends_with("kept sorted\n*.[Ll]og\n"),
        "{gitignore}"
    );
    assert_eq!(
        sandbox.read(".git/hooks/pre-commit.local"),
        "#!/bin/sh\necho mine\n"
    );

    // Running init again changes nothing.
    let before = sandbox.read(".gitignore");
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    assert_eq!(sandbox.read(".gitignore"), before);
}

#[test]
fn init_finds_a_project_in_a_subfolder() {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("game");
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    assert!(sandbox.read(".gitignore").contains("\n/game/[Ll]ibrary/\n"));
}

#[test]
fn ignore_add_and_check() {
    let sandbox = init_unity_repo();
    let outcome = sandbox.run(GANG, &["ignore", "add", "Assets/_Sandbox/"]);
    assert_eq!(outcome.code, Some(0), "{outcome:?}");
    let gitignore = sandbox.read(".gitignore");
    assert!(gitignore.contains("\n/[Aa]ssets/_[Ss]andbox.meta\n/[Aa]ssets/_[Ss]andbox/\n"));

    let check = sandbox.run(
        GANG,
        &[
            "ignore",
            "check",
            "Assets/_Sandbox/a.txt",
            "assets/_sandbox.meta",
            "Assets/Player.cs",
            ".DS_Store",
        ],
    );
    assert_eq!(check.code, Some(0), "{check:?}");
    let lines: Vec<&str> = check.stdout.lines().collect();
    assert!(lines[0].starts_with("Assets/_Sandbox/a.txt: ignored by /[Aa]ssets/_[Ss]andbox/"));
    assert!(lines[1].starts_with("assets/_sandbox.meta: ignored by"));
    assert_eq!(lines[2], "Assets/Player.cs: not ignored");
    assert!(lines[3].starts_with(".DS_Store: ignored by .DS_Store"));
}

#[test]
fn ignore_add_local_stays_out_of_gitignore() {
    let sandbox = init_unity_repo();
    let before = sandbox.read(".gitignore");
    assert_eq!(
        sandbox
            .run(GANG, &["ignore", "add", "--local", "notes/"])
            .code,
        Some(0)
    );
    assert_eq!(sandbox.read(".gitignore"), before);
    assert!(sandbox.read(".git/info/exclude").ends_with("[Nn]otes/\n"));
}

#[test]
fn ignore_doctor_finds_and_fixes_tracked_junk() {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    sandbox.git(&["init", "-q", "-b", "main"]);
    sandbox.write("Library/cache.bin", "x");
    sandbox.write(".DS_Store", "x");
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "oops"]);
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));

    let doctor = sandbox.run(GANG, &["ignore", "doctor"]);
    assert_eq!(doctor.code, Some(1));
    assert!(
        doctor.stderr.contains("2 tracked files match ignore rules"),
        "{doctor:?}"
    );

    assert_eq!(
        sandbox.run(GANG, &["ignore", "doctor", "--fix"]).code,
        Some(0)
    );
    let tracked = sandbox.run("git", &["ls-files"]).stdout;
    assert!(
        !tracked.contains("Library/") && !tracked.contains(".DS_Store"),
        "{tracked}"
    );
    assert!(sandbox.repo.join("Library/cache.bin").exists());
}

#[test]
fn managed_files_merge_without_conflicts() {
    let sandbox = init_unity_repo();
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "gang"]);

    sandbox.git(&["switch", "-q", "-c", "other"]);
    sandbox.run(GANG, &["ignore", "add", "Assets/_Theirs/"]);
    sandbox.git(&["commit", "-q", "-am", "theirs"]);
    sandbox.git(&["switch", "-q", "main"]);
    sandbox.run(GANG, &["ignore", "add", "Assets/_Mine/"]);
    sandbox.git(&["commit", "-q", "-am", "mine"]);

    let merge = sandbox.run(GANG, &["merge", "-q", "--no-edit", "other"]);
    assert_eq!(merge.code, Some(0), "{merge:?}");
    let gitignore = sandbox.read(".gitignore");
    assert!(gitignore.contains("/[Aa]ssets/_[Mm]ine/"));
    assert!(gitignore.contains("/[Aa]ssets/_[Tt]heirs/"));
    assert!(!gitignore.contains("<<<<<<<"));
}

#[test]
fn config_project_get_and_set() {
    let sandbox = init_unity_repo();
    let get = sandbox.run(GANG, &["config", "--project", "locks.auto_ttl"]);
    assert_eq!(get.stdout, "8h\n");
    assert_eq!(
        sandbox
            .run(GANG, &["config", "--project", "locks.auto_ttl", "12h"])
            .code,
        Some(0)
    );
    assert_eq!(
        sandbox
            .run(GANG, &["config", "--project", "locks.auto_ttl"])
            .stdout,
        "12h\n"
    );
    assert_eq!(
        sandbox
            .run(
                GANG,
                &["config", "--project", "locks.default_level", "high"]
            )
            .code,
        Some(1)
    );
    // Without --project, it is git's config.
    let git_config = sandbox.run(GANG, &["config", "--get", "merge.gang.name"]);
    assert_eq!(git_config.code, Some(0));
}

#[test]
fn clone_sets_up_a_gang_repository() {
    let sandbox = init_unity_repo();
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "gang"]);
    let remote = sandbox.dir.path().join("remote.git");
    sandbox.git(&["clone", "-q", "--bare", ".", remote.to_str().unwrap()]);

    let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
    let clone = sandbox.run_in("", GANG, &["clone", "-q", &url, "teammate"]);
    assert_eq!(clone.code, Some(0), "{clone:?}");
    assert!(
        sandbox
            .dir
            .path()
            .join("teammate/.git/hooks/pre-push")
            .exists()
    );
    let driver = sandbox.run_in("teammate", "git", &["config", "--get", "merge.gang.driver"]);
    assert!(driver.stdout.contains("merge-driver unity"));
}

#[test]
fn plain_git_clone_gets_set_up_by_the_next_gang_command() {
    let sandbox = init_unity_repo();
    sandbox.git(&["add", "-A"]);
    sandbox.git(&["commit", "-q", "-m", "gang"]);
    let teammate = sandbox.dir.path().join("teammate");
    sandbox.git(&["clone", "-q", ".", teammate.to_str().unwrap()]);

    assert_eq!(
        sandbox
            .run_in("teammate", GANG, &["status", "--short"])
            .code,
        Some(0)
    );
    let driver = sandbox.run_in("teammate", "git", &["config", "--get", "merge.gang.driver"]);
    assert!(driver.stdout.contains("merge-driver unity"));
}
