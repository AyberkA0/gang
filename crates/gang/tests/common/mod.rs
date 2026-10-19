//! Test sandbox: a temporary repository with an isolated git configuration.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

pub const GANG: &str = env!("CARGO_BIN_EXE_gang");

/// A repository with two commits, a tag, a modified file and an untracked file,
/// created the same way every time so commit ids match between sandboxes.
pub struct Sandbox {
    pub dir: TempDir,
    pub repo: PathBuf,
    pub home: PathBuf,
    /// Extra environment for every command run in the sandbox.
    pub env: std::sync::Mutex<Vec<(String, String)>>,
}

impl Sandbox {
    /// A repository with history: see the type's documentation.
    pub fn new() -> Sandbox {
        let sandbox = Sandbox::empty();
        sandbox.git(&["init", "-q", "-b", "main"]);
        sandbox.write("a.txt", "hello\n");
        sandbox.write("b.txt", "world\n");
        sandbox.git(&["add", "."]);
        sandbox.git(&["commit", "-q", "-m", "first"]);
        sandbox.write("a.txt", "hello\nagain\n");
        sandbox.git(&["commit", "-q", "-am", "second"]);
        sandbox.git(&["tag", "v1"]);
        sandbox.write("b.txt", "world!\n");
        sandbox.write("c.txt", "new\n");
        sandbox
    }

    /// An empty `repo` folder (not a git repository) with an isolated home.
    pub fn empty() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(home.join(".gitconfig"), "").unwrap();
        Sandbox {
            dir,
            repo,
            home,
            env: Default::default(),
        }
    }

    pub fn write(&self, name: &str, content: &str) {
        let path = self.repo.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    pub fn read(&self, name: &str) -> String {
        fs::read_to_string(self.repo.join(name)).unwrap()
    }

    /// A minimal Unity project (Force Text, Unity 6) in `dir` ("" for the root).
    pub fn unity_project(&self, dir: &str) {
        let base = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        self.write(&format!("{base}Assets/Scenes/Main.unity"), "%YAML 1.1\n");
        self.write(
            &format!("{base}Assets/Scenes/Main.unity.meta"),
            "fileFormatVersion: 2\nguid: 0123456789abcdef0123456789abcdef\n",
        );
        self.write(
            &format!("{base}Assets/Scenes.meta"),
            "fileFormatVersion: 2\nguid: 11111111111111111111111111111111\nfolderAsset: yes\n",
        );
        self.write(
            &format!("{base}ProjectSettings/ProjectVersion.txt"),
            "m_EditorVersion: 6000.0.23f1\n",
        );
        self.write(
            &format!("{base}ProjectSettings/EditorSettings.asset"),
            "%YAML 1.1\n--- !u!159 &1\nEditorSettings:\n  m_SerializationMode: 2\n",
        );
    }

    /// Runs a program in `dir` (relative to the sandbox root).
    pub fn run_in(&self, dir: &str, program: &str, args: &[&str]) -> Outcome {
        let path = self.dir.path().join(dir);
        let mut command = self.command(program);
        command.current_dir(&path);
        Outcome::new(command.args(args).output().unwrap(), &path)
    }

    pub fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GANG_DEBUG")
            .env("GANG_NO_PRESENCE", "1")
            .env(
                "GANG_CREDENTIALS_FILE",
                self.home.join("gang-credentials.json"),
            );
        for (key, value) in self.env.lock().unwrap().iter() {
            command.env(key, value);
        }
        command
    }

    pub fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?} failed: {output:?}");
    }

    pub fn run(&self, program: &str, args: &[&str]) -> Outcome {
        Outcome::new(
            self.command(program).args(args).output().unwrap(),
            &self.repo,
        )
    }
}

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn new(output: Output, repo: &Path) -> Outcome {
        let normalize = |bytes: &[u8]| {
            let mut text = String::from_utf8_lossy(bytes).into_owned();
            let path = repo.to_string_lossy().into_owned();
            text = text.replace(&path, "<repo>");
            text.replace(&path.replace('\\', "/"), "<repo>")
        };
        Outcome {
            code: output.status.code(),
            stdout: normalize(&output.stdout),
            stderr: normalize(&output.stderr),
        }
    }
}
