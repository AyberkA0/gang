//! Team presence: every user publishes which files they are changing
//! (DESIGN.md §9). Each user owns `refs/gang/presence/<login>`, whose tree
//! holds only `.gang/<login>.json`; readers fetch all of them.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Config, parse_duration};
use crate::repo::{Repo, split_nul};
use crate::{clock, unity};

/// Where users publish their presence.
pub const REF_PREFIX: &str = "refs/gang/presence/";
/// Where GANG keeps the copies it fetched.
pub const LOCAL_PREFIX: &str = "refs/gang/remote/presence/";

const SCHEMA: u32 = 1;
const PUSH_ATTEMPTS: u32 = 3;

/// Who this user is on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub login: String,
    pub github_id: Option<u64>,
}

/// The user's GitHub login: `git config gang.user`, else the GitHub no-reply
/// address in `user.email` (`<id>+<login>@users.noreply.github.com`), else
/// the name part of `user.email`.
pub fn identity(repo: &Repo) -> Result<Identity> {
    if let Some(login) = repo.config_get("gang.user")? {
        return Ok(Identity {
            login: sanitize(&login),
            github_id: None,
        });
    }
    // `git var` honors both the config and GIT_AUTHOR_EMAIL.
    let email = repo
        .try_run(["var", "GIT_AUTHOR_IDENT"])?
        .and_then(|ident| {
            let start = ident.find('<')?;
            let end = ident[start..].find('>')? + start;
            Some(ident[start + 1..end].to_owned())
        })
        .filter(|email| !email.is_empty())
        .context("git does not know who you are; set `git config user.email`")?;
    if let Some(local) = email.strip_suffix("@users.noreply.github.com") {
        if let Some((id, login)) = local.split_once('+') {
            return Ok(Identity {
                login: sanitize(login),
                github_id: id.parse().ok(),
            });
        }
        return Ok(Identity {
            login: sanitize(local),
            github_id: None,
        });
    }
    let local = email.split('@').next().unwrap_or(&email);
    Ok(Identity {
        login: sanitize(local),
        github_id: None,
    })
}

/// Keeps characters that are valid in a GitHub login and a ref name.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_owned();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Modified,
    Added,
    Deleted,
    Unpushed,
    Stashed,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Modified => "modified",
            State::Added => "added",
            State::Deleted => "deleted",
            State::Unpushed => "unpushed",
            State::Stashed => "stashed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFlag {
    pub state: State,
    pub since: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub ts: String,
    pub kind: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// One user's published state (`.gang/<login>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub schema: u32,
    pub user: String,
    #[serde(default)]
    pub github_id: Option<u64>,
    pub seq: u64,
    pub ts: String,
    pub machine: String,
    pub os: String,
    #[serde(default)]
    pub unity: Option<String>,
    #[serde(default)]
    pub editor_open: bool,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub files: BTreeMap<String, FileFlag>,
    #[serde(default)]
    pub notices: Vec<Notice>,
}

impl Presence {
    /// Everything except the sequence number and timestamp.
    fn same_state(&self, other: &Presence) -> bool {
        let strip = |p: &Presence| Presence {
            seq: 0,
            ts: String::new(),
            ..p.clone()
        };
        strip(self) == strip(other)
    }

    /// Seconds since the user last published, by our clock.
    pub fn age(&self, now: i64) -> i64 {
        clock::parse(&self.ts)
            .map(|ts| now - ts)
            .unwrap_or(i64::MAX)
    }

    pub fn is_online(&self, now: i64, offline_after: i64) -> bool {
        self.age(now) <= offline_after
    }
}

/// The branch flags are compared with: the upstream, else the remote's
/// default branch.
pub fn upstream(repo: &Repo, config: &Config) -> Result<Option<String>> {
    if let Some(upstream) = repo.try_run([
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        "@{upstream}",
    ])? {
        return Ok(Some(upstream.trim().to_owned()));
    }
    let mut candidates = Vec::new();
    if let Some(branch) = &config.project.default_branch {
        candidates.push(format!("origin/{branch}"));
    }
    if let Some(head) =
        repo.try_run(["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"])?
    {
        candidates.push(head.trim().to_owned());
    }
    candidates.extend(["origin/main".to_owned(), "origin/master".to_owned()]);
    for candidate in candidates {
        if repo
            .try_run([
                "rev-parse",
                "-q",
                "--verify",
                &format!("{candidate}^{{commit}}"),
            ])?
            .is_some()
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// Which files differ from what teammates have, and how.
pub fn compute_flags(repo: &Repo, upstream: Option<&str>) -> Result<BTreeMap<String, State>> {
    let mut flags = BTreeMap::new();

    let status = repo.run(["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
    let mut entries = status.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let kind = entry.chars().next().unwrap_or(' ');
        match kind {
            '?' => {
                flags.insert(entry[2..].to_owned(), State::Added);
            }
            '1' | '2' | 'u' => {
                let fields: Vec<&str> = entry
                    .splitn(
                        if kind == '1' {
                            9
                        } else if kind == '2' {
                            10
                        } else {
                            11
                        },
                        ' ',
                    )
                    .collect();
                let xy = fields.get(1).copied().unwrap_or("..");
                let Some(path) = fields.last() else { continue };
                let state = if xy.contains('D') {
                    State::Deleted
                } else if xy.starts_with('A') || kind == '2' {
                    State::Added
                } else {
                    State::Modified
                };
                flags.insert((*path).to_owned(), state);
                if kind == '2' {
                    if let Some(original) = entries.next() {
                        flags.insert(original.to_owned(), State::Deleted);
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(upstream) = upstream {
        let range = format!("{upstream}...HEAD");
        if let Some(diff) = repo.try_run(["diff", "--name-status", "--no-renames", "-z", &range])? {
            for pair in split_nul(&diff).chunks(2) {
                if let [status, path] = pair {
                    let state = match status.as_str() {
                        "A" => State::Added,
                        "D" => State::Deleted,
                        _ => State::Unpushed,
                    };
                    flags.entry(path.clone()).or_insert(state);
                }
            }
        }
    }

    // A scene that was only opened and saved is not a change (DESIGN.md §12.2).
    let rules = gang_merge::diff::Rules::default();
    let noise: Vec<String> = flags
        .iter()
        .filter(|(path, state)| {
            **state == State::Modified && crate::attributes::is_unity_yaml(path)
        })
        .filter(|(path, _)| crate::semantic::is_noise_change(repo, path, &rules).unwrap_or(false))
        .map(|(path, _)| path.clone())
        .collect();
    for path in noise {
        flags.remove(&path);
    }

    let stashes = repo.run(["stash", "list", "--format=%gd"])?;
    for stash in stashes.lines().take(10) {
        if let Some(files) = repo.try_run(["stash", "show", "--name-only", "-z", stash])? {
            for path in split_nul(&files) {
                flags.entry(path).or_insert(State::Stashed);
            }
        }
    }
    Ok(flags)
}

/// Builds this user's current presence. `previous` keeps `since` times and
/// the sequence number going.
pub fn current(repo: &Repo, config: &Config, previous: Option<&Presence>) -> Result<Presence> {
    let identity = identity(repo)?;
    let upstream = upstream(repo, config)?;
    let flags = compute_flags(repo, upstream.as_deref())?;
    let now = clock::format(clock::now());
    let files = flags
        .into_iter()
        .map(|(path, state)| {
            let since = previous
                .and_then(|p| p.files.get(&path))
                .filter(|f| f.state == state)
                .map(|f| f.since.clone())
                .unwrap_or_else(|| now.clone());
            (path, FileFlag { state, since })
        })
        .collect();

    let projects = unity::find_projects(repo.root());
    let ahead = match &upstream {
        Some(upstream) => repo
            .try_run(["rev-list", "--count", &format!("{upstream}..HEAD")])?
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0),
        None => 0,
    };
    Ok(Presence {
        schema: SCHEMA,
        user: identity.login,
        github_id: identity.github_id,
        seq: previous.map(|p| p.seq + 1).unwrap_or(1),
        ts: now,
        machine: machine_name(),
        os: std::env::consts::OS.to_owned(),
        unity: projects.iter().find_map(|p| p.editor_version.clone()),
        editor_open: projects
            .iter()
            .any(|p| unity::is_editor_open(&p.path(repo.root()))),
        branch: repo
            .try_run(["symbolic-ref", "--short", "-q", "HEAD"])?
            .map(|b| b.trim().to_owned()),
        head: repo
            .try_run(["rev-parse", "--short", "-q", "--verify", "HEAD"])?
            .map(|h| h.trim().to_owned()),
        ahead,
        files,
        notices: previous.map(|p| p.notices.clone()).unwrap_or_default(),
    })
}

fn machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// What GANG remembers between publishes (`.git/gang/presence.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
struct LocalState {
    last: Option<Presence>,
    published_at: i64,
}

fn state_path(repo: &Repo) -> PathBuf {
    repo.gang_dir().join("presence.json")
}

fn load_state(repo: &Repo) -> LocalState {
    fs::read_to_string(state_path(repo))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_state(repo: &Repo, state: &LocalState) -> Result<()> {
    fs::create_dir_all(repo.gang_dir())?;
    fs::write(state_path(repo), serde_json::to_string_pretty(state)?)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    Pushed,
    Unchanged,
    NoRemote,
}

/// Whether the repository has an `origin` to publish to.
pub fn has_origin(repo: &Repo) -> Result<bool> {
    Ok(repo.try_run(["remote", "get-url", "origin"])?.is_some())
}

/// Adds a notice to this user's presence; it goes out with the next publish
/// (DESIGN.md §9.1). Keeps the last 20.
pub fn add_notice(repo: &Repo, notice: Notice) -> Result<()> {
    let mut state = load_state(repo);
    if let Some(last) = &mut state.last {
        last.notices.push(notice);
        let excess = last.notices.len().saturating_sub(20);
        last.notices.drain(..excess);
    } else {
        let config = Config::load(repo.root()).unwrap_or_default();
        let mut presence = current(repo, &config, None)?;
        presence.notices.push(notice);
        state.last = Some(presence);
    }
    state.published_at = 0;
    save_state(repo, &state)
}

/// Publishes this user's presence when it changed, or when the heartbeat is
/// due, or when `force` is set. Only one publish runs at a time per clone.
pub fn publish(repo: &Repo, config: &Config, force: bool) -> Result<Published> {
    if !has_origin(repo)? {
        return Ok(Published::NoRemote);
    }
    fs::create_dir_all(repo.gang_dir())?;
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(repo.gang_dir().join("presence.lock"))?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.write()?;

    let mut state = load_state(repo);
    let presence = current(repo, config, state.last.as_ref())?;
    let heartbeat = parse_duration(&config.presence.heartbeat)?.as_secs() as i64;
    let now = clock::now();
    let unchanged = state
        .last
        .as_ref()
        .is_some_and(|last| last.same_state(&presence));
    if unchanged && !force && now - state.published_at < heartbeat {
        return Ok(Published::Unchanged);
    }

    let commit = write_commit(repo, &presence)?;
    let remote_ref = format!("{REF_PREFIX}{}", presence.user);
    push_with_retries(repo, &commit, &remote_ref)?;
    repo.run([
        "update-ref",
        &format!("{LOCAL_PREFIX}{}", presence.user),
        &commit,
    ])?;

    state.last = Some(presence);
    state.published_at = now;
    save_state(repo, &state)?;
    Ok(Published::Pushed)
}

/// A parentless commit whose tree is `.gang/<login>.json`.
fn write_commit(repo: &Repo, presence: &Presence) -> Result<String> {
    let mut json = serde_json::to_string_pretty(presence)?;
    json.push('\n');
    let blob = repo.run_with_input(["hash-object", "-w", "--stdin"], json.as_bytes())?;
    let inner = repo.run_with_input(
        ["mktree"],
        format!("100644 blob {}\t{}.json\n", blob.trim(), presence.user).as_bytes(),
    )?;
    let root = repo.run_with_input(
        ["mktree"],
        format!("040000 tree {}\t.gang\n", inner.trim()).as_bytes(),
    )?;
    let output = repo
        .command()
        .env("GIT_AUTHOR_NAME", "GANG")
        .env("GIT_AUTHOR_EMAIL", "gang@localhost")
        .env("GIT_COMMITTER_NAME", "GANG")
        .env("GIT_COMMITTER_EMAIL", "gang@localhost")
        .args(["commit-tree", root.trim(), "-m"])
        .arg(format!("presence: {} [skip ci]", presence.user))
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "could not write the presence commit"
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Pushes, retrying on failure: GitHub occasionally fails a push under load
/// (DESIGN.md §6.2).
pub fn push_with_retries(repo: &Repo, commit: &str, remote_ref: &str) -> Result<()> {
    let mut last_error = String::new();
    for attempt in 0..PUSH_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500 * 2u64.pow(attempt)));
        }
        let output = repo.output([
            "push",
            "-q",
            "--force",
            "origin",
            &format!("{commit}:{remote_ref}"),
        ])?;
        if output.status.success() {
            return Ok(());
        }
        last_error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    }
    anyhow::bail!("could not push {remote_ref}: {last_error}")
}

/// Fetches everyone's presence into `refs/gang/remote/presence/`.
pub fn fetch(repo: &Repo) -> Result<()> {
    if !has_origin(repo)? {
        return Ok(());
    }
    repo.run([
        "fetch",
        "-q",
        "--prune",
        "--no-tags",
        "--no-write-fetch-head",
        "origin",
        &format!("+{REF_PREFIX}*:{LOCAL_PREFIX}*"),
    ])?;
    Ok(())
}

/// Checks the remote with `ls-remote` and fetches only when something
/// changed. Returns whether it fetched.
pub fn poll(repo: &Repo) -> Result<bool> {
    if !has_origin(repo)? {
        return Ok(false);
    }
    let remote = repo.run(["ls-remote", "origin", &format!("{REF_PREFIX}*")])?;
    let mut remote_tips: Vec<(String, String)> = remote
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter_map(|(sha, name)| Some((name.strip_prefix(REF_PREFIX)?.to_owned(), sha.to_owned())))
        .collect();
    remote_tips.sort();
    let mut local_tips = local_tips(repo)?;
    local_tips.sort();
    if remote_tips == local_tips {
        return Ok(false);
    }
    fetch(repo)?;
    Ok(true)
}

fn local_tips(repo: &Repo) -> Result<Vec<(String, String)>> {
    let refs = repo.run([
        "for-each-ref",
        "--format=%(refname) %(objectname)",
        LOCAL_PREFIX,
    ])?;
    Ok(refs
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(name, sha)| {
            Some((name.strip_prefix(LOCAL_PREFIX)?.to_owned(), sha.to_owned()))
        })
        .collect())
}

/// Everyone's last fetched presence, including this user's own.
pub fn team(repo: &Repo) -> Result<Vec<Presence>> {
    let mut team = Vec::new();
    for (login, sha) in local_tips(repo)? {
        let Some(text) = repo.try_run(["cat-file", "-p", &format!("{sha}:.gang/{login}.json")])?
        else {
            continue;
        };
        if let Ok(presence) = serde_json::from_str::<Presence>(&text) {
            team.push(presence);
        }
    }
    team.sort_by(|a, b| a.user.cmp(&b.user));
    Ok(team)
}

/// Seconds after which a silent teammate counts as offline.
pub fn offline_after(config: &Config) -> i64 {
    parse_duration(&config.presence.offline_after)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(300)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_logins() {
        assert_eq!(sanitize("AyberkA0"), "AyberkA0");
        assert_eq!(sanitize("ayberk.atalay"), "ayberk-atalay");
        assert_eq!(sanitize("  "), "unknown");
    }

    #[test]
    fn same_state_ignores_seq_and_time() {
        let a = Presence {
            schema: 1,
            user: "a".into(),
            github_id: None,
            seq: 1,
            ts: "2026-01-01T00:00:00Z".into(),
            machine: "m".into(),
            os: "linux".into(),
            unity: None,
            editor_open: false,
            branch: None,
            head: None,
            ahead: 0,
            files: BTreeMap::new(),
            notices: Vec::new(),
        };
        let mut b = a.clone();
        b.seq = 2;
        b.ts = "2026-01-01T00:05:00Z".into();
        assert!(a.same_state(&b));
        b.ahead = 1;
        assert!(!a.same_state(&b));
    }
}
