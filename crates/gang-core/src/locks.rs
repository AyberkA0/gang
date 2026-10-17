//! Locks (DESIGN.md §10): stored in `locks.json` on the shared ref
//! `refs/gang/locks`, changed with compare-and-swap pushes.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::repo::Repo;
use crate::{clock, presence};

pub const REF: &str = "refs/gang/locks";
const LOCAL_REF: &str = "refs/gang/remote/locks";
const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockKind {
    Manual,
    Auto,
    Bake,
}

/// Something a lock covers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Target {
    File {
        path: String,
    },
    Folder {
        path: String,
    },
    Glob {
        pattern: String,
    },
    /// A GameObject or prefab instance and its subtree.
    Object {
        file: String,
        file_id: i64,
        path: String,
    },
    /// One document, such as a scene's LightmapSettings.
    Document {
        file: String,
        file_id: i64,
        class: String,
    },
}

impl Target {
    /// How the target is shown: `Assets/Art/Hero.psd`, `Level1.unity › Player`.
    pub fn label(&self) -> String {
        match self {
            Target::File { path } | Target::Folder { path } => path.clone(),
            Target::Glob { pattern } => pattern.clone(),
            Target::Object { file, path, .. } => {
                format!("{} › {}", short(file), path.replace('/', " › "))
            }
            Target::Document { file, class, .. } => format!("{} › {class}", short(file)),
        }
    }

    /// Whether the target covers the whole file at `path`.
    pub fn covers_file(&self, path: &str) -> bool {
        let asset = path.strip_suffix(".meta").unwrap_or(path);
        match self {
            Target::File { path: locked } => locked == asset,
            Target::Folder { path: folder } => {
                let folder = folder.trim_end_matches('/');
                asset.starts_with(&format!("{folder}/")) || asset == folder
            }
            Target::Glob { pattern } => glob_match(pattern, asset),
            Target::Object { .. } | Target::Document { .. } => false,
        }
    }

    /// The Unity file an object or document target is in.
    pub fn unity_file(&self) -> Option<&str> {
        match self {
            Target::Object { file, .. } | Target::Document { file, .. } => Some(file),
            _ => None,
        }
    }
}

fn short(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `*` matches within a folder, `**` across folders, `?` one character.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn matches(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=s.len()).any(|i| matches(rest, &s[i..]))
            }
            (Some(b'*'), _) => (0..=s.len())
                .take_while(|i| *i == 0 || s[i - 1] != b'/')
                .any(|i| matches(&p[1..], &s[i..])),
            (Some(b'?'), Some(c)) if *c != b'/' => matches(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => matches(&p[1..], &s[1..]),
            _ => false,
        }
    }
    matches(pattern.as_bytes(), path.as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derived {
    #[serde(flatten)]
    pub target: Target,
    /// Why a level-2 target is included: `CameraFollow.target → Player`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    pub id: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_id: Option<u64>,
    pub since: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub kind: LockKind,
    pub level: u8,
    pub root: Target,
    #[serde(default)]
    pub derived: Vec<Derived>,
}

impl Lock {
    /// The root and every derived target.
    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        std::iter::once(&self.root).chain(self.derived.iter().map(|d| &d.target))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LockFile {
    pub schema: u32,
    pub locks: Vec<Lock>,
}

/// A new lock id.
pub fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{:06x}",
        (nanos ^ ((std::process::id() as u128) << 20)) as u32 & 0xff_ffff
    )
}

/// Fetches the shared lock table into `refs/gang/remote/locks`.
pub fn fetch(repo: &Repo) -> Result<()> {
    if !presence::has_origin(repo)? {
        return Ok(());
    }
    let output = repo.output([
        "fetch",
        "-q",
        "--no-tags",
        "--no-write-fetch-head",
        "origin",
        &format!("+{REF}:{LOCAL_REF}"),
    ])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("couldn't find remote ref") {
            // No locks yet.
            let _ = repo.run(["update-ref", "-d", LOCAL_REF]);
            return Ok(());
        }
        bail!("could not fetch locks: {}", stderr.trim());
    }
    Ok(())
}

/// The lock table as last fetched.
pub fn read(repo: &Repo) -> Result<LockFile> {
    let Some(text) = repo.try_run(["cat-file", "-p", &format!("{LOCAL_REF}:locks.json")])? else {
        return Ok(LockFile {
            schema: SCHEMA,
            locks: Vec::new(),
        });
    };
    serde_json::from_str(&text).context("the shared lock table is unreadable")
}

/// Changes the shared lock table with compare-and-swap: fetches it, applies
/// `change` (which may refuse by returning an error), and pushes only if
/// nobody changed it meanwhile; otherwise starts over.
pub fn update<T>(repo: &Repo, mut change: impl FnMut(&mut LockFile) -> Result<T>) -> Result<T> {
    if !presence::has_origin(repo)? {
        bail!("locks need an `origin` remote");
    }
    let mut delay = Duration::from_millis(200);
    for _ in 0..12 {
        let tip = repo
            .try_run(["ls-remote", "origin", REF])?
            .and_then(|out| out.split_whitespace().next().map(str::to_owned));
        if tip.is_some() {
            fetch(repo)?;
        }
        let mut table = if tip.is_some() {
            read(repo)?
        } else {
            LockFile {
                schema: SCHEMA,
                locks: Vec::new(),
            }
        };
        let result = change(&mut table)?;
        table.schema = SCHEMA;
        let commit = write_commit(repo, &table)?;
        let lease = format!("--force-with-lease={REF}:{}", tip.as_deref().unwrap_or(""));
        let output = repo.output(["push", "-q", &lease, "origin", &format!("{commit}:{REF}")])?;
        if output.status.success() {
            repo.run(["update-ref", LOCAL_REF, &commit])?;
            return Ok(result);
        }
        std::thread::sleep(delay + Duration::from_millis(u64::from(std::process::id() % 97)));
        delay = (delay * 2).min(Duration::from_secs(3));
    }
    bail!("could not update the lock table: too many people changing it at once; try again")
}

fn write_commit(repo: &Repo, table: &LockFile) -> Result<String> {
    let mut json = serde_json::to_string_pretty(table)?;
    json.push('\n');
    let blob = repo.run_with_input(["hash-object", "-w", "--stdin"], json.as_bytes())?;
    let tree = repo.run_with_input(
        ["mktree"],
        format!("100644 blob {}\tlocks.json\n", blob.trim()).as_bytes(),
    )?;
    let output = repo
        .command()
        .env("GIT_AUTHOR_NAME", "GANG")
        .env("GIT_AUTHOR_EMAIL", "gang@localhost")
        .env("GIT_COMMITTER_NAME", "GANG")
        .env("GIT_COMMITTER_EMAIL", "gang@localhost")
        .args(["commit-tree", tree.trim(), "-m", "locks [skip ci]"])
        .output()?;
    anyhow::ensure!(output.status.success(), "could not write the lock commit");
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Auto locks of owners who have been offline longer than the TTL no longer
/// count (DESIGN.md §10.9).
pub fn expired(lock: &Lock, team: &[presence::Presence], ttl: Duration) -> bool {
    if lock.kind != LockKind::Auto {
        return false;
    }
    let now = clock::now();
    match team.iter().find(|p| p.user == lock.owner) {
        Some(owner) => owner.age(now) > ttl.as_secs() as i64,
        None => clock::parse(&lock.since).is_some_and(|since| now - since > ttl.as_secs() as i64),
    }
}

/// Two targets that cannot both be locked by different people.
pub fn overlaps(a: &Target, b: &Target, subtree: &dyn Fn(&str, i64) -> BTreeSet<i64>) -> bool {
    match (a, b) {
        (
            Target::Object {
                file: fa,
                file_id: ia,
                ..
            },
            Target::Object {
                file: fb,
                file_id: ib,
                ..
            },
        )
        | (
            Target::Object {
                file: fa,
                file_id: ia,
                ..
            },
            Target::Document {
                file: fb,
                file_id: ib,
                ..
            },
        )
        | (
            Target::Document {
                file: fa,
                file_id: ia,
                ..
            },
            Target::Object {
                file: fb,
                file_id: ib,
                ..
            },
        ) => {
            fa == fb && (ia == ib || subtree(fa, *ia).contains(ib) || subtree(fb, *ib).contains(ia))
        }
        (
            Target::Document {
                file: fa,
                file_id: ia,
                ..
            },
            Target::Document {
                file: fb,
                file_id: ib,
                ..
            },
        ) => fa == fb && ia == ib,
        (Target::Object { file, .. } | Target::Document { file, .. }, other)
        | (other, Target::Object { file, .. } | Target::Document { file, .. }) => {
            other.covers_file(file)
        }
        (Target::File { path }, other) | (other, Target::File { path }) => other.covers_file(path),
        (Target::Folder { path: a }, Target::Folder { path: b }) => {
            let (a, b) = (a.trim_end_matches('/'), b.trim_end_matches('/'));
            a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
        }
        // Globs against folders or globs: be safe.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("Assets/Audio/*.wav", "Assets/Audio/theme.wav"));
        assert!(!glob_match(
            "Assets/Audio/*.wav",
            "Assets/Audio/Music/theme.wav"
        ));
        assert!(glob_match(
            "Assets/**/*.wav",
            "Assets/Audio/Music/theme.wav"
        ));
        assert!(glob_match("Assets/**/*.wav", "Assets/theme.wav"));
        assert!(glob_match("Assets/Art/Hero?.psd", "Assets/Art/Hero2.psd"));
    }

    #[test]
    fn file_coverage() {
        let file = Target::File {
            path: "Assets/Hero.psd".into(),
        };
        assert!(file.covers_file("Assets/Hero.psd"));
        assert!(file.covers_file("Assets/Hero.psd.meta"));
        let folder = Target::Folder {
            path: "Assets/Art/".into(),
        };
        assert!(folder.covers_file("Assets/Art/Hero.psd"));
        assert!(
            folder.covers_file("Assets/Art.meta"),
            "the folder's own .meta goes with it"
        );
        assert!(!folder.covers_file("Assets/Artwork/x.png"));
    }

    #[test]
    fn overlap() {
        let none = |_: &str, _: i64| BTreeSet::new();
        let scene = "Assets/Scenes/L.unity";
        let object = |id| Target::Object {
            file: scene.into(),
            file_id: id,
            path: String::new(),
        };
        let tree = |_: &str, id: i64| {
            if id == 1 {
                BTreeSet::from([1, 2, 3])
            } else {
                BTreeSet::from([id])
            }
        };
        assert!(overlaps(&object(1), &object(2), &tree));
        assert!(!overlaps(&object(4), &object(5), &tree));
        assert!(overlaps(
            &Target::File { path: scene.into() },
            &object(4),
            &none
        ));
        assert!(overlaps(
            &Target::Folder {
                path: "Assets/Scenes/".into()
            },
            &object(4),
            &none
        ));
        assert!(!overlaps(
            &Target::File {
                path: "Assets/a.png".into()
            },
            &Target::File {
                path: "Assets/b.png".into()
            },
            &none
        ));
    }

    #[test]
    fn serializes_like_the_design() {
        let lock = Lock {
            id: "7f3a9c".into(),
            owner: "ayberk".into(),
            github_id: None,
            since: "2026-10-17T09:30:00Z".into(),
            reason: Some("player rework".into()),
            kind: LockKind::Manual,
            level: 2,
            root: Target::Object {
                file: "Assets/Scenes/Level1.unity".into(),
                file_id: 4821937561,
                path: "Player".into(),
            },
            derived: vec![Derived {
                target: Target::File {
                    path: "Assets/Scripts/Player.cs".into(),
                },
                why: None,
            }],
        };
        let json = serde_json::to_string(&lock).unwrap();
        assert!(json.contains("\"root\":{\"type\":\"object\",\"file\":\"Assets/Scenes/Level1.unity\",\"file_id\":4821937561,\"path\":\"Player\"}"), "{json}");
        assert!(
            json.contains(
                "\"derived\":[{\"type\":\"file\",\"path\":\"Assets/Scripts/Player.cs\"}]"
            ),
            "{json}"
        );
        let back: Lock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, lock);
    }
}
