//! P5: presence between several clones of one remote.

mod common;

use std::thread;

use common::{GANG, Sandbox};
use serde_json::Value;

/// A shared bare remote with a GANG Unity project, and a clone per user.
struct Team {
    sandbox: Sandbox,
}

impl Team {
    fn new(users: &[&str]) -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
        sandbox.git(&["add", "-A"]);
        sandbox.git(&["commit", "-q", "-m", "start"]);
        let remote = sandbox.dir.path().join("remote.git");
        sandbox.git(&["clone", "-q", "--bare", ".", remote.to_str().unwrap()]);
        let url = format!("file://{}", remote.to_string_lossy().replace('\\', "/"));
        for user in users {
            let clone = sandbox.run_in("", GANG, &["clone", "-q", &url, user]);
            assert_eq!(clone.code, Some(0), "{clone:?}");
            assert_eq!(
                sandbox
                    .run_in(user, "git", &["config", "gang.user", user])
                    .code,
                Some(0)
            );
        }
        Team { sandbox }
    }

    fn gang(&self, user: &str, args: &[&str]) -> common::Outcome {
        self.sandbox.run_in(user, GANG, args)
    }

    fn git(&self, user: &str, args: &[&str]) {
        let outcome = self.sandbox.run_in(user, "git", args);
        assert_eq!(outcome.code, Some(0), "{outcome:?}");
    }

    fn write(&self, user: &str, path: &str, content: &str) {
        let full = self.sandbox.dir.path().join(user).join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    fn publish(&self, user: &str) {
        let outcome = self.gang(user, &["presence-publish"]);
        assert_eq!(outcome.code, Some(0), "{outcome:?}");
    }

    /// What `viewer` sees of `user`'s files, as `path → state`.
    fn seen(&self, viewer: &str, user: &str) -> Vec<(String, String)> {
        let outcome = self.gang(viewer, &["--json", "who"]);
        assert_eq!(outcome.code, Some(0), "{outcome:?}");
        let team: Value = serde_json::from_str(&outcome.stdout).unwrap();
        let person = team
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["user"] == user)
            .unwrap_or_else(|| panic!("{viewer} does not see {user}: {}", outcome.stdout));
        person["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, flag)| (path.clone(), flag["state"].as_str().unwrap().to_owned()))
            .collect()
    }
}

const SCENE: &str = "Assets/Scenes/Main.unity";

#[test]
fn teammates_see_each_others_changes_until_they_are_pushed() {
    let team = Team::new(&["alice", "bob"]);

    team.write("alice", SCENE, "%YAML 1.1\n# changed\n");
    team.publish("alice");
    assert_eq!(
        team.seen("bob", "alice"),
        vec![(SCENE.to_owned(), "modified".to_owned())]
    );

    team.git("alice", &["commit", "-q", "-am", "scene"]);
    team.publish("alice");
    assert_eq!(
        team.seen("bob", "alice"),
        vec![(SCENE.to_owned(), "unpushed".to_owned())]
    );

    team.git("alice", &["push", "-q"]);
    team.publish("alice");
    assert_eq!(team.seen("bob", "alice"), vec![]);
}

#[test]
fn who_with_a_path_lists_everyone_changing_it() {
    let team = Team::new(&["alice", "bob"]);
    team.write("alice", SCENE, "%YAML 1.1\n# alice\n");
    team.write("bob", SCENE, "%YAML 1.1\n# bob\n");
    team.publish("alice");

    let who = team.gang("bob", &["who", SCENE]);
    assert_eq!(who.code, Some(0), "{who:?}");
    assert!(who.stdout.contains("alice"), "{who:?}");
    assert!(who.stdout.contains("bob (you)"), "{who:?}");
}

#[test]
fn unchanged_presence_is_not_pushed_again() {
    let team = Team::new(&["alice"]);
    team.write("alice", SCENE, "%YAML 1.1\n# changed\n");
    assert_eq!(
        team.gang("alice", &["presence-publish"]).stdout,
        "published\n"
    );
    assert_eq!(
        team.gang("alice", &["presence-publish"]).stdout,
        "unchanged\n"
    );
    assert_eq!(
        team.gang("alice", &["presence-publish", "--force"]).stdout,
        "published\n"
    );
}

#[test]
fn concurrent_publishers_never_lose_updates() {
    let users = ["u1", "u2", "u3", "u4"];
    let team = Team::new(&users);
    thread::scope(|scope| {
        for user in users {
            let team = &team;
            scope.spawn(move || {
                for round in 0..3 {
                    team.write(user, &format!("Assets/{user}-{round}.txt"), "x");
                    team.publish(user);
                }
            });
        }
    });
    for user in users {
        let files = team.seen("u1", user);
        assert_eq!(files.len(), 3, "{user}: {files:?}");
    }
    // Each user's ref is a single commit without parents.
    let remote = team.sandbox.dir.path().join("remote.git");
    for user in users {
        let parents = team.sandbox.run_in(
            "",
            "git",
            &[
                "--git-dir",
                remote.to_str().unwrap(),
                "rev-list",
                "--count",
                &format!("refs/gang/presence/{user}"),
            ],
        );
        assert_eq!(parents.stdout, "1\n", "{user}");
    }
}

#[test]
fn commands_publish_in_the_background() {
    let team = Team::new(&["alice"]);
    team.write("alice", SCENE, "%YAML 1.1\n# changed\n");
    // `add` changes state, so it starts `gang presence-publish` detached.
    let add = team
        .sandbox
        .command(GANG)
        .current_dir(team.sandbox.dir.path().join("alice"))
        .env_remove("GANG_NO_PRESENCE")
        .args(["add", SCENE])
        .output()
        .unwrap();
    assert!(add.status.success(), "{add:?}");

    let remote = team.sandbox.dir.path().join("remote.git");
    let published = (0..150).any(|_| {
        let refs = team.sandbox.run_in(
            "",
            "git",
            &[
                "--git-dir",
                remote.to_str().unwrap(),
                "for-each-ref",
                "refs/gang/presence/alice",
            ],
        );
        if refs.stdout.is_empty() {
            thread::sleep(std::time::Duration::from_millis(200));
            false
        } else {
            true
        }
    });
    assert!(published, "the background publisher never pushed");
    assert_eq!(
        team.seen("alice", "alice"),
        vec![(SCENE.to_owned(), "modified".to_owned())]
    );
}
