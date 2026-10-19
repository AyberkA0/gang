//! Running the `git` binary.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::process::{Command, ExitStatus, Stdio};

use thiserror::Error;

/// Oldest git release GANG supports.
pub const MIN_GIT: Version = Version {
    major: 2,
    minor: 34,
    patch: 0,
};

/// A `major.minor.patch` version, as printed by git and git-lfs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Reads the first version number in `text`, e.g. from
    /// `git version 2.39.5 (Apple Git-154)`, `git version 2.45.2.windows.1`
    /// or `git-lfs/3.7.0 (GitHub; darwin arm64; go 1.24.4)`.
    pub fn parse(text: &str) -> Option<Version> {
        let start = text.find(|c: char| c.is_ascii_digit())?;
        let number: String = text[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let mut parts = number.split('.').filter(|part| !part.is_empty());
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git was not found on PATH")]
    NotFound,
    #[error("git {found} is too old; GANG needs {} or newer", MIN_GIT)]
    TooOld { found: Version },
    #[error("could not read git's version from {0:?}")]
    UnknownVersion(String),
    #[error("could not run git: {0}")]
    Io(#[from] io::Error),
}

fn spawn_error(err: io::Error) -> GitError {
    if err.kind() == io::ErrorKind::NotFound {
        GitError::NotFound
    } else {
        GitError::Io(err)
    }
}

/// The installed git version.
pub fn git_version() -> Result<Version, GitError> {
    let output = Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(spawn_error)?;
    let text = String::from_utf8_lossy(&output.stdout);
    Version::parse(&text).ok_or_else(|| GitError::UnknownVersion(text.trim().to_owned()))
}

/// The installed git-lfs version, if git-lfs is installed.
pub fn lfs_version() -> Option<Version> {
    let output = Command::new("git")
        .args(["lfs", "version"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Version::parse(&String::from_utf8_lossy(&output.stdout))
}

/// Fails unless a supported git is installed.
pub fn ensure_supported() -> Result<Version, GitError> {
    let found = git_version()?;
    if found < MIN_GIT {
        return Err(GitError::TooOld { found });
    }
    Ok(found)
}

/// Runs git with the terminal attached and returns its exit code.
pub fn run_inherited<I, S>(args: I) -> Result<i32, GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    // Ctrl+C reaches git too. GANG ignores it and waits for git to finish, so
    // pagers and editors git started are never left behind.
    let _ = ctrlc::set_handler(|| {});
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(spawn_error)?;
    Ok(exit_code(status))
}

/// [`run_inherited`] with extra environment variables for git and the
/// merge drivers it starts.
pub fn run_inherited_env<I, S>(args: I, env: &[(&str, &str)]) -> Result<i32, GitError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let _ = ctrlc::set_handler(|| {});
    let status = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .status()
        .map_err(spawn_error)?;
    Ok(exit_code(status))
}

/// The exit code to pass on for a finished process. A process killed by a
/// signal maps to `128 + signal`, as shells report it.
pub fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn parses_git_versions() {
        assert_eq!(Version::parse("git version 2.54.0\n"), Some(v(2, 54, 0)));
        assert_eq!(
            Version::parse("git version 2.39.5 (Apple Git-154)"),
            Some(v(2, 39, 5))
        );
        assert_eq!(
            Version::parse("git version 2.45.2.windows.1"),
            Some(v(2, 45, 2))
        );
        assert_eq!(
            Version::parse("git-lfs/3.7.0 (GitHub; darwin arm64; go 1.24.4)"),
            Some(v(3, 7, 0))
        );
        assert_eq!(Version::parse("git version 3.0"), Some(v(3, 0, 0)));
    }

    #[test]
    fn rejects_text_without_a_version() {
        assert_eq!(Version::parse("git version"), None);
        assert_eq!(Version::parse("version 7"), None);
    }

    #[test]
    fn orders_versions() {
        assert!(v(2, 33, 9) < MIN_GIT);
        assert!(v(2, 34, 0) >= MIN_GIT);
        assert!(v(3, 0, 0) > MIN_GIT);
    }
}
