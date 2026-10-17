//! `gang` must behave exactly like `git` for every command it does not change.
//!
//! Each case runs once with git and once with gang, each in its own identical
//! repository, and compares stdout, stderr and the exit code.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const GANG: &str = env!("CARGO_BIN_EXE_gang");

/// A repository with two commits, a tag, a modified file and an untracked file,
/// created the same way every time so commit ids match between sandboxes.
struct Sandbox {
    _dir: TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(home.join(".gitconfig"), "").unwrap();
        let sandbox = Sandbox {
            _dir: dir,
            repo,
            home,
        };

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

    fn write(&self, name: &str, content: &str) {
        fs::write(self.repo.join(name), content).unwrap();
    }

    fn command(&self, program: &str) -> Command {
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
            .env_remove("GANG_DEBUG");
        command
    }

    fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?} failed: {output:?}");
    }

    fn run(&self, program: &str, args: &[&str]) -> Outcome {
        Outcome::new(
            self.command(program).args(args).output().unwrap(),
            &self.repo,
        )
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Outcome {
    fn new(output: Output, repo: &Path) -> Outcome {
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

/// Runs `args` through git and through gang in fresh sandboxes.
fn compare(args: &[&str]) -> Option<String> {
    let expected = Sandbox::new().run("git", args);
    let actual = Sandbox::new().run(GANG, args);
    (expected != actual).then(|| format!("{args:?}\n  git:  {expected:?}\n  gang: {actual:?}"))
}

const CASES: &[&[&str]] = &[
    // Inspecting
    &["status"],
    &["status", "--short"],
    &["status", "--porcelain=v2", "--branch"],
    &["log"],
    &["log", "--oneline"],
    &["log", "-p"],
    &["log", "--raw"],
    &["log", "--format=%H %s", "-1"],
    &["show", "HEAD"],
    &["show", "--stat", "HEAD~1"],
    &["diff"],
    &["diff", "--stat"],
    &["diff", "--cached"],
    &["diff", "HEAD~1", "HEAD"],
    &["blame", "a.txt"],
    &["grep", "hello"],
    &["shortlog", "-sn", "HEAD"],
    &["describe", "--tags"],
    // Plumbing
    &["rev-parse", "HEAD"],
    &["rev-parse", "--abbrev-ref", "HEAD"],
    &["ls-files"],
    &["ls-tree", "HEAD"],
    &["cat-file", "-p", "HEAD"],
    // Changing things
    &["add", "."],
    &["commit", "-q", "-am", "third"],
    &["mv", "a.txt", "renamed.txt"],
    &["rm", "-q", "--cached", "b.txt"],
    &["rm", "b.txt"],
    &["reset", "--soft", "HEAD~1"],
    &["stash"],
    &["branch", "feature"],
    &["branch"],
    &["switch", "-c", "topic"],
    &["checkout", "-b", "other"],
    &["tag", "-l"],
    // Errors
    &["show", "does-not-exist"],
    &["frobnicate"],
    // git's global options
    &["-c", "core.abbrev=12", "log", "--oneline"],
    &["--no-pager", "log", "-1"],
    &["-C", ".", "status", "--short"],
];

#[test]
fn behaves_like_git() {
    let mismatches: Vec<String> = CASES.iter().filter_map(|args| compare(args)).collect();
    assert!(
        mismatches.is_empty(),
        "gang differs from git:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn plain_git_behaves_like_git() {
    let expected = Sandbox::new().run("git", &["status", "--short"]);
    let actual = Sandbox::new().run(GANG, &["git", "status", "--short"]);
    assert_eq!(expected, actual);
}

#[test]
fn gang_options_before_the_command_are_not_passed_to_git() {
    let expected = Sandbox::new().run("git", &["status", "--short"]);
    let actual = Sandbox::new().run(GANG, &["--raw", "status", "--short"]);
    assert_eq!(expected, actual);
}

#[test]
fn version() {
    let outcome = Sandbox::new().run(GANG, &["version"]);
    assert_eq!(outcome.code, Some(0));
    assert!(
        outcome
            .stdout
            .starts_with(&format!("gang     {}\n", env!("CARGO_PKG_VERSION")))
    );
    assert!(outcome.stdout.contains("\ngit      "));

    let outcome = Sandbox::new().run(GANG, &["--version"]);
    assert_eq!(
        outcome.stdout,
        format!("gang {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn version_as_json() {
    let outcome = Sandbox::new().run(GANG, &["version", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap();
    assert_eq!(report["gang"], env!("CARGO_PKG_VERSION"));
    assert!(report["git"].is_string());
}

#[test]
fn help() {
    for args in [&[][..], &["help"], &["--help"], &["-h"]] {
        let outcome = Sandbox::new().run(GANG, args);
        assert_eq!(outcome.code, Some(0), "{args:?}");
        assert!(
            outcome
                .stdout
                .contains("Every other command is passed to git"),
            "{args:?}"
        );
    }
}
