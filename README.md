# GANG

*Gang Avoids Nasty Git-merges*

A terminal version-control tool for Unity teams, built on top of git and GitHub.

- **Scenes and prefabs merge object by object.** Two people can change the same scene; GANG only asks when both changed the same property, with a quick question instead of conflict markers (`gang bang`), and every conflict is asked on its own, never in bulk. Collision matrices, list additions, define symbols and renamed fields merge without a question; a rebuilt prefab keeps its instances' overrides, and an asset one side deleted while the other started using it is caught after the merge and can be brought back.
- **Layer numbers never silently change meaning.** A new layer is reserved for whoever names it first; a teammate's clashing layer moves before it is shared, together with the objects, masks, collision matrix and the C# code that use it, which GANG finds with the C# compiler that ships with Unity.
- **See who is changing what** before it conflicts (`gang who`, `gang note "…"`, or `gang` for a live panel with the team, locks and notices).
- **Lock** files, or objects inside a scene with the scripts and objects around them (`gang lock`); scenes the team wants edited by one person at a time are locked for you (`[scenes.policy]`).
- **One setup on macOS, Windows and Linux**: shared ignore rules, LF line endings, `.meta` files kept with their assets, OS junk, unportable names, leftover conflict markers and scenes that use assets only your machine has stopped before they are committed.
- **Every git command still works**: `gang status`, `gang log`, `gang rebase -i` … and pull requests are merged with GANG's merge, not GitHub's line merge (`gang pr merge`).
- **Free, with nothing but git and GitHub**: no server, Git LFS off by default, and `gang clone` downloads old versions of big files only when they are needed (partial clone).

## Status

Early, working. All five milestones of the [plan](docs/PLAN.md) are reached: safe git, team awareness, scene merging, locks, and release tooling. The test suite passes on macOS, Windows and Linux, and the Unity YAML parser round-trips two public Unity projects (11,000 files) byte for byte. Not yet used by a team in Unity day to day.

A two-person field test on a real Unity 6 project (101 commits, 19 merges, no broken scene) found bugs; they are fixed, and a second round closed the gaps that thought experiments found (layer reservations, compiler-level layer detection, rebuilt prefabs, assets added twice, pull requests, renamed fields); and a third round put GANG's own features through the same questions (the git side doors around one-by-one answers, stash pops, deleted assets still in use, assembly definitions, protected branches); see [Hardening](docs/HARDENING.md). Binaries stay out of Git LFS by default (`[lfs] enabled = false`), so GANG costs nothing on GitHub.

Still open: registering GANG's GitHub OAuth App (until then, sign in with your existing git credentials), merge fixtures made in a real Unity editor, and a Homebrew tap / Scoop bucket.

## Install

From a release (macOS, Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/AyberkA0/gang/main/install.sh | sh
```

On Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/AyberkA0/gang/main/install.ps1 | iex
```

From source (Rust 1.85 or newer, git 2.34 or newer; Git LFS only if your project turns it on):

```bash
cargo install --path crates/gang
```

## Building and testing

```bash
cargo build --release           # binary in target/release/gang
cargo test --workspace          # unit, merge and multi-user integration tests
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace has five crates: `gang` (the CLI and team panel), `gang-core` (git runner, auth, presence, locks, layer reservations, guards, the embedded C# analyzer), `unity-yaml` (lossless Unity YAML parser), `gang-merge` (semantic diff and merge) and `gang-csharp` (C# text rules for lock levels and layer numbers). With a Unity editor installed, tests also exercise the compiler-level analysis.

## Quick start

```bash
gang auth login                 # once: sign in to GitHub
gang init --github              # in your Unity project: set it up and create the repository
gang save -m "First version"    # commit everything, after GANG's checks
gang sync                       # merge the team's work and push yours
```

Teammates join with `gang clone owner/repo`.

## Documents

- [A day with GANG](docs/WORKFLOW.md) — a team using it
- [Commands](docs/COMMANDS.md)
- [Design](docs/DESIGN.md) — how it works
- [Plan](docs/PLAN.md) — what is done and what is next
- [Hardening](docs/HARDENING.md) — field-test findings, blind spots and the fix plan
