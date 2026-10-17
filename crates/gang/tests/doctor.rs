//! P11: gang doctor and scene-rename warnings.

mod common;

use common::{GANG, Sandbox};

fn messy() -> Sandbox {
    let sandbox = Sandbox::empty();
    sandbox.unity_project("");
    sandbox.write(
        "Assets/Old.png.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000aaa1\n",
    );
    sandbox.write("Assets/NoMeta.txt", "x");
    sandbox.write(".DS_Store", "junk");
    sandbox.write(
        "ProjectSettings/EditorBuildSettings.asset",
        "%YAML 1.1\n--- !u!1045 &1\nEditorBuildSettings:\n  m_Scenes:\n  - enabled: 1\n    path: Assets/Scenes/Gone.unity\n    guid: 0000000000000000000000000000aaa2\n",
    );
    sandbox.write(
        "Assets/Scripts/Loader.cs",
        "class Loader { void Go() { SceneManager.LoadScene(\"Main\"); } }\n",
    );
    sandbox.write(
        "Assets/Scripts/Loader.cs.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000aaa3\n",
    );
    sandbox.write(
        "Assets/Scripts.meta",
        "fileFormatVersion: 2\nguid: 0000000000000000000000000000aaa4\nfolderAsset: yes\n",
    );
    sandbox.git(&["init", "-q", "-b", "main"]);
    sandbox.git(&["add", "-A", "-f"]);
    sandbox.git(&["commit", "-q", "-m", "messy", "--no-verify"]);
    assert_eq!(sandbox.run(GANG, &["init"]).code, Some(0));
    sandbox
}

#[test]
fn doctor_finds_and_fixes_problems() {
    let sandbox = messy();
    let doctor = sandbox.run(GANG, &["doctor"]);
    assert_eq!(doctor.code, Some(1), "{doctor:?}");
    for expected in [
        ".DS_Store is an OS junk file",
        "Assets/Old.png.meta has no asset",
        "Assets/NoMeta.txt has no .meta",
        "Build Settings lists Assets/Scenes/Gone.unity",
    ] {
        assert!(
            doctor.stdout.contains(expected),
            "missing {expected:?} in:\n{}",
            doctor.stdout
        );
    }

    let fix = sandbox.run(GANG, &["doctor", "--fix"]);
    assert!(
        fix.stdout.contains("stopped tracking 1 junk file"),
        "{fix:?}"
    );
    assert!(
        fix.stdout.contains("removed 1 orphan .meta file"),
        "{fix:?}"
    );
    assert!(
        !fix.stdout.contains(".DS_Store is an OS junk file"),
        "{fix:?}"
    );
}

#[test]
fn renaming_a_scene_warns_about_code_that_loads_it() {
    let sandbox = messy();
    let mv = sandbox.run(
        GANG,
        &[
            "mv",
            "Assets/Scenes/Main.unity",
            "Assets/Scenes/Forest.unity",
        ],
    );
    assert_eq!(mv.code, Some(0), "{mv:?}");
    assert!(
        mv.stderr
            .contains("Assets/Scripts/Loader.cs:1 still loads the scene by its old name"),
        "{mv:?}"
    );
}
