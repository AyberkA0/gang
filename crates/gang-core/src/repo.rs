//! The git repository GANG works in.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

pub struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
}

impl Repo {
    /// The repository containing the current directory, honoring git's global
    /// options (`-C`, `--git-dir`, …). `None` outside a repository.
    pub fn discover(git_globals: &[OsString]) -> Result<Option<Repo>> {
        let output = Command::new("git")
            .args(git_globals)
            .args(["rev-parse", "--show-toplevel", "--absolute-git-dir"])
            .stdin(Stdio::null())
            .output()
            .context("could not run git")?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        match (lines.next(), lines.next()) {
            (Some(root), Some(git_dir)) => Ok(Some(Repo {
                root: PathBuf::from(root),
                git_dir: PathBuf::from(git_dir),
            })),
            // A bare repository has no working tree.
            _ => Ok(None),
        }
    }

    /// The repository whose working tree is `dir`.
    pub fn open(dir: &Path) -> Result<Repo> {
        Repo::discover(&[OsString::from("-C"), dir.as_os_str().to_owned()])?
            .with_context(|| format!("{} is not inside a git repository", dir.display()))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Whether a rebase is in progress. Git swaps the sides then: "ours" is
    /// the upstream commit, "theirs" the user's own commit being replayed.
    pub fn rebasing(&self) -> bool {
        self.rebase_dir().is_some()
    }

    /// The commit a rebase in progress replays onto.
    pub fn rebase_onto(&self) -> Option<String> {
        let onto = std::fs::read_to_string(self.rebase_dir()?.join("onto")).ok()?;
        Some(onto.trim().to_owned()).filter(|o| !o.is_empty())
    }

    fn rebase_dir(&self) -> Option<PathBuf> {
        let merge = self.git_dir.join("rebase-merge");
        if merge.is_dir() {
            return Some(merge);
        }
        // `rebase-apply` is also `git am`'s, which does not swap the sides.
        let apply = self.git_dir.join("rebase-apply");
        apply.join("rebasing").exists().then_some(apply)
    }

    /// `.git/gang/`, where GANG keeps its local state.
    pub fn gang_dir(&self) -> PathBuf {
        self.git_dir.join("gang")
    }

    /// A git command running in the repository root.
    pub fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.root).stdin(Stdio::null());
        command
    }

    /// Runs git and returns its raw output, whatever the exit code.
    pub fn output<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        self.command()
            .args(&args)
            .output()
            .with_context(|| format!("could not run git {}", display_args(&args)))
    }

    /// Runs git and returns stdout; fails with git's stderr when git fails.
    pub fn run<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let output = self.output(&args)?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                display_args(&args),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs git with `input` on stdin and returns stdout.
    pub fn run_with_input<I, S>(&self, args: I, input: &[u8]) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        use std::io::Write;
        let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let mut child = self
            .command()
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not run git {}", display_args(&args)))?;
        child.stdin.take().context("no stdin")?.write_all(input)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "git {} failed: {}",
                display_args(&args),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs git; `None` when git exits with an error.
    pub fn try_run<I, S>(&self, args: I) -> Result<Option<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.output(args)?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
    }

    /// A value from the repository's git config.
    pub fn config_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .try_run(["config", "--get", key])?
            .map(|value| value.trim_end_matches('\n').to_owned()))
    }

    /// Sets a value in the repository's own git config (`.git/config`).
    pub fn config_set(&self, key: &str, value: &str) -> Result<()> {
        self.run(["config", "--local", key, value])?;
        Ok(())
    }

    /// Paths of tracked files, relative to the root.
    pub fn tracked_files(&self) -> Result<Vec<String>> {
        Ok(split_nul(&self.run(["ls-files", "-z"])?))
    }

    /// The directory git runs hooks from.
    pub fn hooks_dir(&self) -> Result<PathBuf> {
        let path = self.run(["rev-parse", "--git-path", "hooks"])?;
        Ok(self.root.join(path.trim_end_matches('\n')))
    }

    /// Whether `path` (relative to the root) is tracked.
    pub fn is_tracked(&self, path: &str) -> Result<bool> {
        Ok(self
            .try_run(["ls-files", "--error-unmatch", "--", path])?
            .is_some())
    }
}

/// Splits `-z` output into entries.
pub fn split_nul(text: &str) -> Vec<String> {
    text.split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
