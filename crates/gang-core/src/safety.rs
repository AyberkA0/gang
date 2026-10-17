//! Backups before destructive commands, and the operation log behind
//! `gang undo` (DESIGN.md §18.2).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::repo::Repo;

const BACKUP_PREFIX: &str = "refs/gang/backup/";
const BACKUP_DAYS: u64 = 14;

/// One undoable operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    /// Milliseconds since the Unix epoch; also the operation's id.
    pub id: u64,
    /// `save`, `reset`, `clean`, `restore`, …
    pub op: String,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    /// A backup of the working tree taken before the operation.
    pub backup: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Entry {
    Op(Op),
    Undo { undo: u64 },
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The commit `HEAD` points at; `None` in a repository without commits.
pub fn head(repo: &Repo) -> Result<Option<String>> {
    Ok(repo
        .try_run(["rev-parse", "-q", "--verify", "HEAD^{commit}"])?
        .map(|s| s.trim().to_owned()))
}

/// Saves the whole working tree (tracked and untracked, not ignored files) as
/// a commit under `refs/gang/backup/<time>` without touching the index.
/// Returns the backup commit.
pub fn snapshot(repo: &Repo, label: &str) -> Result<String> {
    let gang_dir = repo.gang_dir();
    fs::create_dir_all(&gang_dir)?;
    let index = gang_dir.join("backup-index");
    let _ = fs::remove_file(&index);
    let head = head(repo)?;

    let git = |args: &[&str]| -> Result<String> {
        let output = repo
            .command()
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_AUTHOR_NAME", "GANG")
            .env("GIT_AUTHOR_EMAIL", "gang@localhost")
            .env("GIT_COMMITTER_NAME", "GANG")
            .env("GIT_COMMITTER_EMAIL", "gang@localhost")
            .args(args)
            .output()?;
        if !output.status.success() {
            bail!(
                "backup failed (git {}): {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };

    match &head {
        Some(head) => git(&["read-tree", head])?,
        None => git(&["read-tree", "--empty"])?,
    };
    git(&["add", "-A"])?;
    let tree = git(&["write-tree"])?;
    let message = format!("gang backup before {label}");
    let commit = match &head {
        Some(head) => git(&["commit-tree", &tree, "-p", head, "-m", &message])?,
        None => git(&["commit-tree", &tree, "-m", &message])?,
    };
    let _ = fs::remove_file(&index);

    let millis = now_millis();
    repo.run(["update-ref", &format!("{BACKUP_PREFIX}{millis}"), &commit])?;
    prune_backups(repo, millis)?;
    Ok(commit)
}

fn prune_backups(repo: &Repo, now: u64) -> Result<()> {
    let cutoff = now.saturating_sub(BACKUP_DAYS * 24 * 3600 * 1000);
    let refs = repo.run(["for-each-ref", "--format=%(refname)", BACKUP_PREFIX])?;
    for name in refs.lines() {
        let stamp: u64 = name
            .strip_prefix(BACKUP_PREFIX)
            .and_then(|s| s.parse().ok())
            .unwrap_or(u64::MAX);
        if stamp < cutoff {
            repo.run(["update-ref", "-d", name])?;
        }
    }
    Ok(())
}

fn log_path(repo: &Repo) -> std::path::PathBuf {
    repo.gang_dir().join("oplog.jsonl")
}

fn append(repo: &Repo, entry: &Entry) -> Result<()> {
    fs::create_dir_all(repo.gang_dir())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(repo))?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

/// Starts recording an operation: `HEAD` now, and optionally a backup.
pub fn begin(repo: &Repo, op: &str, backup: bool) -> Result<Op> {
    Ok(Op {
        id: now_millis(),
        op: op.to_owned(),
        head_before: head(repo)?,
        head_after: None,
        backup: if backup {
            Some(snapshot(repo, op)?)
        } else {
            None
        },
    })
}

/// Records a finished operation in the log.
pub fn finish(repo: &Repo, mut op: Op) -> Result<()> {
    op.head_after = head(repo)?;
    append(repo, &Entry::Op(op))
}

/// The most recent operation that has not been undone.
pub fn last(repo: &Repo) -> Result<Option<Op>> {
    let text = match fs::read_to_string(log_path(repo)) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut ops = Vec::new();
    let mut undone = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(Entry::Op(op)) => ops.push(op),
            Ok(Entry::Undo { undo }) => undone.push(undo),
            Err(_) => continue,
        }
    }
    Ok(ops.into_iter().rev().find(|op| !undone.contains(&op.id)))
}

/// Undoes `op` and returns a description of what was done.
pub fn undo(repo: &Repo, op: &Op) -> Result<String> {
    let current = head(repo)?;
    let message = match op.op.as_str() {
        "save" => {
            if current != op.head_after {
                bail!("new commits were made after that save; undo them first");
            }
            let pushed = repo
                .run(["branch", "-r", "--contains", "HEAD"])?
                .lines()
                .any(|l| !l.trim().is_empty());
            if pushed {
                bail!("that commit is already pushed; use `gang revert HEAD` instead");
            }
            let before = op
                .head_before
                .as_deref()
                .context("cannot undo the first commit of a repository")?;
            repo.run(["reset", "-q", "--soft", before])?;
            "the commit is undone; its changes are staged again".to_owned()
        }
        _ => {
            let backup = op
                .backup
                .as_deref()
                .context("this operation has no backup to restore")?;
            if let Some(before) = &op.head_before {
                if current.as_deref() != Some(before.as_str()) {
                    repo.run(["reset", "-q", "--soft", before])?;
                }
            }
            repo.run(["restore", "--source", backup, "--worktree", "--", ":/"])?;
            format!(
                "files restored from before `{}` (changes are unstaged)",
                op.op
            )
        }
    };
    append(repo, &Entry::Undo { undo: op.id })?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        let repo = Repo::open(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn undo_restores_files_lost_by_reset_hard() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        fs::write(dir.path().join("new.txt"), "untracked\n").unwrap();

        let op = begin(&repo, "reset", true).unwrap();
        git(dir.path(), &["reset", "-q", "--hard"]);
        fs::remove_file(dir.path().join("new.txt")).unwrap();
        finish(&repo, op).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );

        let op = last(&repo).unwrap().unwrap();
        undo(&repo, &op).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(last(&repo).unwrap(), None);
    }

    #[test]
    fn undo_save_keeps_the_changes_staged() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        let op = begin(&repo, "save", false).unwrap();
        git(dir.path(), &["commit", "-q", "-am", "second"]);
        finish(&repo, op).unwrap();

        let op = last(&repo).unwrap().unwrap();
        undo(&repo, &op).unwrap();
        let log = repo.run(["log", "--format=%s"]).unwrap();
        assert_eq!(log, "first\n");
        let staged = repo.run(["diff", "--cached", "--name-only"]).unwrap();
        assert_eq!(staged, "a.txt\n");
    }

    #[test]
    fn snapshot_leaves_the_index_alone() {
        let (dir, repo) = repo();
        fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        let commit = snapshot(&repo, "test").unwrap();
        assert_eq!(repo.run(["status", "--porcelain"]).unwrap(), "?? b.txt\n");
        let files = repo.run(["ls-tree", "--name-only", &commit]).unwrap();
        assert_eq!(files, "a.txt\nb.txt\n");
    }
}
