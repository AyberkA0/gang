//! `gang` must behave exactly like `git` for every command it does not change.
//!
//! Each case runs once with git and once with gang, each in its own identical
//! repository, and compares stdout, stderr and the exit code.

mod common;

use common::{GANG, Sandbox};

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
