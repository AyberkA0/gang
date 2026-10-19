//! P8–P10 on a terminal: the questions GANG asks only when stdin and stdout
//! are terminals, driven through a pseudo terminal.

mod common;
mod pty;

use common::{GANG, Sandbox};
use pty::Pty;

const SCENE: &str = "Assets/Scenes/Level1.unity";

/// A shared bare remote with a GANG Unity project holding `files`, and a
/// clone per user.
struct Team {
    sandbox: Sandbox,
}

impl Team {
    fn new(users: &[&str], files: &[(&str, &str)]) -> Team {
        let sandbox = Sandbox::empty();
        sandbox.unity_project("");
        for (path, text) in files {
            sandbox.write(path, text);
        }
        if files.iter().any(|(path, _)| *path == SCENE) {
            sandbox.write(
                &format!("{SCENE}.meta"),
                "fileFormatVersion: 2\nguid: 11112222333344445555666677778888\n",
            );
        }
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

    /// `gang args` in `user`'s clone, on a terminal.
    fn terminal(&self, user: &str, args: &[&str]) -> Pty {
        let mut command = self.sandbox.command(GANG);
        command
            .current_dir(self.sandbox.dir.path().join(user))
            .args(args);
        Pty::spawn(&command)
    }

    fn write(&self, user: &str, path: &str, text: &str) {
        let full = self.sandbox.dir.path().join(user).join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    fn read(&self, user: &str, path: &str) -> String {
        std::fs::read_to_string(self.sandbox.dir.path().join(user).join(path)).unwrap()
    }

    fn sync(&self, user: &str, message: &str) {
        let sync = self.gang(user, &["sync", "-m", message]);
        assert_eq!(sync.code, Some(0), "{sync:?}");
    }
}

/// A Player and a Main Camera whose scripts have the given values.
fn tuning(values: [&str; 6]) -> String {
    let [jump, speed, gravity, drag, offset, smoothing] = values;
    format!(
        "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!1 &200
GameObject:
  m_Component:
  - component: {{fileID: 201}}
  - component: {{fileID: 202}}
  m_Name: Player
--- !u!4 &201
Transform:
  m_GameObject: {{fileID: 200}}
  m_Children: []
  m_Father: {{fileID: 0}}
--- !u!114 &202
MonoBehaviour:
  m_GameObject: {{fileID: 200}}
  m_Script: {{fileID: 11500000, guid: 0000000000000000000000000000aaaa, type: 3}}
  jumpHeight: {jump}
  speed: {speed}
  gravity: {gravity}
  drag: {drag}
--- !u!1 &300
GameObject:
  m_Component:
  - component: {{fileID: 301}}
  - component: {{fileID: 302}}
  m_Name: Main Camera
--- !u!4 &301
Transform:
  m_GameObject: {{fileID: 300}}
  m_Children: []
  m_Father: {{fileID: 0}}
--- !u!114 &302
MonoBehaviour:
  m_GameObject: {{fileID: 300}}
  m_Script: {{fileID: 11500000, guid: 0000000000000000000000000000cccc, type: 3}}
  offset: {offset}
  smoothing: {smoothing}
"
    )
}

/// GameObjects with a Transform each: `(id, name, parent id or 0, x)`.
/// Transform ids are the object's id + 1; children follow the list's order.
fn hierarchy(rect: bool, objects: &[(u32, &str, u32, &str)]) -> String {
    let (tag, kind) = if rect {
        (224, "RectTransform")
    } else {
        (4, "Transform")
    };
    let mut text = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n".to_owned();
    for &(id, name, parent, x) in objects {
        let children: String = objects
            .iter()
            .filter(|o| o.2 == id)
            .map(|o| format!("\n  - {{fileID: {}}}", o.0 + 1))
            .collect();
        let children = if children.is_empty() {
            " []".to_owned()
        } else {
            children
        };
        let father = if parent == 0 { 0 } else { parent + 1 };
        let transform = id + 1;
        text.push_str(&format!(
            "--- !u!1 &{id}
GameObject:
  m_Component:
  - component: {{fileID: {transform}}}
  m_Name: {name}
--- !u!{tag} &{transform}
{kind}:
  m_GameObject: {{fileID: {id}}}
  m_LocalPosition: {{x: {x}, y: 0, z: 0}}
  m_Children:{children}
  m_Father: {{fileID: {father}}}
"
        ));
    }
    text
}

/// The YAML document `--- !u!<tag> &<id>` of a scene.
fn doc<'a>(text: &'a str, tag: &str, id: u32) -> &'a str {
    let header = format!("--- !u!{tag} &{id}\n");
    let start = text
        .find(&header)
        .unwrap_or_else(|| panic!("no {header:?} in\n{text}"));
    let rest = &text[start + header.len()..];
    &text[start..start + header.len() + rest.find("--- ").unwrap_or(rest.len())]
}

#[track_caller]
fn assert_shows(screen: &str, parts: &[&str]) {
    for part in parts {
        assert!(screen.contains(part), "{part:?} is missing from:\n{screen}");
    }
}

#[test]
fn sync_offers_bang_and_every_key_answers_value_conflicts() {
    let team = Team::new(
        &["alice", "bob"],
        &[(SCENE, &tuning(["2.5", "1", "9.8", "0", "1", "0.5"]))],
    );
    team.write("alice", SCENE, &tuning(["3", "3", "10", "1", "2", "0.6"]));
    team.sync("alice", "alice tuning");
    team.write("bob", SCENE, &tuning(["4", "2", "11", "2", "3", "0.7"]));

    let mut sync = team.terminal("bob", &["sync", "-m", "bob tuning"]);
    sync.expect("6 conflicts found. Resolve them now? [Y/n]");
    sync.send("\r");

    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "Level1.unity · conflict 1/6",
            "Player › MonoBehaviour › Jump Height",
            "base  2.5",
            "you   4",
            "Test  3",
            "Why: You both changed the jump height.",
            "[m] mine",
            "[t] theirs",
            "[b] both (2.5 + 1.5 + 0.5 = 4.5)",
            "[o] original",
            "[e] edit",
            "[?] details",
            "[u] undo",
        ],
    );
    sync.send("b");
    sync.expect("> both (2.5 + 1.5 + 0.5 = 4.5)");

    // Undo goes back to the jump height.
    let screen = sync.expect("[q] quit");
    assert_shows(&screen, &["conflict 2/6", "Player › MonoBehaviour › Speed"]);
    sync.send("u");
    let screen = sync.expect("[q] quit");
    assert_shows(&screen, &["conflict 1/6", "Jump Height"]);
    sync.send("o");
    sync.expect("> original");

    let screen = sync.expect("[q] quit");
    assert_shows(&screen, &["conflict 2/6", "Speed"]);
    sync.send("e");
    sync.expect("New value: ");
    sync.send("7\r");

    let screen = sync.expect("[q] quit");
    assert_shows(&screen, &["conflict 3/6", "Gravity"]);
    sync.send("?");
    let details = sync.expect("Test: 10");
    assert_shows(
        &details,
        &["File: Assets/Scenes/Level1.unity", "Base: 9.8", "Yours: 11"],
    );
    sync.send("m");
    sync.expect("> mine");

    let screen = sync.expect("[q] quit");
    assert_shows(&screen, &["conflict 4/6", "Drag"]);
    sync.send("t");
    sync.expect("> theirs");

    // Every question is asked on its own: there is no key that answers a
    // whole object (or everything) at once.
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &["conflict 5/6", "Main Camera › MonoBehaviour › Offset"],
    );
    assert!(
        !screen.contains("[M]") && !screen.contains("[T]"),
        "{screen}"
    );
    sync.send("T");
    sync.send("t");
    sync.expect("> theirs");
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &["conflict 6/6", "Main Camera › MonoBehaviour › Smoothing"],
    );
    sync.send("t");
    sync.expect("> theirs");

    let summary = sync.expect("Apply them? [Y/n]");
    assert_shows(
        &summary,
        &[
            "Your answers:",
            "Level1.unity · Player › MonoBehaviour › Jump Height: original (2.5)",
            "Level1.unity · Player › MonoBehaviour › Speed: set to 7",
            "Level1.unity · Player › MonoBehaviour › Gravity: mine (11)",
            "Level1.unity · Player › MonoBehaviour › Drag: theirs (1)",
            "Level1.unity · Main Camera › MonoBehaviour › Offset: theirs (2)",
            "Level1.unity · Main Camera › MonoBehaviour › Smoothing: theirs (0.6)",
        ],
    );
    sync.send("\r");
    sync.expect("All conflicts resolved.");
    sync.expect("Commit the merge now? [Y/n]");
    sync.send("\r");
    let (code, screen) = sync.finish();
    assert_eq!(code, 0, "{screen}");
    assert_shows(&screen, &["Pushed 2 commits"]);

    let expected = tuning(["2.5", "7", "11", "1", "2", "0.6"]);
    assert_eq!(team.read("bob", SCENE), expected);
    team.sync("alice", "-");
    assert_eq!(team.read("alice", SCENE), expected);
}

#[test]
fn deleted_objects_can_be_deleted_kept_or_reparented_after_a_break() {
    let base = hierarchy(
        false,
        &[
            (100, "Environment", 0, "0"),
            (200, "Player", 100, "0"),
            (400, "Tree", 100, "0"),
            (500, "Rock", 100, "0"),
            (600, "Lamp", 100, "0"),
        ],
    );
    let team = Team::new(&["alice", "bob"], &[(SCENE, &base)]);
    // Alice deletes three objects and moves the Lamp; Bob adds a child
    // under each of the three and deletes the Lamp.
    team.write(
        "alice",
        SCENE,
        &hierarchy(
            false,
            &[(100, "Environment", 0, "0"), (600, "Lamp", 100, "5")],
        ),
    );
    team.sync("alice", "clean up");
    team.write(
        "bob",
        SCENE,
        &hierarchy(
            false,
            &[
                (100, "Environment", 0, "0"),
                (200, "Player", 100, "0"),
                (700, "Weapon", 200, "0"),
                (400, "Tree", 100, "0"),
                (800, "Leaf", 400, "0"),
                (500, "Rock", 100, "0"),
                (900, "Pebble", 500, "0"),
            ],
        ),
    );

    // Not now.
    let mut sync = team.terminal("bob", &["sync", "-m", "details"]);
    sync.expect("4 conflicts found. Resolve them now? [Y/n]");
    sync.send("n\r");
    sync.expect("Run `gang bang` when you are ready.");
    assert_eq!(sync.finish().0, 1);

    // One answer, then quit.
    let mut bang = team.terminal("bob", &["bang"]);
    let screen = bang.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "Level1.unity · conflict 1/4",
            "Environment › Player",
            "Why: Test deleted Player. You added Weapon under it.",
            "[d] delete (Weapon is deleted too)",
            "[k] keep it, with the other side's changes",
            "[r] keep only Weapon, move it to Environment",
        ],
    );
    bang.send("d");
    bang.expect("> delete (Weapon is deleted too)");
    bang.expect("conflict 2/4");
    bang.expect("[q] quit");
    bang.send("q");
    bang.expect("Your answers are kept. Run `gang bang` to continue.");
    assert_eq!(bang.finish().0, 1);

    // The rest, where it left off.
    let mut bang = team.terminal("bob", &["bang"]);
    let screen = bang.expect("[q] quit");
    assert_shows(&screen, &["conflict 2/4", "Environment › Tree", "Leaf"]);
    assert!(!screen.contains("conflict 1/4"), "{screen}");
    bang.send("k");
    bang.expect("> keep it, with the other side's changes");

    let screen = bang.expect("[q] quit");
    assert_shows(&screen, &["conflict 3/4", "Environment › Rock"]);
    bang.send("r");
    bang.expect("> keep only Pebble, move it to Environment");

    let screen = bang.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "conflict 4/4",
            "Environment › Lamp",
            "Why: You deleted Lamp. Test changed it.",
            "[d] delete",
            "[k] keep it",
        ],
    );
    assert!(!screen.contains("[r]"), "{screen}");
    bang.send("k");

    let summary = bang.expect("Apply them? [Y/n]");
    assert_shows(
        &summary,
        &[
            "Level1.unity · Environment › Player: delete",
            "Level1.unity · Environment › Tree: keep",
            "Level1.unity · Environment › Rock: move to the parent",
            "Level1.unity · Environment › Lamp: keep",
        ],
    );
    bang.send("\r");
    bang.expect("Commit the merge now? [Y/n]");
    bang.send("\r");
    let (code, screen) = bang.finish();
    assert_eq!(code, 0, "{screen}");

    let text = team.read("bob", SCENE);
    for gone in ["m_Name: Player", "m_Name: Weapon", "m_Name: Rock"] {
        assert!(!text.contains(gone), "{gone}:\n{text}");
    }
    assert!(doc(&text, "4", 401).contains("- {fileID: 801}"), "{text}");
    assert!(doc(&text, "4", 801).contains("m_Father: {fileID: 401}"));
    assert!(doc(&text, "4", 901).contains("m_Father: {fileID: 101}"));
    assert!(doc(&text, "4", 601).contains("{x: 5, y: 0, z: 0}"));
    let environment = doc(&text, "4", 101);
    for child in [401, 601, 901] {
        assert!(
            environment.contains(&format!("- {{fileID: {child}}}")),
            "{child}: {environment}"
        );
    }
    assert!(!environment.contains("{fileID: 201}"), "{environment}");
    assert_eq!(team.git("bob", &["status", "--porcelain"]).stdout, "");
}

#[test]
fn moves_orders_packages_code_and_binaries_are_answered_on_one_screen() {
    let ui = |order: [(u32, &'static str); 3], holder: u32| {
        let mut objects = vec![(100, "Canvas", 0, "0")];
        objects.extend(order.iter().map(|&(id, name)| (id, name, 100, "0")));
        objects.extend([
            (600, "Holder1", 100, "0"),
            (700, "Holder2", 100, "0"),
            (500, "Box", holder, "0"),
        ]);
        hierarchy(true, &objects)
    };
    let manifest = |version: &str| {
        format!("{{\n  \"dependencies\": {{\n    \"com.unity.ugui\": \"{version}\"\n  }}\n}}\n")
    };
    let script = |speed: &str| format!("class Player\n{{\n    int speed = {speed};\n}}\n");
    let (a, b, c) = ((200, "A"), (300, "B"), (400, "C"));
    let team = Team::new(
        &["alice", "bob"],
        &[
            (SCENE, &ui([a, b, c], 100)),
            (".gitattributes", "*.dat binary\n"),
            ("Packages/manifest.json", &manifest("2.0.0")),
            ("Assets/Scripts/Player.cs", &script("1")),
            (
                "Assets/Scripts/Player.cs.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000aaaa\n",
            ),
            (
                "Assets/Scripts.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000bbbb\nfolderAsset: yes\n",
            ),
            ("Assets/Data/level.dat", "base\0data"),
            (
                "Assets/Data/level.dat.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\n",
            ),
            (
                "Assets/Data.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\nfolderAsset: yes\n",
            ),
        ],
    );
    team.write("alice", SCENE, &ui([c, a, b], 600));
    team.write("alice", "Packages/manifest.json", &manifest("2.1.0"));
    team.write("alice", "Assets/Scripts/Player.cs", &script("2"));
    team.write("alice", "Assets/Data/level.dat", "alice\0data");
    team.sync("alice", "alice");
    team.write("bob", SCENE, &ui([b, c, a], 700));
    team.write("bob", "Packages/manifest.json", &manifest("2.2.0"));
    team.write("bob", "Assets/Scripts/Player.cs", &script("3"));
    team.write("bob", "Assets/Data/level.dat", "bob\0data");

    let mut sync = team.terminal("bob", &["sync", "-m", "bob"]);
    sync.expect("5 conflicts found. Resolve them now? [Y/n]");
    sync.send("y\r");

    // Order: mine or theirs only.
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "Level1.unity · conflict 1/5",
            "Canvas › RectTransform › Children",
            "Why: You both reordered these UI objects; their order is their drawing order.",
        ],
    );
    assert!(
        !screen.contains("[b]") && !screen.contains("[o]"),
        "{screen}"
    );
    sync.send("m");
    sync.expect("> mine");

    // Move: mine or theirs only.
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "conflict 2/5",
            "Box › RectTransform › Father",
            "Why: You both moved it, to different parents.",
        ],
    );
    assert!(
        !screen.contains("[b]") && !screen.contains("[o]"),
        "{screen}"
    );
    sync.send("t");
    sync.expect("> theirs");

    // A package version.
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "manifest.json · conflict 3/5",
            "dependencies › com.unity.ugui",
            "2.0.0",
            "2.1.0",
            "2.2.0",
            "[o] original",
        ],
    );
    sync.send("o");
    sync.expect("> original");

    // A binary file: save theirs to compare, then take theirs.
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "level.dat · conflict 4/5",
            "Assets/Data/level.dat cannot be merged",
            "[a] save theirs next to it to compare",
        ],
    );
    sync.send("a");
    sync.expect("Saved theirs as Assets/Data/level.dat.theirs");
    assert_eq!(
        team.read("bob", "Assets/Data/level.dat.theirs"),
        "alice\0data"
    );
    sync.send("t");

    // A code hunk: both, mine first.
    let screen = sync.expect("[q] quit");
    assert_shows(
        &screen,
        &[
            "Player.cs · conflict 5/5 · hunk 1",
            "int speed = 3;",
            "int speed = 2;",
            "[b] both (mine first)",
            "[B] both (theirs first)",
        ],
    );
    sync.send("b");

    let summary = sync.expect("Apply them? [Y/n]");
    assert_shows(
        &summary,
        &[
            "Level1.unity · Canvas › RectTransform › Children: mine",
            "Box › RectTransform › Father: theirs",
            "manifest.json · dependencies › com.unity.ugui: original",
        ],
    );
    sync.send("\r");
    sync.expect("Commit the merge now? [Y/n]");
    sync.send("\r");
    let (code, screen) = sync.finish();
    assert_eq!(code, 0, "{screen}");
    assert_shows(&screen, &["Pushed 2 commits"]);

    let text = team.read("bob", SCENE);
    let children = |id| {
        doc(&text, "224", id)
            .lines()
            .filter(|l| l.starts_with("  - {fileID:"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        children(101),
        ["301", "401", "201", "601", "701"].map(|id| format!("  - {{fileID: {id}}}"))
    );
    assert!(doc(&text, "224", 501).contains("m_Father: {fileID: 601}"));
    assert_eq!(children(601), ["  - {fileID: 501}"]);
    assert!(children(701).is_empty(), "{text}");
    assert_eq!(
        team.read("bob", "Packages/manifest.json"),
        manifest("2.0.0")
    );
    assert_eq!(team.read("bob", "Assets/Data/level.dat"), "alice\0data");
    assert_eq!(
        team.read("bob", "Assets/Scripts/Player.cs"),
        "class Player\n{\n    int speed = 3;\n    int speed = 2;\n}\n"
    );
    assert_eq!(
        team.git("bob", &["status", "--porcelain"]).stdout,
        "?? Assets/Data/level.dat.theirs\n"
    );
}

#[test]
fn push_asks_to_release_locks_on_what_was_pushed() {
    let team = Team::new(
        &["alice"],
        &[
            ("Assets/Art/Notes.txt", "notes\n"),
            (
                "Assets/Art/Notes.txt.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\n",
            ),
            (
                "Assets/Art.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\nfolderAsset: yes\n",
            ),
        ],
    );
    let lock = team.gang(
        "alice",
        &["lock", "Assets/Art/Notes.txt", "-m", "rewriting"],
    );
    assert_eq!(lock.code, Some(0), "{lock:?}");

    // Keep it.
    team.write("alice", "Assets/Art/Notes.txt", "first pass\n");
    let mut sync = team.terminal("alice", &["sync", "-m", "first pass"]);
    sync.expect("You pushed changes to locked targets: Assets/Art/Notes.txt");
    sync.expect("Release your lock? [Y/n]");
    sync.send("n\r");
    let (code, screen) = sync.finish();
    assert_eq!(code, 0, "{screen}");
    assert!(!screen.contains("Released"), "{screen}");
    let locks = team.gang("alice", &["locks"]);
    assert!(locks.stdout.contains("Assets/Art/Notes.txt"), "{locks:?}");

    // Release it.
    team.write("alice", "Assets/Art/Notes.txt", "second pass\n");
    let mut sync = team.terminal("alice", &["sync", "-m", "second pass"]);
    sync.expect("Release your lock? [Y/n]");
    sync.send("\r");
    sync.expect("🔓 Released Assets/Art/Notes.txt");
    assert_eq!(sync.finish().0, 0);
    assert_eq!(team.gang("alice", &["locks"]).stdout, "No locks.\n");

    // Without a terminal nothing is asked and the lock stays.
    assert_eq!(
        team.gang("alice", &["lock", "Assets/Art/Notes.txt"]).code,
        Some(0)
    );
    team.write("alice", "Assets/Art/Notes.txt", "third pass\n");
    let sync = team.gang("alice", &["sync", "-m", "third pass"]);
    assert_eq!(sync.code, Some(0), "{sync:?}");
    assert!(!sync.stdout.contains("Release your lock"), "{sync:?}");
    assert!(
        team.gang("alice", &["locks"])
            .stdout
            .contains("Assets/Art/Notes.txt")
    );
}

#[test]
fn status_on_a_terminal_adds_noise_lock_and_team_notes() {
    let level = |player_x: &str, tree_x: &str| {
        hierarchy(
            false,
            &[
                (100, "Environment", 0, "0"),
                (200, "Player", 100, player_x),
                (400, "Tree", 100, tree_x),
            ],
        )
    };
    let noise = |color: &str| {
        format!(
            "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!104 &2\nRenderSettings:\n  m_IndirectSpecularColor: {color}\n"
        )
    };
    let team = Team::new(
        &["alice", "bob"],
        &[
            (SCENE, &level("0", "0")),
            (
                "Assets/Scenes/Noise.unity",
                &noise("{r: 0, g: 0, b: 0, a: 1}"),
            ),
            (
                "Assets/Scenes/Noise.unity.meta",
                "fileFormatVersion: 2\nguid: 99992222333344445555666677778888\n",
            ),
            ("Assets/Art/Notes.txt", "notes\n"),
            (
                "Assets/Art/Notes.txt.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\n",
            ),
            (
                "Assets/Art.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\nfolderAsset: yes\n",
            ),
        ],
    );
    let lock = team.gang(
        "alice",
        &[
            "lock",
            &format!("{SCENE}::Environment/Player"),
            "-m",
            "player rework",
            "--yes",
        ],
    );
    assert_eq!(lock.code, Some(0), "{lock:?}");
    team.write("alice", SCENE, &level("0", "3"));
    team.write("alice", "Assets/Art/Notes.txt", "alice\n");
    assert_eq!(team.gang("alice", &["presence-publish"]).code, Some(0));

    team.write("bob", SCENE, &level("4", "0"));
    team.write("bob", "Assets/Art/Notes.txt", "bob\n");
    team.write(
        "bob",
        "Assets/Scenes/Noise.unity",
        &noise("{r: 0.4, g: 0.4, b: 0.5, a: 1}"),
    );
    team.gang("bob", &["locks"]);

    // Without a terminal, plain git status.
    let plain = team.gang("bob", &["status"]);
    assert_eq!(plain.code, Some(0), "{plain:?}");
    for section in ["Unity:", "Locks:", "Team:"] {
        assert!(!plain.stdout.contains(section), "{plain:?}");
    }

    let (code, screen) = team.terminal("bob", &["status"]).finish();
    assert_eq!(code, 0, "{screen}");
    assert_shows(
        &screen,
        &[
            "modified:   Assets/Art/Notes.txt",
            "Unity:\n  Assets/Scenes/Noise.unity: only noise\n  Run `gang restore --noise` to drop noise-only changes.",
            "Locks:\n  ⚠ Level1.unity › Environment › Player is locked by alice (\"player rework\"",
            "  Undo only that object: gang restore Assets/Scenes/Level1.unity::Environment/Player",
            "Team:",
            "⚠ alice is also changing Assets/Art/Notes.txt (modified",
            "⚠ alice is also changing Assets/Scenes/Level1.unity (modified",
            "It merges automatically unless you both change the same property.",
        ],
    );

    // Following both hints leaves only the team notes.
    let restore = team.gang("bob", &["restore", &format!("{SCENE}::Environment/Player")]);
    assert_eq!(restore.code, Some(0), "{restore:?}");
    assert_eq!(team.gang("bob", &["restore", "--noise"]).code, Some(0));
    let (code, screen) = team.terminal("bob", &["status"]).finish();
    assert_eq!(code, 0, "{screen}");
    assert!(
        !screen.contains("Unity:") && !screen.contains("Locks:"),
        "{screen}"
    );
    assert_shows(
        &screen,
        &["Team:", "⚠ alice is also changing Assets/Art/Notes.txt"],
    );
}

/// R29 on a terminal: a material deleted with its whole folder while a
/// teammate used it comes back on one answer, with the texture it uses and
/// its folder, in a commit of its own.
#[test]
fn a_deleted_asset_still_in_use_can_be_brought_back() {
    let floor = "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!1 &100
GameObject:
  m_Name: Floor
";
    let red = "%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!21 &2100000
Material:
  m_Name: Red
  m_SavedProperties:
    m_TexEnvs:
    - _MainTex:
        m_Texture: {fileID: 2800000, guid: 0000000000000000000000000000dddd, type: 3}
";
    let team = Team::new(
        &["alice", "bob"],
        &[
            (SCENE, floor),
            (
                "Assets/Materials.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000eeee\nfolderAsset: yes\n",
            ),
            ("Assets/Materials/Red.mat", red),
            (
                "Assets/Materials/Red.mat.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000cccc\n",
            ),
            ("Assets/Materials/Red.png", "not really a png\n"),
            (
                "Assets/Materials/Red.png.meta",
                "fileFormatVersion: 2\nguid: 0000000000000000000000000000dddd\n",
            ),
        ],
    );
    let alice = team.sandbox.dir.path().join("alice");
    std::fs::remove_dir_all(alice.join("Assets/Materials")).unwrap();
    std::fs::remove_file(alice.join("Assets/Materials.meta")).unwrap();
    team.sync("alice", "no more materials");

    team.write(
        "bob",
        SCENE,
        &format!(
            "{floor}--- !u!23 &101
MeshRenderer:
  m_GameObject: {{fileID: 100}}
  m_Materials:
  - {{fileID: 2100000, guid: 0000000000000000000000000000cccc, type: 2}}
"
        ),
    );
    let mut sync = team.terminal("bob", &["sync", "-m", "red floor"]);
    sync.expect("deleted Assets/Materials/Red.mat (");
    sync.expect("Bring Red.mat back?");
    sync.send("\r");
    sync.expect("gang: bring back Red.mat, still in use");
    let (code, screen) = sync.finish();
    assert_eq!(code, 0, "{screen}");
    let bob = team.sandbox.dir.path().join("bob");
    for path in [
        "Assets/Materials.meta",
        "Assets/Materials/Red.mat",
        "Assets/Materials/Red.mat.meta",
        "Assets/Materials/Red.png",
        "Assets/Materials/Red.png.meta",
    ] {
        assert!(bob.join(path).exists(), "{path}\n{screen}");
    }
    let log = team.git("bob", &["log", "-1", "--format=%B"]);
    assert!(
        log.stdout.contains("with Assets/Materials/Red.png\n"),
        "{log:?}"
    );
    assert!(
        log.stdout.contains("used by Assets/Scenes/Level1.unity"),
        "{log:?}"
    );
    assert_eq!(team.git("bob", &["status", "--porcelain"]).stdout, "");
    // Pushed with the rest: alice gets the material back.
    team.sync("alice", "-");
    assert!(alice.join("Assets/Materials/Red.mat").exists());
}
