# GANG

*Gang Avoids Nasty Git-merges*

A terminal version-control tool for Unity teams, built on top of git and GitHub.

- **Scenes and prefabs merge object by object.** Two people can change the same scene; GANG only asks when both changed the same property, with a quick question instead of conflict markers (`gang bang`).
- **See who is changing what** before it conflicts (`gang who`, or `gang` for a live panel).
- **Lock** files, or objects inside a scene with the scripts and objects around them (`gang lock`).
- **One setup on macOS, Windows and Linux**: shared ignore rules, LF line endings, `.meta` files kept with their assets, OS junk and unportable names stopped before they are committed.
- **Every git command still works**: `gang status`, `gang log`, `gang rebase -i` …

## Status

Early, working. Tested on macOS against two public Unity projects (11,000 files); not yet used by a team in Unity day to day.

## Install

From a release (macOS, Linux):

```bash
curl -fsSL https://raw.githubusercontent.com/AyberkA0/gang/main/install.sh | sh
```

On Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/AyberkA0/gang/main/install.ps1 | iex
```

From source (Rust 1.85 or newer, git 2.34 or newer, Git LFS):

```bash
cargo install --path crates/gang
```

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
