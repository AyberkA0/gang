//! `gang pr merge <number>` (DESIGN.md §18.4): merges a GitHub pull request
//! with GANG's semantic merge instead of GitHub's line merge.
//!
//! GitHub's "Merge pull request" button merges Unity files line by line:
//! it refuses when lines clash, and when they do not, it can still write a
//! scene that is valid YAML but wrong (two children appended at the same
//! place, a layer number taken twice). GANG merges the pull request here,
//! the way the pull request's author would sync: on the pull request's
//! commits, with the layer pass, the merge driver and `gang bang`; then it
//! records a merge commit on the base branch and pushes it. GitHub marks
//! the pull request as merged because its commits are now on the base.

use std::ffi::OsString;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gang_core::invocation::Invocation;
use gang_core::repo::Repo;
use gang_core::{git, setup};
use serde::{Deserialize, Serialize};

use crate::ui;

/// GitHub pull requests, merged with GANG
#[derive(Parser)]
#[command(name = "gang pr")]
pub struct PrArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Merge a pull request with GANG's semantic merge and push it
    Merge {
        /// The pull request's number
        number: Option<u64>,
        /// The branch to merge into (default: the pull request's base)
        #[arg(long)]
        into: Option<String>,
        /// Finish a merge that stopped for questions
        #[arg(long = "continue")]
        resume: bool,
        /// Give up a merge that stopped for questions
        #[arg(long)]
        abort: bool,
        /// Push the merge to the pull request's own branch instead of the
        /// base (for a protected base: then press Merge on GitHub, which
        /// keeps GANG's result because the branch is up to date)
        #[arg(long)]
        on_branch: bool,
    },
}

/// What a stopped `gang pr merge` needs to finish (`.git/gang/pr-merge.json`).
#[derive(Debug, Serialize, Deserialize)]
struct State {
    number: u64,
    base: String,
    title: String,
    /// The branch checked out before, to return to.
    previous: Option<String>,
    /// The pull request's own branch on `origin`, when it lives there: where
    /// the merge goes when the base branch is protected.
    #[serde(default)]
    head_branch: Option<String>,
    /// Push the merge to the pull request's branch instead of the base.
    #[serde(default)]
    on_branch: bool,
}

fn state_path(repo: &Repo) -> std::path::PathBuf {
    repo.gang_dir().join("pr-merge.json")
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = PrArgs::parse_from(
        std::iter::once(OsString::from("gang pr")).chain(invocation.args.iter().cloned()),
    );
    git::ensure_supported()?;
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    setup::ensure_installed(&repo)?;
    let Action::Merge {
        number,
        into,
        resume,
        abort,
        on_branch,
    } = args.action;
    if abort {
        return abort_merge(&repo);
    }
    if resume {
        let state: State = serde_json::from_str(
            &std::fs::read_to_string(state_path(&repo))
                .context("no pull request merge is waiting; nothing to continue")?,
        )?;
        return finish(&repo, &state);
    }
    let number = number.context("which pull request? gang pr merge <number>")?;
    start(invocation, &repo, number, into, on_branch)
}

/// A token for GitHub's API: GANG's sign-in, else whatever git's own
/// credential helpers have for github.com (Git Credential Manager, `gh`).
fn token(repo: &Repo) -> Option<String> {
    if let Some((credentials, _)) = gang_core::auth::load() {
        return Some(credentials.token);
    }
    let output = repo
        .command()
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            let _ = child
                .stdin
                .take()?
                .write_all(b"protocol=https\nhost=github.com\n\n");
            child.wait_with_output().ok()
        })?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("password=").map(str::to_owned))
}

/// `owner/name` of the `origin` remote on GitHub.
fn github_slug(repo: &Repo) -> Option<String> {
    let url = repo.try_run(["remote", "get-url", "origin"]).ok()??;
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    Some(
        rest.trim_end_matches('/')
            .trim_end_matches(".git")
            .to_owned(),
    )
}

#[derive(Deserialize)]
struct PullRequest {
    title: String,
    state: String,
    #[serde(default)]
    merged: bool,
    base: Ref,
    head: Ref,
}

#[derive(Deserialize)]
struct Ref {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    #[serde(default)]
    repo: Option<HeadRepo>,
}

#[derive(Deserialize)]
struct HeadRepo {
    full_name: String,
}

fn pull_request(repo: &Repo, number: u64) -> Option<PullRequest> {
    let slug = github_slug(repo)?;
    let token = token(repo)?;
    gang_core::auth::get_json(&token, &format!("/repos/{slug}/pulls/{number}"))
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn start(
    invocation: &Invocation,
    repo: &Repo,
    number: u64,
    into: Option<String>,
    on_branch: bool,
) -> Result<i32> {
    if !repo
        .run(["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty()
    {
        bail!(
            "save or stash your changes first; merging a pull request needs a clean working tree"
        );
    }
    // Merging switches the working tree to the pull request's files: not
    // under an open editor, which would write its own copies back.
    if !invocation.options.ignore_editor
        && gang_core::unity::find_projects(repo.root())
            .iter()
            .any(|p| gang_core::unity::is_editor_open(&p.path(repo.root())))
    {
        bail!(
            "Unity is open; merging a pull request switches the project's files. Save your work, close Unity, and run it again"
        );
    }
    if state_path(repo).exists() {
        bail!(
            "a pull request merge is waiting: answer it (`gang bang`), then `gang pr merge --continue`, or `gang pr merge --abort`"
        );
    }
    let info = pull_request(repo, number);
    if let Some(pr) = &info {
        if pr.merged || pr.state != "open" {
            bail!(
                "pull request #{number} is not open ({})",
                if pr.merged {
                    "merged"
                } else {
                    pr.state.as_str()
                }
            );
        }
    }
    let current = repo
        .try_run(["symbolic-ref", "--short", "-q", "HEAD"])?
        .map(|b| b.trim().to_owned());
    let base = match (into, &info, &current) {
        (Some(base), _, _) => base,
        (None, Some(pr), _) => pr.base.name.clone(),
        (None, None, Some(current)) => {
            ui::warn(format!(
                "could not ask GitHub about #{number}; merging it into {current} (use --into to choose)"
            ));
            current.clone()
        }
        (None, None, None) => {
            bail!("could not ask GitHub about #{number}; say where it goes with --into <branch>")
        }
    };
    let title = info.as_ref().map(|pr| pr.title.clone()).unwrap_or_default();

    // 1. The pull request and its base, as they are on GitHub.
    let pr_ref = format!("refs/gang/pr/{number}");
    let fetch = git::run_inherited([
        "-C",
        &repo.root().to_string_lossy(),
        "fetch",
        "-q",
        "origin",
        &format!("+refs/heads/{base}:refs/remotes/origin/{base}"),
        &format!("+refs/pull/{number}/head:{pr_ref}"),
    ])?;
    if fetch != 0 {
        bail!("could not fetch pull request #{number} and {base} from origin");
    }
    if let Some(pr) = &info {
        let fetched = repo.run(["rev-parse", &pr_ref])?;
        if fetched.trim() != pr.head.sha {
            ui::warn(format!(
                "#{number} on GitHub points at {}, the fetched head at {}; merging what was fetched",
                &pr.head.sha[..7.min(pr.head.sha.len())],
                &fetched.trim()[..7]
            ));
        }
    }
    let upstream = format!("origin/{base}");
    // The local base branch must not hold work GitHub does not have.
    if repo
        .try_run(["rev-parse", "-q", "--verify", &format!("refs/heads/{base}")])?
        .is_some()
        && repo
            .try_run(["merge-base", "--is-ancestor", &base, &upstream])?
            .is_none()
    {
        bail!("your {base} has commits that are not on GitHub; run `gang sync` on {base} first");
    }
    let already = repo
        .try_run(["merge-base", "--is-ancestor", &pr_ref, &upstream])?
        .is_some();
    if already {
        println!("#{number} is already part of {base}.");
        return Ok(0);
    }

    // The pull request's branch, when it is a branch of this repository:
    // GitHub's answer, else the branch on origin whose tip is the head.
    let pr_head = repo.run(["rev-parse", &pr_ref])?.trim().to_owned();
    let head_branch = match &info {
        Some(pr)
            if pr
                .head
                .repo
                .as_ref()
                .zip(github_slug(repo))
                .is_some_and(|(r, slug)| r.full_name.eq_ignore_ascii_case(&slug)) =>
        {
            Some(pr.head.name.clone())
        }
        Some(_) => None,
        None => repo
            .try_run(["ls-remote", "origin", "refs/heads/*"])?
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .find(|(sha, _)| *sha == pr_head)
            .and_then(|(_, name)| name.strip_prefix("refs/heads/").map(str::to_owned)),
    };
    if on_branch && head_branch.is_none() {
        bail!(
            "the pull request's branch is not a branch of this repository, so the merge cannot go there"
        );
    }

    // 2. On the pull request's commits, as its author would sync.
    let state = State {
        number,
        base: base.clone(),
        title,
        previous: current,
        head_branch,
        on_branch,
    };
    std::fs::create_dir_all(repo.gang_dir())?;
    std::fs::write(state_path(repo), serde_json::to_string_pretty(&state)?)?;
    repo.run(["switch", "-q", "--detach", &pr_ref])?;
    println!(
        "Merging {base} into pull request #{number}{}…",
        if state.title.is_empty() {
            String::new()
        } else {
            format!(" ({})", state.title)
        }
    );
    let config = gang_core::config::Config::load(repo.root()).unwrap_or_default();
    if config.layers.auto_remap {
        crate::sync_cmd::pre_merge(repo, &upstream, false, invocation.options.ignore_editor)?;
    }
    let incoming = repo
        .run(["rev-list", "--count", &format!("HEAD..{upstream}")])?
        .trim()
        .parse::<usize>()
        .unwrap_or(0);
    if incoming > 0 {
        let code =
            crate::sync_cmd::merge_from(invocation, repo, &upstream, incoming, base.as_str())?;
        if code != 0 {
            println!();
            ui::hint(
                "Answer the questions with `gang bang`, then run `gang pr merge --continue`. (`gang pr merge --abort` gives up.)",
            );
            return Ok(code);
        }
    }
    finish(repo, &state)
}

/// 3. A merge commit on the base branch, pushed.
fn finish(repo: &Repo, state: &State) -> Result<i32> {
    if !crate::bang::unmerged(repo)?.is_empty() || repo.git_dir().join("MERGE_HEAD").exists() {
        bail!("questions are still open: answer them with `gang bang` first");
    }
    let base = &state.base;
    let upstream = format!("origin/{base}");
    let tree = repo.run(["rev-parse", "HEAD^{tree}"])?;
    let head = repo.run(["rev-parse", "HEAD"])?;
    if state.on_branch {
        return to_branch(repo, state, head.trim(), None);
    }
    let mut message = format!("Merge pull request #{} into {base}", state.number);
    if !state.title.is_empty() {
        message.push_str(&format!("\n\n{}", state.title));
    }
    let commit = repo.run([
        "commit-tree",
        tree.trim(),
        "-p",
        &upstream,
        "-p",
        head.trim(),
        "-m",
        &message,
    ])?;
    let commit = commit.trim().to_owned();
    // The base branch moves to the merge.
    if repo
        .try_run(["rev-parse", "-q", "--verify", &format!("refs/heads/{base}")])?
        .is_some()
    {
        repo.run(["switch", "-q", base])?;
        repo.run(["merge", "-q", "--ff-only", &commit])?;
    } else {
        repo.run(["switch", "-q", "-c", base, &commit])?;
        let _ = repo.run(["branch", "-q", "--set-upstream-to", &upstream]);
    }
    let output = repo.output(["push", "-q", "origin", &format!("{base}:refs/heads/{base}")])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let protected = ["protected branch", "GH006", "protected"]
            .iter()
            .any(|w| stderr.contains(w));
        if protected && state.head_branch.is_some() {
            // The base only takes merges through GitHub: undo our local
            // merge commit and put the merge on the pull request's branch.
            repo.run(["reset", "-q", "--keep", &upstream])?;
            return to_branch(repo, state, head.trim(), Some(&stderr));
        }
        let _ = std::fs::remove_file(state_path(repo));
        let _ = repo.run([
            "update-ref",
            "-d",
            &format!("refs/gang/pr/{}", state.number),
        ]);
        eprint!("{stderr}");
        ui::warn(format!(
            "the merge is committed on your {base} but not pushed; run `gang sync` to push it"
        ));
        return Ok(1);
    }
    let _ = std::fs::remove_file(state_path(repo));
    let _ = repo.run([
        "update-ref",
        "-d",
        &format!("refs/gang/pr/{}", state.number),
    ]);
    ui::done(format!(
        "Merged pull request #{} into {base} and pushed; GitHub marks it as merged",
        state.number
    ));
    if let Some(previous) = &state.previous {
        if previous != base {
            let _ = repo.run(["switch", "-q", previous]);
        }
    }
    Ok(0)
}

/// Puts the merge (the base merged into the pull request, GANG's way) on
/// the pull request's own branch. GitHub's Merge button then merges a
/// branch that already contains the base, which keeps GANG's result.
fn to_branch(repo: &Repo, state: &State, merged: &str, refused: Option<&str>) -> Result<i32> {
    let branch = state
        .head_branch
        .as_deref()
        .context("the pull request's branch is not known")?;
    let output = repo.output([
        "push",
        "-q",
        "origin",
        &format!("{merged}:refs/heads/{branch}"),
    ])?;
    let _ = std::fs::remove_file(state_path(repo));
    let _ = repo.run([
        "update-ref",
        "-d",
        &format!("refs/gang/pr/{}", state.number),
    ]);
    let back = state.previous.as_deref().unwrap_or(&state.base);
    let _ = repo.run(["switch", "-q", back]);
    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        ui::warn(format!("could not push the merge to {branch}"));
        return Ok(1);
    }
    if refused.is_some() {
        ui::warn(format!(
            "{} only takes merges through GitHub (a protected branch).",
            state.base
        ));
    }
    ui::done(format!(
        "Merged {} into pull request #{}'s branch {branch} and pushed",
        state.base, state.number
    ));
    ui::hint(
        "Press Merge on GitHub now: the branch already contains the base, so GitHub keeps GANG's result.",
    );
    Ok(0)
}

fn abort_merge(repo: &Repo) -> Result<i32> {
    let Ok(text) = std::fs::read_to_string(state_path(repo)) else {
        println!("No pull request merge is waiting.");
        return Ok(0);
    };
    let state: State = serde_json::from_str(&text)?;
    if repo.git_dir().join("MERGE_HEAD").exists() {
        repo.run(["merge", "--abort"])?;
    }
    gang_core::store::clear_all(repo)?;
    let back = state.previous.as_deref().unwrap_or(&state.base);
    let _ = repo.run(["switch", "-q", back]);
    let _ = std::fs::remove_file(state_path(repo));
    let _ = repo.run([
        "update-ref",
        "-d",
        &format!("refs/gang/pr/{}", state.number),
    ]);
    ui::done(format!("Gave up merging #{}; back on {back}", state.number));
    Ok(0)
}
