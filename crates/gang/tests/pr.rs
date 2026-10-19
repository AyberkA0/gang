//! Thought experiment TE27: pull requests merged on GitHub bypass GANG's
//! merge. `gang pr merge` merges them here instead.

mod common;

use common::{GANG, Sandbox};

const SCENE: &str = "Assets/Scenes/Level1.unity";

/// Environment with the given children (id, name, x).
fn scene(children: &[(u32, &str, &str)]) -> String {
    let mut text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Component:\n  - component: {fileID: 101}\n  m_Name: Environment\n--- !u!4 &101\nTransform:\n  m_GameObject: {fileID: 100}\n".to_owned();
    if children.is_empty() {
        text.push_str("  m_Children: []\n");
    } else {
        text.push_str("  m_Children:\n");
        for (id, _, _) in children {
            text.push_str(&format!("  - {{fileID: {}}}\n", id + 1));
        }
    }
    text.push_str("  m_Father: {fileID: 0}\n");
    for (id, name, x) in children {
        text.push_str(&format!(
            "--- !u!1 &{id}\nGameObject:\n  m_Component:\n  - component: {{fileID: {}}}\n  m_Name: {name}\n--- !u!4 &{}\nTransform:\n  m_GameObject: {{fileID: {id}}}\n  m_LocalPosition: {{x: {x}, y: 0, z: 0}}\n  m_Children: []\n  m_Father: {{fileID: 101}}\n",
            id + 1,
            id + 1
        ));
    }
    text
}

struct Team {
    sandbox: Sandbox,
    url: String,
}

impl Team {
    fn new() -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        sandbox.write(SCENE, &scene(&[(200, "Tree", "0")]));
        sandbox.write(
            &format!("{SCENE}.meta"),
            "fileFormatVersion: 2\nguid: 11112222333344445555666677778888\n",
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
        Team { sandbox, url }
    }

    fn gang(&self, user: &str, args: &[&str]) -> common::Outcome {
        self.sandbox.run_in(user, GANG, args)
    }

    fn git(&self, user: &str, args: &[&str]) -> common::Outcome {
        self.sandbox.run_in(user, "git", args)
    }

    fn write(&self, user: &str, path: &str, text: &str) {
        std::fs::write(self.sandbox.dir.path().join(user).join(path), text).unwrap();
    }

    fn read(&self, user: &str, path: &str) -> String {
        std::fs::read_to_string(self.sandbox.dir.path().join(user).join(path)).unwrap()
    }

    /// Bob opens pull request #1 from a branch with `text` as the scene.
    fn open_pull_request(&self, text: &str) {
        self.gang("bob", &["switch", "-q", "-c", "forest"]);
        self.write("bob", SCENE, text);
        assert_eq!(self.gang("bob", &["save", "-m", "forest"]).code, Some(0));
        let push = self.git("bob", &["push", "-q", &self.url, "forest:refs/pull/1/head"]);
        assert_eq!(push.code, Some(0), "{push:?}");
    }
}

#[test]
fn a_pull_request_merges_object_by_object() {
    let team = Team::new();
    team.open_pull_request(&scene(&[(200, "Tree", "0"), (300, "Rock", "5")]));
    // Main moves on meanwhile: a lamp under the same parent. Line by line,
    // both children land in the same place of m_Children.
    team.write(
        "alice",
        SCENE,
        &scene(&[(200, "Tree", "0"), (400, "Lamp", "9")]),
    );
    assert_eq!(team.gang("alice", &["sync", "-m", "lamp"]).code, Some(0));

    let merge = team.gang("alice", &["pr", "merge", "1", "--into", "main"]);
    assert_eq!(merge.code, Some(0), "{merge:?}");
    assert!(
        merge
            .stdout
            .contains("Merged pull request #1 into main and pushed"),
        "{merge:?}"
    );
    let text = team.read("alice", SCENE);
    assert!(!text.contains("<<<<<<<"), "{text}");
    for name in ["m_Name: Tree", "m_Name: Rock", "m_Name: Lamp"] {
        assert!(text.contains(name), "{name}\n{text}");
    }
    // The pull request's commit is on main on GitHub: it counts as merged.
    let pr_head = team.git("bob", &["rev-parse", "forest"]).stdout;
    team.git("alice", &["fetch", "-q"]);
    let contains = team.git(
        "alice",
        &["merge-base", "--is-ancestor", pr_head.trim(), "origin/main"],
    );
    assert_eq!(contains.code, Some(0), "{contains:?}");
    // A merge commit with main first.
    let parents = team
        .git("alice", &["log", "-1", "--format=%P %s", "origin/main"])
        .stdout;
    assert!(
        parents.contains("Merge pull request #1 into main"),
        "{parents}"
    );
    let branch = team
        .git("alice", &["symbolic-ref", "--short", "HEAD"])
        .stdout;
    assert_eq!(branch.trim(), "main");
}

#[test]
fn a_pull_request_with_a_question_waits_and_continues() {
    let team = Team::new();
    team.open_pull_request(&scene(&[(200, "Tree", "3")]));
    team.write("alice", SCENE, &scene(&[(200, "Tree", "7")]));
    assert_eq!(
        team.gang("alice", &["sync", "-m", "tree at 7"]).code,
        Some(0)
    );

    let merge = team.gang("alice", &["pr", "merge", "1", "--into", "main"]);
    assert_eq!(merge.code, Some(1), "{merge:?}");
    assert!(
        merge.stderr.contains("gang pr merge --continue"),
        "{merge:?}"
    );
    let list = team.gang("alice", &["bang", "--list"]);
    assert!(
        list.stdout.contains("Tree › Transform › Position.x"),
        "{list:?}"
    );
    // "mine" is the pull request's side here; take main's value.
    let bang = team.gang("alice", &["bang", "--answer", "theirs"]);
    assert_eq!(bang.code, Some(0), "{bang:?}");
    let resume = team.gang("alice", &["pr", "merge", "--continue"]);
    assert_eq!(resume.code, Some(0), "{resume:?}");
    assert!(team.read("alice", SCENE).contains("{x: 7, y: 0, z: 0}"));
    team.git("alice", &["fetch", "-q"]);
    let pr_head = team.git("bob", &["rev-parse", "forest"]).stdout;
    let contains = team.git(
        "alice",
        &["merge-base", "--is-ancestor", pr_head.trim(), "origin/main"],
    );
    assert_eq!(contains.code, Some(0), "{contains:?}");
}
