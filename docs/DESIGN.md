# GANG — Design Specification

> **GANG** — *Gang Avoids Nasty Git-merges*
> Status: Draft v0.1 · 2026-10-17
> Companion documents: [PLAN.md](PLAN.md) (implementation roadmap) · [WORKFLOW.md](WORKFLOW.md) (a day with GANG)

---

## Contents

1. [Overview](#1-overview)
2. [Goals and non-goals](#2-goals-and-non-goals)
3. [Principles](#3-principles)
4. [Architecture](#4-architecture)
5. [Repository layout](#5-repository-layout)
6. [Storage model](#6-storage-model)
7. [Authentication](#7-authentication)
8. [Command model](#8-command-model)
9. [Presence](#9-presence)
10. [Locks](#10-locks)
11. [Unity YAML](#11-unity-yaml)
12. [Semantic diff and noise](#12-semantic-diff-and-noise)
13. [Semantic merge engine](#13-semantic-merge-engine)
14. [Special files](#14-special-files)
15. [gang bang](#15-gang-bang)
16. [Guards](#16-guards)
17. [Managed ignore and attributes](#17-managed-ignore-and-attributes)
18. [Sync, undo and safety](#18-sync-undo-and-safety)
19. [Configuration](#19-configuration)
20. [Output and UX conventions](#20-output-and-ux-conventions)
21. [Known limitations](#21-known-limitations)
22. [Decision log](#22-decision-log)

---

## 1. Overview

GANG is a terminal version-control tool for Unity teams. Users only ever type `gang`; underneath, GANG drives the `git` binary and stores everything on GitHub. It adds what git lacks for Unity projects:

- **Semantic merging** of scenes, prefabs and other Unity YAML assets — object by object, property by property.
- **Quick conflict questions** (`gang bang`) for the few conflicts that are real.
- **Live team awareness** — every teammate publishes which files they are changing.
- **Locks** for files and for objects inside scenes and prefabs, with lock levels.
- **Cross-platform safety** — one managed `.gitignore`, LF line endings, case and Unicode checks, OS junk never committed.
- **The full git command surface** — every git command still works through `gang`.

## 2. Goals and non-goals

### Goals

| ID | Goal |
|---|---|
| G1 | Scene and prefab merges succeed automatically unless two people changed the same property. |
| G2 | GANG never silently produces a broken Unity file (invalid YAML, dangling references, conflict markers). |
| G3 | Teammates see who is changing which file *before* a conflict happens. |
| G4 | Work that cannot be merged (binary files, bake data) or should not be shared (an object being reworked) can be locked. |
| G5 | macOS, Windows and Linux teammates never fight over ignore rules, line endings, file-name case, Unicode or OS junk. |
| G6 | Every git command keeps working through `gang`. |
| G7 | All user-facing text, generated files and documentation are in English. |

### Non-goals (v1)

- A new storage format or transport. Git is the storage, GitHub the only host.
- A GANG server. GitHub is the only backend.
- Hosts other than GitHub.
- Enforcing locks against people who bypass GANG (locks are cooperative, see §10.7).
- Runtime-only interactions (physics contacts, string lookups) in lock levels.
- Unity Editor integration (planned for later, see PLAN.md P13).

## 3. Principles

1. **Git underneath, invisible on top.** A GANG repository is a normal git repository on GitHub.
2. **Never corrupt.** When in doubt, ask. Never write invalid YAML or conflict markers into a Unity file.
3. **Byte-exact.** Data GANG did not change is written back byte for byte.
4. **Deterministic.** Same inputs give the same output on every OS: generated files, merges, JSON state.
5. **No contention.** Shared state is partitioned so two users never write the same data. Where sharing is unavoidable (locks), updates are compare-and-swap.
6. **Transparent passthrough.** Git arguments are forwarded unchanged; exit codes and stdio are preserved.
7. **Script-friendly.** When stdout is not a terminal, GANG prints git's raw output and never prompts.

## 4. Architecture

```
                 ┌──────────────────────────── gang (single binary) ─────────────────────────────┐
  user ────────▶ │  CLI (clap) ─▶ command router ─▶ middleware layers ─▶ git runner               │
                 │       │                │                   │                                    │
                 │   team panel       gang bang        presence · locks · guards                  │
                 │       │                │                   │                                    │
                 │       └──── gang-merge ◀──── unity-yaml ────▶ gang-csharp                       │
                 └──────────────────────────────────────┬─────────────────────────────────────────┘
                                                        │ spawns
                                              git · git-lfs ─────▶ GitHub
```

GANG runs in three kinds of processes:

| Process | Lifetime | Started by |
|---|---|---|
| Team panel (`gang`) | Long-running; owns the file watcher and the presence loop | User |
| One-shot commands (`gang sync`, `gang lock`, passthrough…) | One command | User or scripts |
| Helpers (`gang merge-driver`, `gang credential`, `gang hook`) | One call | git |

## 5. Repository layout

```
gang/
  Cargo.toml                # workspace
  crates/
    gang/                   # binary: CLI, commands, team panel, prompts
    gang-core/              # git runner, auth, config, presence, locks, guards, ignore
    unity-yaml/             # Unity YAML parser/writer, document model, GUID index, hierarchy
    gang-merge/             # semantic diff, noise rules, 3-way merge, special-file mergers
    gang-csharp/            # C# type index (tree-sitter) for lock levels and layer remapping
  tests/
    harness/                # multi-user integration harness (bare remote + N clones)
    fixtures/
      merge/<case>/         # base / ours / theirs / expected
      projects/             # small Unity projects (git-lfs)
  docs/
```

Main dependencies: `clap`, `ratatui` + `crossterm`, `serde` / `serde_json` / `toml`, `keyring`, `notify` + `notify-debouncer-full`, `ureq`, `unicode-normalization`, `tree-sitter` + `tree-sitter-c-sharp`, `globset`, `directories`, `fd-lock`, `anyhow` / `thiserror`. Tests: `tempfile`, `proptest`, `insta`, `expectrl`, `cargo-fuzz`, `criterion`.

## 6. Storage model

| Data | Location | Shared | Written by |
|---|---|---|---|
| Project files | Project branches (`main`, …) | Yes | Users through gang/git |
| Project config | `.gang/config.toml` on project branches | Yes | `gang init`, `gang config --project` |
| Presence | One ref per user, `refs/gang/presence/<github-login>`, holding `.gang/<github-login>.json` | Yes | Each user writes **only their own ref** |
| Locks | Ref `refs/gang/locks`, file `locks.json` | Yes | Compare-and-swap |
| Local state | `.git/gang/` | No | GANG |
| Personal settings | `.git/gang/local.toml` | No | User |
| User settings | `~/.config/gang/config.toml` (platform equivalent) | No | User |
| GitHub token | OS keychain (service `gang`) | No | `gang auth` |
| Safety backups | `refs/gang/backup/<timestamp>` (local, never pushed) | No | Destructive commands |

`.git/gang/` holds `state.json` (presence sequence numbers, last flags), `conflicts.json`, `merge-state.json` (bang progress), `rerere/`, `oplog.jsonl` (for `gang undo`) and `cache/` (GUID index, C# type index, normalized hashes).

Presence files never appear in the working tree. In project branches, `.gang/` only contains `config.toml`.

### 6.1 Shared-ref update protocol

Every update is a commit **without a parent**, built with plumbing only (`hash-object -w`, `mktree`, `commit-tree`), whose message ends with `[skip ci]`. The ref therefore always points at a single commit and history never grows; the working tree, index and current branch are never touched.

**Presence — one ref per user.** Each user force-pushes their own ref, `refs/gang/presence/<login>`, whose tree holds only `.gang/<login>.json`. Nobody else ever writes that ref, so there is no contention and no fetch before writing. Readers fetch `refs/gang/presence/*` and see the `.gang/` folder with one file per user.

**Locks — one shared ref, compare-and-swap:**

1. `git fetch origin refs/gang/locks` → current tip `T` (or none).
2. Build the new `locks.json` from `T`'s plus the change.
3. `git push --force-with-lease=refs/gang/locks:<T> origin <C>:refs/gang/locks`. The server checks the old value atomically, so of two concurrent writers only one wins.
4. If rejected, re-validate the request against the new state and retry with exponential backoff and jitter.

**Every push is retried on failure**, not only on lease rejections: GitHub occasionally fails a push under load (see S1).

### 6.2 Spike S1 results (2026-10-17)

Run with [`spikes/s1_shared_refs.sh`](../spikes/s1_shared_refs.sh) against a private GitHub repository, 4 concurrent writers × 20 updates each:

| Experiment | Time | Retries | Lost updates |
|---|---|---|---|
| Presence on per-user refs, writing as fast as possible | 32 s | 0 | 2 of 80 pushes failed with transient GitHub errors (not retried by the script) |
| Presence on one shared branch, one update every 0–4 s per writer | 246 s | 136 | 0 |
| Locks on one shared ref, writing as fast as possible (worst case) | 196 s | 154 | 0 |

A first run of the shared presence branch with writers pushing non-stop and no backoff starved: one writer won every round and two gave up after 40 retries. This is why presence moved to per-user refs.

| Measurement (average of 5) | Value |
|---|---|
| `git ls-remote` of all GANG refs | 1.06 s |
| `git fetch` with no change | 0.93 s |
| Per-user ref update (push only) | 1.67 s |
| Shared ref update (fetch + push) | 2.83 s |
| Update visible to another clone right after the push | 5 of 5 |

Other findings:

- GitHub accepts pushes to custom refs under `refs/gang/`; they are fetched normally and not shown as branches.
- GitHub Actions: only a branch push whose commit contains a workflow file and no `[skip ci]` started a run. Pushes to `refs/gang/*`, branch pushes of commits without workflow files, and commits marked `[skip ci]` started none. GANG's commits never contain workflow files, so they cannot start runs; `[skip ci]` stays as a second guard.
- Transient server errors (`unable to rename temporary '*.pack' file`) happened on 2.5% of pushes under parallel load.

## 7. Authentication

Before any command that needs the network, and when the panel starts, GANG runs these checks in order:

1. **git** is installed, at least version **2.34**, and **git-lfs** is installed. Otherwise: print install instructions for the current OS.
2. **A token exists** in the OS keychain and is valid (`GET /user`).
3. **Otherwise, GitHub Device Flow:**
   1. `POST https://github.com/login/device/code` with GANG's `client_id` and scopes `repo read:user`.
   2. Show the user code and `https://github.com/login/device`.
   3. Poll `POST https://github.com/login/oauth/access_token` (grant type `urn:ietf:params:oauth:grant-type:device_code`), honoring `interval` and `slow_down`.
   4. Store the token in the keychain.
4. **Credential helper:** register `gang credential` as git's credential helper for `https://github.com` in the repository config. Git then gets the token from GANG for every fetch and push.
5. **Identity:** if `user.name` / `user.email` are unset, fill them from the GitHub profile (`<id>+<login>@users.noreply.github.com` as the email).
6. **Remote:** the `origin` remote must be on `github.com`.

GANG is registered once as a GitHub **OAuth App** with Device Flow enabled. Only the `client_id` is embedded in the binary; no secret is needed.

Commands: `gang auth login`, `gang auth logout`, `gang auth status`.

## 8. Command model

GANG has three kinds of commands:

1. **GANG commands** — no git equivalent (`bang`, `lock`, `sync`…).
2. **Wrapped git commands** — run git, with middleware layers before and after.
3. **Passthrough** — any command GANG does not know is forwarded to `git` unchanged. This covers every git command, future git commands and the user's git aliases.

### 8.1 Middleware layers

| Code | Layer | What it does |
|---|---|---|
| **A** | Auth | Ensures a token and the credential helper before network operations |
| **G** | Guard | Runs the checks in §16 |
| **M** | Meta pairing | Moves, removes and restores an asset together with its `.meta` |
| **E** | Editor guard | Stops operations that would rewrite scenes/prefabs while Unity is open |
| **S** | Semantic merge | Uses the GANG merge driver and offers `gang bang` on conflicts |
| **D** | Unity display | Readable output for scenes and prefabs (disabled by `--raw` or a non-TTY stdout) |
| **L** | Locks | Blocks commits and pushes that touch someone else's locks |
| **P** | Presence | Runs after **every** command: if the local state changed, the presence file is republished |

The merge driver is registered through `.gitattributes`, so the merge part of **S** also works for passthrough commands that merge (`am -3`, `checkout -m`, …). Wrapped commands add the `gang bang` prompt on top.

### 8.2 GANG commands

| Command | Purpose |
|---|---|
| `gang` | Team panel (TUI): team state, your changes, locks, notices. Runs the presence loop while open |
| `gang bang [file] [--list] [--answer <choice>] [--review] [--reset]` | Quick conflict questions, one at a time (§15) |
| `gang save [-m msg] [--push]` | Guarded `add -A` + `commit` (+ push) |
| `gang sync [--rebase]` | Save prompt → fetch → pre-merge passes → merge → bang → push (§18.1) |
| `gang undo` | Undo the last GANG operation (§18.2) |
| `gang who [path]` | Who is changing what; who has unpushed work |
| `gang lock <target>… [--level 0\|1\|2] [-m reason] [--bake] [--partial] [--dry-run] [--yes]` | Take locks (§10) |
| `gang lock --auto [--only kinds] [--dry-run] [--watch\|--off]` | Lock your unpushed files (§10.9) |
| `gang lock --refresh <target\|id>` | Recompute the derived targets of one of your locks |
| `gang unlock <target>… [--bake] [--all] [--force]` | Release locks (`--bake` releases a scene's bake lock) |
| `gang locks [--tree] [--json]` | List locks |
| `gang tree <file>[::<object>] [--depth n]` | Print the object hierarchy of a scene or prefab with object paths and fileIDs |
| `gang ignore add <rule> [--local]` · `update` · `check <path>` · `doctor` | Managed ignore (§17) |
| `gang doctor [--fix]` | Project health: metas, GUIDs, references, Unity settings, LFS, layer numbers in code, hotspots, workflows |
| `gang auth login\|logout\|status` | GitHub authentication |
| `gang git <args>` | Escape hatch: plain git with no layers (auth is still provided) |

Hidden helpers, invoked by git: `gang credential`, `gang merge-driver %O %A %B %P`, `gang hook <name>`.

### 8.3 Wrapped git commands

| Command | Layers | Added behavior |
|---|---|---|
| `init` | G | Writes `.gitattributes`, managed `.gitignore`, `.gang/config.toml`; LFS; driver and hook registration; Unity settings check. `--github` also creates the GitHub repository |
| `clone` | A | `owner/repo` shorthand; LFS; driver and hook registration; fetches presence and locks; Unity version check |
| `config` | — | Passthrough. `--project` edits `.gang/config.toml` |
| `remote` | — | Warns when a non-GitHub remote is added |
| `add` | G M | Adds the `.meta` with its asset; blocks junk and non-portable names; warns on `-f` for ignored files |
| `status` | D | Asset and `.meta` on one line; "noise only" tag; team flags; lock warnings |
| `diff` | D | Semantic diff for Unity YAML |
| `commit` | G L | Pre-commit checks; locked files and objects blocked |
| `restore` | E M | Restores `.meta` pairs. `--noise` reverts noise-only changes. `<file>::<object>` restores one object |
| `reset` | E | `--hard` asks first and saves a backup ref |
| `rm` | M | Removes the `.meta` too; warns about Build Settings and code references when a scene is removed |
| `mv` | M | Moves the `.meta` too; case-only renames done in two steps on case-insensitive file systems; warns about code references when a scene is renamed |
| `clean` | E | Protects `Library/` even with `-x` (needs `--include-library`); shows a dry run first |
| `stash` | E S | `pop`/`apply` use semantic merge; stashed files show as `stashed` in presence |
| `branch`, `tag` | — | Passthrough |
| `switch`, `checkout` | E | Editor guard; `checkout -- <path>` restores `.meta` pairs |
| `merge`, `cherry-pick`, `revert` | E S | Semantic merge; bang on conflicts |
| `rebase` | E S | Bang per step with semantic rerere; warns when rebasing pushed commits |
| `mergetool` | S | Bang for Unity files; the configured tool for others |
| `bisect` | E | Editor guard at every step |
| `worktree` | — | Warns that a new worktree re-imports `Library/` from scratch |
| `log` | D | `--unity` adds a per-commit object summary |
| `show` | D | Semantic diff |
| `blame` | D | Property-level blame for Unity YAML (later phase) |
| `fetch` | A | Also fetches presence and locks |
| `pull` | A E S | Pre-merge passes, semantic merge, bang, lock refresh |
| `push` | A G L | Pre-push integrity and lock checks; LFS; `--force` becomes `--force-with-lease`; force-pushing the default branch asks first |
| `ls-remote`, `submodule`, `lfs` | A | Passthrough with auth. GANG locks replace `git lfs lock` |
| `help` | — | `gang help` shows GANG help; `gang help <git-command>` shows git's page plus GANG's additions |
| `version` | — | GANG, git and git-lfs versions |
| `bugreport` | — | Adds GANG diagnostics |

### 8.4 Passthrough

Everything else goes straight to git, including `am`, `apply`, `archive`, `bundle`, `format-patch`, `send-email`, `request-pull`, `fast-export`, `fast-import`, `replace`, `pack-refs`, `refs`, `rerere`, `sparse-checkout`, `scalar`, `difftool`, `show-branch`, `verify-commit`, `verify-tag`, `merge-tree`, `shortlog`, `describe`, `grep`, `reflog`, `range-diff`, `notes`, `gc`, `maintenance`, `fsck`, `prune`, `repack`, `count-objects`, `diagnose`, the VCS bridges (`svn`, `p4`, `cvsimport`, …), the GUIs (`gitk`, `gui`, `citool`, `instaweb`) and all plumbing (`cat-file`, `hash-object`, `rev-parse`, `ls-files`, `update-ref`, …). Layer **P** still runs afterwards.

### 8.5 Dangerous operations

| Operation | Risk | GANG behavior |
|---|---|---|
| `filter-branch`, `filter-repo` | Rewrites everyone's history | Double confirmation; notice published to the team |
| `push --force` | Deletes others' work | Converted to `--force-with-lease`; default branch asks first |
| `reset --hard`, `checkout -- .`, `restore .` | Loses uncommitted work | Confirmation + backup ref; recoverable with `gang undo` |
| `clean -fdx` | Deletes `Library/` → hours of re-import | `Library/` protected |
| `rebase` of pushed commits | History diverges from the team | Warning |

### 8.6 Implementation rules

- Wrapped commands **do not redefine git's flags**. Arguments are captured raw (`trailing_var_arg`, `allow_hyphen_values`); GANG only sniffs the flags it cares about (`--hard`, `--force`, `-x`, …) and forwards the **original** arguments. The only rewrite is `--force` → `--force-with-lease`.
- The top level is routed by hand, not by clap: GANG's options and git's global options are split off, then the command goes to a GANG command (whose own arguments clap parses), a wrapped git command, or straight to git.
- stdin, stdout and stderr are inherited, so pagers, editors, `rebase -i` and `add -p` work. Git's exit code is returned unchanged.
- GANG commands take precedence over git aliases with the same name; `gang init` warns about such aliases.

## 9. Presence

### 9.1 File format

Each user owns `.gang/<github-login>.json`, stored on their own ref `refs/gang/presence/<github-login>` (§6.1). Keys and paths are always written in sorted order, so an unchanged state produces identical bytes and no push.

```json
{
  "schema": 1,
  "user": "ayberk",
  "github_id": 12345678,
  "seq": 418,
  "ts": "2026-10-17T14:32:10+03:00",
  "machine": "ayberk-mbp",
  "os": "macos",
  "unity": "6000.0.23f1",
  "editor_open": true,
  "branch": "main",
  "head": "a1b2c3d",
  "ahead": 2,
  "files": {
    "Assets/Art/Characters/Hero.psd": { "state": "unpushed", "since": "2026-10-17T13:55:40+03:00" },
    "Assets/Old/Enemy.prefab":        { "state": "deleted",  "since": "2026-10-17T14:01:00+03:00" },
    "Assets/Scenes/Level1.unity":     { "state": "modified", "since": "2026-10-17T14:10:02+03:00" }
  },
  "notices": [
    { "ts": "2026-10-17T12:40:00+03:00", "kind": "force_unlock", "target": "Assets/Art/UI/Logo.psd", "from": "zeynep" }
  ]
}
```

- `ts` is the local wall-clock time, for display only.
- `seq` increases by one on every write. Readers order a user's updates by `seq`, never by `ts`, so an old update can never overwrite a newer one.
- `github_id` identifies the user even if the GitHub login changes. The file name stays the login.
- Only **"changed" flags** are shared — never what changed or how much.
- `notices` holds the user's last 20 team-relevant events (force unlocks, history rewrites).

### 9.2 File states

| State | Meaning | Cleared when |
|---|---|---|
| `modified` | Changed on disk, not committed | The file matches the upstream version again |
| `added` | New file, not pushed | Pushed, or removed again |
| `deleted` | Deleted, not pushed | Pushed, or restored |
| `unpushed` | Committed but not pushed | Pushed, or the commit is undone so the file matches upstream |
| `stashed` | Only in the stash | Stash dropped or applied |

A flag is cleared only when the file matches the **upstream** version (the branch's upstream, or `origin/<default branch>` when there is none). Committing alone does not clear it — the file is still different from what teammates have.

Sources: `git status --porcelain=v2 -z`, `git diff --name-status -z <upstream>...HEAD`, `git stash list`. For Unity YAML files, a modification that is only noise (§12.2) is not flagged.

### 9.3 Timing

| Event | Behavior |
|---|---|
| File changed (watcher) | Debounce 4 s, then publish |
| `save`, `sync`, `lock`, `unlock`, and any command that changed state | Publish immediately |
| Heartbeat | Publish every 60 s even without changes |
| Reading others | Every 15 s: `git ls-remote origin 'refs/gang/*'` (about 1 s on GitHub); fetch only the refs whose tip changed |
| Offline | A user whose `ts` + skew is older than 5 min is shown offline; their flags stay visible |

The watcher ignores `Library/`, `Temp/`, `Logs/`, `obj/`, `UserSettings/` and ignored paths.

**Clock skew:** on every GitHub API call GANG compares the response's `Date` header with the local clock and warns when they differ by more than 30 s. Skew is also used to judge whether teammates are offline.

### 9.4 Consumers

- **Team panel:** team list, flags, locks, notices.
- **`gang status`:** annotates your changed files with teammates' flags on the same files.
- **Overlap warnings:** same Unity YAML file → yellow ("merges automatically unless the same property changes"); same binary file → red ("stop and lock").
- **`gang who [path]`**.
- **Auto locks and lock TTLs** (§10.9).

Without a Unity Editor plugin, GANG knows which files are modified, not which scenes are open.

## 10. Locks

A lock says *"I am working on this; others must not change it until I am done."* A lock has a **root target**, a **level**, and a snapshot of **derived targets**.

### 10.1 Target syntax

| Kind | Syntax | Example |
|---|---|---|
| File | `<path>` | `Assets/Art/Characters/Hero.psd` |
| Folder | `<path>/` | `Assets/Art/Characters/` |
| Glob | `"<pattern>"` (quoted) | `"Assets/Audio/Music/*.wav"` |
| Object | `<scene-or-prefab>::<hierarchy path>` | `Assets/Scenes/Level1.unity::Environment/Player` |
| Object by fileID | `<file>::&<fileID>` | `Assets/Scenes/Level1.unity::&4821937561` |

Object paths:

- Segments are GameObject names separated by `/`, starting at a root object.
- Siblings with the same name are disambiguated with a 1-based index in hierarchy order: `Enemies/Enemy[3]`.
- A `/` inside a name is written as `\/`.
- An empty path after `::` (`Level1.unity::`) opens an interactive picker.
- `gang tree <file>` prints every object with its path and fileID.

Normalization, applied to every target before it is stored or compared:

| Input | Stored as |
|---|---|
| `Assets\Art\Hero.psd` | `Assets/Art/Hero.psd` |
| `assets/art/hero.psd` (case-insensitive FS) | The tracked spelling, `Assets/Art/Hero.psd` |
| NFD names (macOS) | NFC |
| `../Hero.psd` (relative to cwd) | Repository-root-relative path |
| `Hero.psd.meta` | `Hero.psd` (an asset and its `.meta` are always locked together) |

**Name search fallback:** when a target matches no path, GANG searches asset names (`gang lock Hero`) and asks when there are several matches.

### 10.2 Levels

| Level | Locks |
|---|---|
| **0** | The target only |
| **1** | The target and everything that **directly defines it**: scripts attached to the object and all its descendants, and the source prefabs of nested prefab instances in the subtree (with their scripts) |
| **2** | Level 1, plus every **interacting** target (§10.3) and the level-1 expansion of each of them. One hop only — interactions of interacting targets are not followed |

Default level: **1**. `--auto` locks use level 0. `--bake` uses its own fixed target set (§10.10).

What each level means per target kind:

| Target | Level 0 | Level 1 | Level 2 |
|---|---|---|---|
| Binary / other file | File | = level 0 | = level 0 |
| Script (`.cs`) | File | = level 0 | + scripts it references by type, and scripts that reference it by type |
| Prefab (`.prefab`) | File | + scripts on all its objects + nested prefab sources | + interacting targets of the prefab's root |
| Scene (`.unity`) | File | + scripts on all its objects + nested prefab sources | + prefabs and ScriptableObjects referenced by its scripts (E3) |
| Object (`file::path`) | Object subtree | + scripts on the subtree + nested prefab sources | + interacting objects and assets |

An **object lock** covers the object, its components and all descendants with their components (resolved by fileID, so renaming or moving the object does not break the lock). Changing anything in that subtree — including adding or removing children, or reparenting into or out of it — counts as changing the locked object.

### 10.3 Interactions (level 2)

An interaction is a link that can be read statically from the project:

| Edge | Description |
|---|---|
| **E1 — Serialized reference** | A script field in the subtree references an object or component outside it (outgoing), or a script field outside references into the subtree (incoming). UnityEvent targets (`m_PersistentCalls.m_Calls[].m_Target`) count. Same file only |
| **E2 — Code reference** | An object outside the subtree carries a script whose C# code references, by type, a script attached in the subtree — or the other way round. Same file only |
| **E3 — Asset reference** | A script field in the subtree references a **prefab** or **ScriptableObject** asset by GUID. Outgoing only |

Not interactions: hierarchy links (`m_Father`, `m_Children`), component ownership (`m_GameObject`, `m_Component`), prefab bookkeeping (`m_CorrespondingSourceObject`, `m_PrefabInstance`) and rendering assets (materials, meshes, textures, audio, animation).

Not detectable statically, therefore not included: physics contacts, string lookups (`GameObject.Find`, tags), runtime instantiation.

Never locked: built-in Unity components, scripts from packages (`Packages/`, `Library/PackageCache/`), and behaviours defined in DLLs (listed as skipped).

### 10.4 Expansion pipeline

1. Resolve and normalize the root target.
2. Load the file's YAML; build the hierarchy; compute the subtree.
3. Collect `m_Script` GUIDs → script paths through the GUID index (tracked files only).
4. Resolve nested prefab instances → source prefabs → their scripts.
5. Level 2: build E1/E2/E3 edges (E2 uses the `gang-csharp` type index); expand each interacting target to level 1.
6. **Preview** the full target set with the reason for every interacting target. Confirmation is required when the set is larger than `locks.confirm_over` (default 10), unless `--yes`.
7. **All-or-nothing:** if any target is locked by someone else, nothing is locked and the blockers are listed. `--partial` locks the rest.
8. Write the lock in one compare-and-swap update.

### 10.5 Storage

`refs/gang/locks` → `locks.json`:

```json
{
  "schema": 1,
  "locks": [
    {
      "id": "7f3a9c",
      "owner": "ayberk",
      "github_id": 12345678,
      "since": "2026-10-17T09:30:00+03:00",
      "reason": "player rework",
      "kind": "manual",
      "level": 2,
      "root": { "type": "object", "file": "Assets/Scenes/Level1.unity", "file_id": 4821937561, "path": "Player" },
      "derived": [
        { "type": "file", "path": "Assets/Scripts/Player/PlayerController.cs" },
        { "type": "object", "file": "Assets/Scenes/Level1.unity", "file_id": 1029384756, "path": "Main Camera",
          "why": "CameraFollow.target → Player" }
      ]
    }
  ]
}
```

`kind` is `manual`, `auto` or `bake`. Target types are `file`, `folder`, `glob`, `object` and `document` (a single non-GameObject document such as `LightmapSettings`, used by bake locks).

The derived set is a **snapshot** taken when the lock is created, so every machine enforces the same set. `gang lock --refresh` recomputes it, and the owner's GANG refreshes it automatically after the owner commits changes inside the locked subtree (for example, a new script was attached).

### 10.6 Effective lock check

For user *U*, a change is blocked when a lock owned by someone else:

- has a `file` target equal to the changed path (an asset or its `.meta`), a `folder` target that is a prefix of it, or a `glob` target that matches it; or
- has an `object` or `document` target in the same file, and the change touches a document of that object's subtree (computed with the semantic diff, §12.1).

The changes checked are always **the user's own**: staged versus `HEAD` at commit time, and the tree difference between the remote tip and the commits being pushed at push time. Incoming changes from teammates are never blocked.

### 10.7 Enforcement

| Point | Behavior |
|---|---|
| Watcher (panel) | Immediate warning when you modify a locked file or object |
| `gang status` | Warning next to the file, with the object and property you changed and an object-level restore hint |
| pre-commit | Blocks commits that change locked targets |
| pre-push | Re-checks everything being pushed against freshly fetched locks |
| `sync` / `merge` | Warns the lock owner if incoming changes touched their locked targets |

`--no-verify` skips the hooks as in git, but `gang push` still warns. Locks are **cooperative**: someone using plain git can bypass them, and GitHub offers no server-side hooks to prevent that.

### 10.8 Release

- `gang unlock <target>` releases the root and all its derived targets. A derived target that is also covered by another of your locks stays locked.
- `gang unlock --all` releases all your locks.
- `gang unlock <target> --force` releases someone else's lock and publishes a notice to the team.
- **Manual locks:** after a push that changed locked targets, GANG asks `Release your lock on …? [Y/n]`.
- **Auto locks:** released automatically when the file's presence flag clears (pushed or reverted).

### 10.9 Auto locks

```
gang lock --auto [--only binary,unity,code,other] [--dry-run]
gang lock --auto --watch      # keep auto-locking files as you modify them (panel must be running)
gang lock --auto --off
```

- Candidates are your presence flags `modified`, `unpushed`, `added`, `deleted`. Stashed and noise-only files are excluded.
- Filtered by the project policy `locks.auto_allowed` (default `["binary", "unity"]`) and by `--only`.
- **Files a teammate is currently changing are skipped** (their presence shows a flag on the same file): an auto lock never grabs shared, active work.
- Level 0, `kind: "auto"`.
- `--watch` is a personal setting stored in `.git/gang/local.toml`.
- **TTL:** when the owner has been offline (presence heartbeat) for longer than `locks.auto_ttl` (default `8h`), their auto locks are treated as expired: not enforced, shown as `expired`. Manual locks never expire.

File kinds: `binary` (LFS or `binary` attribute), `unity` (Unity YAML assets), `code` (`.cs`, shaders, `.asmdef`), `other`.

### 10.10 Bake locks

`gang lock <scene> --bake` locks only what lighting and navigation bakes produce:

- the bake output folder next to the scene (`Assets/Scenes/Level1/` — lightmaps, `LightingData.asset`, reflection probes, NavMesh data), and the scene's LightingSettings asset when referenced;
- the scene's `OcclusionCullingSettings`, `RenderSettings`, `LightmapSettings` and `NavMeshSettings` documents (`document` targets).

The rest of the scene stays editable and mergeable. With the project policy `[scenes] lighting = "lock"`, committing bake outputs without holding the bake lock is blocked.

## 11. Unity YAML

### 11.1 Format

```yaml
%YAML 1.1
%TAG !u! tag:unity3d.com,2011:
--- !u!1 &4821937561
GameObject:
  m_Component:
  - component: {fileID: 4821937562}
  m_Layer: 0
  m_Name: Player
--- !u!4 &4821937562
Transform:
  m_GameObject: {fileID: 4821937561}
  m_LocalRotation: {x: 0, y: 0, z: 0, w: 1}
  m_LocalPosition: {x: 0, y: 1, z: 0}
  m_Children: []
  m_Father: {fileID: 0}
```

- A file is a header plus documents `--- !u!<classID> &<fileID>`, optionally followed by `stripped`.
- Each document body has one root key (the type name).
- Block mappings use 2-space indentation; small structs use flow mappings; references are `{fileID: N}` or `{fileID: N, guid: G, type: T}`.
- Edge cases: long strings quoted and wrapped over several lines, hex-encoded byte blobs, empty `[]` / `{}`, `stripped` documents.
- It is not standard YAML; a generic YAML library cannot round-trip it.

### 11.2 Parser requirements

- **Byte-exact round trip** for every file in the test corpus.
- **Lazy:** a first pass indexes documents (offsets, class ID, fileID, `stripped`); bodies are parsed on demand.
- **Lossless tree:** nodes keep their original byte spans; the writer re-emits only changed nodes, in Unity's style.
- Large files (50 MB+) supported; fuzz-tested.

```rust
struct UnityFile { header: Span, docs: Vec<Doc>, by_id: HashMap<FileId, usize> }
struct Doc { class_id: u32, file_id: FileId, stripped: bool, span: Span, body: OnceCell<Node> }
enum Node { Map(Vec<(Key, Node)>), FlowMap(Vec<(Key, Node)>), Seq(Vec<Node>), Scalar(Span), Ref(ObjRef) }
```

### 11.3 GUID index

Scans `Assets/**/*.meta` and embedded packages' `.meta` files → `guid → path`. Cached in `.git/gang/cache/` keyed by path, size and mtime. Duplicate GUIDs are reported.

### 11.4 Hierarchy

- GameObject ↔ components through `m_Component` / `m_GameObject`.
- Parent ↔ children through the Transform/RectTransform `m_Children` / `m_Father`.
- Root order from the scene roots list (newer Unity versions) or from root Transforms.
- Prefab instances (`PrefabInstance`, `m_TransformParent`, `stripped` documents) are placed in the tree.
- Names from `m_Name`; object paths as in §10.1.

### 11.5 Supported versions and required settings

- Primary targets: **Unity 2022.3 LTS** and **Unity 6**. Older versions: best effort.
- Required: *Asset Serialization = Force Text* and *Version Control = Visible Meta Files*. `gang init` checks `ProjectSettings/EditorSettings.asset` and offers to fix it.

## 12. Semantic diff and noise

### 12.1 Semantic diff

The diff is computed per document: added, removed and changed documents, and for changed ones the changed property paths. It is rendered with hierarchy names and Inspector names:

```
Level1.unity
  ~ Player › Transform › Position     (0, 1, 0) → (2, 1, 0)
  ~ Player › PlayerController › Jump Height   2.5 → 3
  + Environment › Tree (7)
  − Environment › OldBridge
```

Inspector names come from a built-in table for Unity's types (`m_LocalPosition` → `Position`). Script fields are nicified the way Unity does it (strip `m_` / `_`, split camelCase: `moveSpeed` → `Move Speed`).

The same diff powers `gang diff`, `gang show`, `gang log --unity`, the object-level lock checks and `gang restore <file>::<object>`.

### 12.2 Noise

Noise is a change nobody intended. Built-in rules:

| Source | Rule |
|---|---|
| `RenderSettings.m_IndirectSpecularColor` and similar bake-derived values | Ignore |
| `RectTransform` values driven by a layout component (parent has a LayoutGroup, or self has ContentSizeFitter / AspectRatioFitter) | Ignore |
| Float differences below `merge.float_epsilon` | Equal |
| Re-ordered `m_Modifications` entries | Ignore order |

Projects can add rules in `.gang/config.toml` (§19). A **normalized hash** (semantic content minus noise) is used by presence, `gang status` ("noise only") and `gang restore --noise`.

## 13. Semantic merge engine

### 13.1 Entry point

Git calls `gang merge-driver %O %A %B %P` for the Unity YAML extensions registered in `.gitattributes` (`.unity`, `.prefab`, `.asset`, `.mat`, `.anim`, `.controller`, `.overrideController`, `.mask`, `.physicMaterial`, `.physicsMaterial2D`, `.playable`, `.signal`, `.lighting`, `.terrainlayer`, `.spriteatlas`, `.renderTexture`, `.mixer`, `.preset`, …). `.meta` files use the same driver in plain-YAML mode.

A file whose content is not Unity YAML (binary `.asset` files such as `LightingData.asset`) is handed back as a binary conflict.

The driver exits `0` when the merge is clean. On conflicts it writes a **valid** file using a default resolution (ours) for the conflicting items, records the conflicts in `.git/gang/conflicts.json`, and exits `1`, so git marks the file as unmerged. **Conflict markers are never written into Unity files.**

### 13.2 Algorithm

```
parse(base, ours, theirs)                 → fileID → document maps
document-level 3-way                      → added / removed / changed
property-level merge of changed documents → by field kind (§13.4)
noise rules                               → noise-only conflicts resolved automatically
graph validation and repair               → §13.6
emit                                      → unchanged documents verbatim, changed ones re-written
```

### 13.3 Document-level cases

| Base | Ours | Theirs | Result |
|---|---|---|---|
| — | A | — | Add A |
| — | A | A | Add A |
| — | A | A′ (same fileID, different content) | fileID collision: remap theirs' fileID and its references |
| X | X | X′ | X′ |
| X | — | X | Delete |
| X | — | X′ | **Conflict:** delete / modify |
| X | X′ | X″ | Property-level merge |

### 13.4 Field kinds

| Kind | Examples | Rule |
|---|---|---|
| Scalar | `m_Name`, `m_IsActive`, numbers | Conflict only if both sides changed it to different values |
| Independent flow map | `m_LocalPosition {x, y, z}`, `m_LocalScale` | Merged field by field |
| **Coupled group** | `m_LocalRotation {x, y, z, w}` (+ `m_LocalEulerAnglesHint`); RectTransform layout (`m_AnchorMin`, `m_AnchorMax`, `m_AnchoredPosition`, `m_SizeDelta`, `m_Pivot`); colors | Treated as one value. Different fields changed on each side → **conflict** |
| Identity list | `m_Children`, `m_Component`, scene roots | Ordered-set merge (§13.5) |
| Keyed list | `m_Modifications` keyed by `(target, propertyPath)` | Merged as a map |
| Set | `m_RemovedComponents`, `m_RemovedGameObjects`, `m_AddedGameObjects`, `m_AddedComponents` | Union / difference |
| Atomic array | Script arrays, `m_Materials` | Compared as a whole; both changed differently → conflict |
| Unknown mapping | Script data | Merged key by key |
| Unknown sequence | Script data | Atomic |

The schema (which `(class, path)` has which kind, which fields form coupled groups) is a built-in table that projects can extend.

### 13.5 Lists

Identity lists are merged with a 3-way diff over element identities: both sides' additions are kept, both sides' removals applied. Ordering:

- Outside a Canvas subtree: our order, with their insertions placed after their predecessor.
- Inside a Canvas subtree (sibling order is draw order): conflicting orders are a **conflict** (`merge.ui_order_conflicts = "ask"`).

### 13.6 Graph validation and repair

After merging, the result must satisfy:

1. Every local `{fileID: X}` resolves to a document in the file, is 0, or carries a GUID.
2. `m_Father` ↔ `m_Children` and `m_GameObject` ↔ `m_Component` are symmetric.
3. Deleting a GameObject deletes its components and descendants (cascade).
4. No hierarchy cycles (A moved under B on one side, B under A on the other).

Violations become conflicts, never silent output:

| Situation | Conflict |
|---|---|
| One side deleted an object, the other modified it or added a child under it | Delete / modify (bang offers delete, keep, or re-parent the new children) |
| Both sides moved the same object to different parents | Move |
| Moves on both sides form a cycle | Move |
| A script field references an object the other side deleted | Dangling reference (bang offers keep object, or clear the field) |

### 13.7 Output

- Unchanged documents are copied byte for byte.
- Changed documents are re-emitted in Unity style.
- New documents are inserted where Unity would place them, so the next save in Unity produces no churn: scenes keep documents sorted by fileID (scene settings first, `SceneRoots` last); prefabs follow hierarchy order, so a new document goes after its predecessor on the side that added it. (Measured on 1,368 real scenes and prefabs.)
- LF line endings.

### 13.8 Performance targets

- 3-way merge of a 50 MB scene: < 2 s on a laptop.
- Noise check of a typical scene for presence: < 200 ms (with cache).

## 14. Special files

### 14.1 TagManager

- **Tags:** set merge.
- **Sorting layers:** keyed by `uniqueID`; conflicting order → question.
- **Layers:** index-based. Collisions are resolved **automatically** by the layer remap pre-merge pass (§14.2).

### 14.2 Automatic layer remap

Layers are referenced by number. When two people give the same layer index different names, GANG keeps the version already on the remote and moves the local one — and everything that uses it — to a free layer, then commits that change. No question is asked.

**When it runs:** before git brings in other commits, from every GANG command that does: `gang sync` (merge or rebase), `gang merge <branch>`, `gang pull` (merge or `--rebase`; GANG fetches first to see what will come in), `gang rebase [<upstream>]` and `gang cherry-pick <commit>…`. It does not run for `rebase --onto`/`--root`, `merge --squash`/`--no-commit`, `cherry-pick -n`, `pull --ff-only` or `revert`. It needs a clean working tree. `sync` requires a clean working tree (it offers `gang save` first), so the remap only touches committed work.

**Rebase:** the remap commit sits on top of our commits, so a rebase replays it last. Every earlier commit that added the layer would clash with the upstream name again. Before a rebase the pass records the moved layers in `.git/gang/rebase-layers.json` (with the commit the rebase goes onto), and while that rebase runs the merge driver replays our side of `TagManager.asset` with the layer already at its new number, so it merges cleanly. Our earlier commits keep the old number on objects until the remap commit, which moves them; the end result is the same as with a merge.

**Without the pass** (plain `git pull`, or `layers.auto_remap = false`): the clash reaches `gang bang` as a TagManager `Layers[i]` question. Because either name would silently move the other side's objects onto it, the question says so and suggests aborting and running `gang sync` instead of answering mine/theirs. A layer renamed on both sides (`B[i]` non-empty) is a plain name question.

**Detection** (B = merge base, O = ours, T = theirs, for layers 8–31):

| Case | Condition | Action |
|---|---|---|
| Index collision | `B[i]` empty, `O[i]` and `T[i]` non-empty and different | Move ours from `i` to `k` = lowest index empty in B, O, T and not already chosen |
| Same name, different index | Ours added name *n* at `i` (`B[i]` empty), theirs added *n* at `j ≠ i` | Unify on theirs: remap ours `i → j` |
| Both renamed one layer | `B[i]` non-empty, `O[i]` and `T[i]` both renamed it | **Not a collision**: it is one layer with two names. Nothing moves; the TagManager merge asks which name |
| A teammate already moved a layer | `B[i] = O[i] = n`, theirs has *n* at `j ≠ i` (their remap) | Follow it: our commits' new uses of `i` move to `j`; our TagManager is left to the merge. Nothing is committed when our commits did not use `i` |
| No free index | All of 8–31 used | Fall back to a bang question |

**Rewrite of our side** (only values our commits introduced, i.e. that differ from B at the same property):

| Where | What |
|---|---|
| `TagManager.asset` | Move the name from `i` to `k` |
| Scenes, prefabs, assets | `GameObject.m_Layer == i` → `k` |
| | LayerMask values (`m_Bits`, e.g. camera and light culling masks, script `LayerMask` fields): move bit `i` to `k`. Masks that already have bit `k` ("Everything", "everything except …") are left alone and listed under "Needs review": clearing bit `i` would drop the other side's layer |
| | Prefab overrides: `m_Modifications` entries with `propertyPath: m_Layer` or a `m_Bits` path |
| | Script `int` fields whose name contains "layer" and whose value is `i` (heuristic, listed in the report) |
| `DynamicsManager.asset`, `Physics2DSettings.asset` | Collision matrix: listed under "Needs review" (the matrix encoding is not rewritten automatically yet) |
| C# (`Assets/**/*.cs`, only lines added by our commits) | Layers: `.layer = i`, `.layer == i` / `!=`, `i == x.layer`, `case i:` in a `switch` on a layer, `1 << i` (`1u`, `1L`), assignments to names that say "layer" (`UI3DLayer = i`; not `player`, sorting/rendering layers, NavMesh areas, counts), members of enums named for layers, the layer arguments of `Physics.IgnoreLayerCollision` / `GetIgnoreLayerCollision`, `Physics2D.Get/SetLayerCollisionMask` and `LayerMask.LayerToName`, and `const` / `static readonly` numbers and enum members whose name is used as a layer elsewhere in the same file (`go.layer = Interact`) |
| | Single-layer masks (`1 << i` as a number: `256`, `0x100`, `0b…`, rewritten in the same base): assigned to a `LayerMask` or a name containing "mask", passed as `layerMask:`, passed to `Physics.*` / `Physics2D.*` right after a distance (`100f`, `Mathf.Infinity`, `maxDistance`) or written in hex there, and constants used as masks. Name-based calls (`LayerMask.NameToLayer("…")`, `LayerMask.GetMask("…")`) need no change |
| C#, everything else | Every other whole number `i` or `1 << i` (any base) in code on those lines is **listed under "Needs review"**, whatever the line says, with why it was left (`8 is stored in l, which is used as a layer but is not a constant`). So are masks with more bits that include `i` (where used as a mask, or written in hex/binary) and members of layer enums that are `i` by their position. Strings, comments and preprocessor lines are skipped |

**Commit:** a dedicated commit on our side, authored by the user:

```
gang: move layer UI3D from 8 to 9

Layer 8 is "Pickup" on origin/main (ayberk, 11:02), so "UI3D" moved to
the first free layer.

Updated:
- ProjectSettings/TagManager.asset
- ProjectSettings/DynamicsManager.asset (collision matrix)
- Assets/Prefabs/UI/MainMenu.prefab (3 objects)
- Assets/Scenes/Menu.unity (UI Camera culling mask)
- Assets/Scripts/UI/MenuCamera.cs:42

Needs review:
- Assets/Scripts/UI/MenuRaycaster.cs:17

Gang-Remap: layer 8 -> 9
```

The merge then proceeds and `TagManager.asset` merges without a conflict. Whoever pushes first keeps the index; the others' GANG moves their layer when they sync. The sync output always ends the remap with the review list, or "Nothing needs review."

Known gaps (see [HARDENING.md](HARDENING.md) §1.1): collision matrices are not rewritten. In C#, only the patterns above are rewritten; the review list catches every literal, but some uses have no literal on an added line and cannot be found by reading our changes: values computed at run time (`baseLayer + 1`, `1 << (i + 8)`), compiled DLLs and packages, layers that arrive as data at run time (JSON, ScriptableObjects, save files, server config), constants declared in another file, and code our commits did not add. Name-based calls avoid all of these, so `gang doctor` lists scripts under `Assets/` that use user layers by number and suggests `LayerMask.NameToLayer("…")` / `LayerMask.GetMask("…")`.

### 14.3 Packages

- `Packages/manifest.json`: key-wise JSON merge (`dependencies` by package name, `scopedRegistries` by name). Same package, different versions → question.
- `Packages/packages-lock.json`: rebuilt from the merged manifest's choices; entries for packages chosen from one side are taken from that side.

### 14.4 Other project settings

- `EditorBuildSettings.asset`: scene list keyed by GUID; our order plus their insertions; `enabled` flags merged per scene.
- Collision matrices: merged bit by bit.
- `ProjectVersion.txt`: never merged automatically when versions differ → question (upgrading Unity is a team decision).
- Everything else in `ProjectSettings/`: generic Unity YAML merge.

### 14.5 Other files

| File | Merge |
|---|---|
| `.gitignore`, `.gitattributes` | Managed block regenerated with the newest GANG version; project rules as a sorted union (§17). Never conflicts |
| Code and other text | git's normal merge; conflicts answered hunk by hunk in bang |
| Binary | No merge; bang offers mine / theirs / compare |

## 15. gang bang

### 15.1 When it runs

- `gang bang` explicitly.
- Automatically after `merge`, `pull`, `rebase`, `cherry-pick`, `revert` or `stash pop/apply` end with conflicts on a TTY:
  ```
  3 conflicts found. Resolve them now? [Y/n]
  ```
- On a non-TTY it never prompts; it prints `Run 'gang bang' to resolve 3 conflicts.` and exits `1`.

### 15.2 Screen

```
Level1.unity · conflict 1/3
──────────────────────────────────────────────────────────
Environment › Player › Transform › Position.x

  base     0
  you      2      ayberk · today 10:20 · "Tune jump"
  mehmet   5      mehmet · today 10:28 · "Add forest section"

Why: You both changed the x value of Player's position.

[m] mine   [t] theirs   [b] both (0 +2 +5 = 7)
[o] original   [e] edit   [?] details   [u] undo
```

- Hierarchy names and Inspector names, never raw fileIDs or `m_` names.
- Author, time and commit message of the change on each side (first version: the last commit touching the file on that side; later: property-level blame).
- A one-sentence **Why**.
- Single key press, no Enter.

### 15.3 Options by conflict kind

| Kind | Options |
|---|---|
| Value | `m` mine · `t` theirs · `b` both · `o` original · `e` edit |
| Delete / modify | `d` delete · `k` keep (with the other side's changes) · `r` re-parent the other side's new children to the deleted object's parent |
| Move | `m` mine · `t` theirs |
| UI order | `m` mine · `t` theirs (both orders shown) |
| Same-name additions | `b` keep both · `m` only mine · `t` only theirs |
| Dangling reference | `k` keep the object · `c` clear the field |
| Binary | `m` mine · `t` theirs · `a` open both to compare |
| Text hunk | `m` mine · `t` theirs · `b` both (mine first) · `B` both (theirs first) · `e` edit in `$EDITOR` |
| Package version | `m` mine · `t` theirs |
| `ProjectVersion.txt` | `m` mine · `t` theirs |

Global keys: `?` details, `u` undo the last answer, `q` quit and keep progress.

**Every conflict is asked on its own.** There is deliberately no key or flag that answers several conflicts at once (no "mine for everything", no "theirs for this object"): a conflict is exactly the place where two people changed the same thing, and each one deserves a look. Without a terminal, `gang bang --answer <choice>` answers the *next* open question only, after printing it; scripts call it once per question.

### 15.4 "Both"

| Value type | Meaning | Offered |
|---|---|---|
| Number | Sum of both deltas: `base + Δmine + Δtheirs` (the arithmetic is shown) | Yes |
| Vector | Component-wise delta sum | Yes |
| Lists | Union | Merged automatically, never asked |
| Rotation, color, bool, enum, string, reference | No meaningful combination | No |

### 15.5 Flow

- Progress is saved in `.git/gang/merge-state.json`; `gang bang` resumes after `q`. Meanwhile the file on disk is valid and Unity can open it.
- Before writing, a summary of all answers is shown for confirmation.
- Resolved files are written and staged (`git add`); during a rebase, `--continue` is offered.
- **Semantic rerere:** answers are stored in `.git/gang/rerere.json`; when the same conflict comes back (for example in the next rebase step) the question is still asked, with the earlier answer offered under `Enter`. Nothing is answered without the user seeing it.
- `--review` steps through automatically merged changes too.
- After a merge, a summary of automatic merges is printed:
  ```
  ✓ Level1.unity: 14 changes merged automatically
      mehmet: added 3 objects, Player.Position.y, Light.Intensity
      you:    Camera.FOV, deleted 2 objects
    2 conflicts resolved by you
  ```

## 16. Guards

| Check | Runs in | Severity |
|---|---|---|
| Asset without `.meta`, `.meta` without asset | add, commit | Block (auto-fix offered) |
| `.meta` of an empty folder | commit, doctor | Warn (auto-fix offered) |
| Duplicate GUIDs | commit, doctor | Block |
| OS junk (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `._*`, …) — built-in deny list, independent of `.gitignore` | add, commit | Block (silently unstaged) |
| Paths differing only in case (from each other or from tracked paths) | add, commit | Block |
| Windows-invalid names (`< > : " \| ? *`, trailing dot or space, `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`9`, `LPT1`–`9`) | add, commit | Block |
| Path longer than 200 characters | add, commit | Warn |
| Non-NFC names | add, commit | Auto-normalized |
| CRLF in text assets | commit | Auto-normalized |
| `ProjectVersion.txt` changed locally | commit | Block ("opened with a different Unity version") |
| Wrong local Unity version on clone/open | clone, panel | Warn |
| File > 50 MB not in LFS / > 100 MB | add, commit | Warn / block |
| Signing keys (`*.keystore`, `*.jks`) | add | Ask |
| Unity open (`Temp/UnityLockfile` cannot be locked) | E-layer commands | Block (or warn, see §18.3) |

Pre-push additionally checks reference integrity of what is being pushed: GUIDs referenced but not present, missing prefab sources, prefab override targets that no longer exist.

## 17. Managed ignore and attributes

### 17.1 `.gitignore`

```gitignore
# >>> gang managed v1 · DO NOT EDIT — regenerated by `gang ignore update`
# ── Unity ──────────────────────────────
/[Ll]ibrary/
/[Tt]emp/
/[Oo]bj/
/[Bb]uild/
/[Bb]uilds/
/[Ll]ogs/
/[Uu]ser[Ss]ettings/
/[Mm]emory[Cc]aptures/
/[Rr]ecordings/
*_BackUpThisFolder_ButDontShipItWithYourGame
*_BurstDebugInformation_DoNotShip
sysinfo.txt
crashlytics-build.properties
# ── Generated IDE files ────────────────
*.csproj
*.sln
*.suo
*.user
*.pdb
*.mdb
*.opendb
*.VC.db
# ── IDEs ───────────────────────────────
.vs/
.vscode/
.idea/
*.swp
# ── macOS ──────────────────────────────
.DS_Store
.AppleDouble
.LSOverride
._*
.Spotlight-V100
.Trashes
.fseventsd
.TemporaryItems
# ── Windows ────────────────────────────
Thumbs.db
ehthumbs.db
[Dd]esktop.ini
$RECYCLE.BIN/
*.lnk
# ── Linux ──────────────────────────────
*~
.directory
.fuse_hidden*
.Trash-*
.nfs*
# ── Detected for this project ──────────
/ServerData/
/[Aa]ssets/[Ss]treaming[Aa]ssets/aa/
/[Aa]ssets/[Ss]treaming[Aa]ssets/aa.meta
# <<< gang managed

# Project rules — added with `gang ignore add`, kept sorted
/[Aa]ssets/_[Ss]andbox/
/[Aa]ssets/_[Ss]andbox.meta
```

Rules:

- **All three OSes' rules are always included**, whoever runs `gang init`. Output is deterministic (fixed order, LF), so two people generating it get identical bytes.
- The Unity section is based on the community template `github/gitignore` (CC0), embedded in the binary and versioned.
- **Case-insensitive patterns** (`[Ll]ibrary`): a rule written as `library/` would ignore `Library/` on macOS/Windows but not on Linux. `gang ignore add` rewrites user rules into this form.
- **`.meta` pairs:** every rule under `Assets/` gets its `.meta` rule.
- **NFC** for every rule; `.gitignore` itself is forced to LF (a CRLF `.gitignore` silently stops matching on Unix).
- **Project root:** when the Unity project is in a subfolder, rules are prefixed; several Unity projects in one repository are supported.
- **Detection:** Addressables (`ServerData/`, `aa/`), Android builds (`*.apk`, `*.aab`), Burst/IL2CPP folders, Asset Store Tools. Signing keys are never decided silently (§16).
- **Personal rules** (`gang ignore add --local`) go to `.git/info/exclude`.

Commands: `gang ignore add <rule> [--local]`, `gang ignore update`, `gang ignore check <path>` (wraps `git check-ignore -v`), `gang ignore doctor` (tracked files that should be ignored → offers `git rm --cached`; missing `.meta` rules; CRLF; case problems).

`gang init` on an existing repository moves the old rules into the project section and drops duplicates of the managed block.

### 17.2 `.gitattributes`

Managed the same way (block + project rules):

```gitattributes
# >>> gang managed v1 · DO NOT EDIT
* text=auto eol=lf
*.unity  text eol=lf merge=gang
*.prefab text eol=lf merge=gang
*.asset  text eol=lf merge=gang
*.meta   text eol=lf merge=gang
*.mat    text eol=lf merge=gang
# … other Unity YAML extensions
*.cs     text eol=lf diff=csharp
.gitignore     text eol=lf merge=gang-ignore
.gitattributes text eol=lf merge=gang-ignore
**/LightingData.asset filter=lfs diff=lfs merge=lfs -text
*.psd filter=lfs diff=lfs merge=lfs -text
*.png filter=lfs diff=lfs merge=lfs -text
*.fbx filter=lfs diff=lfs merge=lfs -text
*.wav filter=lfs diff=lfs merge=lfs -text
# … images, models, audio, video, fonts, archives, native libraries
# <<< gang managed
```

**Git LFS is off by default** (`[lfs] enabled = false`): GitHub's LFS storage is paid and has a quota, and one full quota stops every push of the team. With LFS off, the same file types get `binary` instead (`*.[pP][nN][gG] binary`), so git never treats them as text and never tries to merge them. `gang init` and `gang clone` then neither install nor pull LFS, and the hooks do not require git-lfs. A project without the `[lfs]` setting (set up by an older GANG) keeps LFS when its `.gitattributes` already uses it. `gang doctor` warns when LFS is off but files in the branch are still stored in LFS.

Binary families (in LFS when it is on): images (`psd png jpg jpeg tga tif tiff exr hdr`), models (`fbx obj blend max ma mb`), audio (`wav mp3 ogg aif aiff flac`), video (`mp4 mov webm`), fonts (`ttf otf`), archives (`zip 7z`), native libraries (`dll so a dylib bundle`), `*.bytes`, and known binary Unity outputs (`LightingData.asset`, lightmap `.exr`).

## 18. Sync, undo and safety

### 18.1 `gang sync`

1. Uncommitted changes → `You have uncommitted changes. Save them first? [Y/n]` (runs `gang save`), or abort.
2. Fetch the branch, presence and locks.
3. Pre-merge passes (layer remap, §14.2).
4. Editor guard: if incoming changes touch scenes or prefabs and Unity is open → stop (§18.3).
5. Merge the upstream (default strategy: merge; `--rebase` optional).
6. Conflicts → bang (TTY) or exit `1` with a hint.
7. Post-merge reference integrity check → warnings.
8. Push (pre-push checks).
9. Auto locks on pushed files released; release prompt for manual locks on pushed targets.
10. Presence published; summary printed.

### 18.2 Undo and backups

- Every GANG operation that changes history or the working tree appends to `.git/gang/oplog.jsonl` (operation, `HEAD` before, backup ref).
- Before destructive operations GANG snapshots the working tree into `refs/gang/backup/<timestamp>` (`git stash create` + `update-ref`). These refs are local only and pruned after 14 days.
- `gang undo` reverts the last operation using the op log (for example: un-commit a `save` keeping the changes, restore the tree before a `reset --hard`, undo a `sync` merge that has not been pushed).

### 18.3 Editor guard

- Unity holds an exclusive lock on `Temp/UnityLockfile` while it runs. GANG tries to take that lock (`fd-lock`); if it cannot, Unity is open. A stale file after a crash is not locked, so it is not mistaken for a running editor.
- Operations that would rewrite scenes or prefabs while Unity is open are blocked (`sync.editor_open = "block"`); `gang --ignore-editor <command>` runs them anyway.
- Single-file, user-initiated rewrites such as `gang restore <file>::<object>` only warn: `Unity is open: let it reload Level1 before you keep editing.`
- With an Editor plugin (later), GANG will know which scenes are open and automate save → close → sync → reopen.

## 19. Configuration

Precedence: command-line flag > `.git/gang/local.toml` > user config > `.gang/config.toml` > built-in defaults. Keys marked **policy** can only be set in the project config.

```toml
# .gang/config.toml
[project]
unity_root = "."                  # auto-detected
default_branch = "main"

[presence]
poll_interval = "15s"
debounce = "4s"
heartbeat = "60s"
offline_after = "5m"

[locks]
default_level = 1
confirm_over = 10
auto_allowed = ["binary", "unity"]   # policy
auto_ttl = "8h"                       # policy

[scenes]
lighting = "lock"                     # policy: lock | ignore | free

[scenes.policy]                       # policy
"Assets/Scenes/Main.unity" = "lock-required"
"Assets/Scenes/Levels/*"   = "merge"

[merge]
float_epsilon = 1e-6
ui_order_conflicts = "ask"

[[merge.noise]]
class = "MonoBehaviour"
script = "Assets/Scripts/UI/AutoLayout.cs"
path = "cachedSize"
rule = "ignore"

[layers]
auto_remap = true

[sync]
strategy = "merge"                    # merge | rebase
editor_open = "block"                 # block | warn

[lfs]
enabled = false                       # policy: store binaries in Git LFS (paid on GitHub)
```

## 20. Output and UX conventions

- English only.
- Symbols: `✓` done, `✗` blocked/failed, `⚠` warning, `🔒` / `🔓` lock taken/released, `●` online, `○` offline.
- Colors through crossterm; `NO_COLOR` respected.
- Global flags: `--raw` (git's output), `--json` (machine output for `status`, `who`, `locks`, `tree`), `--yes` (accept confirmations). They go **before** the command (`gang --raw log`): everything after the command belongs to it, so `gang log --raw` is git's own `--raw`. git's global options (`-C`, `-c`, `--no-pager`, …) are accepted in the same position and passed to git.
- `GANG_DEBUG=1` prints how each command was routed (GANG command, wrapped with which layers, or passthrough).
- Exit codes: passthrough returns git's code. GANG commands: `0` success, `1` conflicts or blocked by a check, `2` usage error, `3` auth or network error.
- Every block message says what to do next.

## 21. Known limitations

- Locks are cooperative; plain git can bypass them.
- Without an Editor plugin, GANG cannot tell which scenes are open.
- Layer remapping in C# only handles recognized patterns; the rest is reported for review.
- Lock interactions are static (§10.3).
- The insertion order of new documents must match Unity's; this is confirmed empirically (spike S2) and covered by tests per supported Unity version.
- Presence is designed for small teams (4–10 people); larger teams are untested against GitHub's limits.

## 22. Decision log

| Date | Decision | Rejected alternatives |
|---|---|---|
| 2026-10-15 | Presented as its own VCS, built on the `git` binary | Writing a new storage engine |
| 2026-10-15 | GitHub is the only host | Other hosts, a GANG server |
| 2026-10-15 | Presence: per-user `.gang/<login>.json`, local `ts` + `seq`, "changed" flags only | A single shared `.gang` file (self-conflicting); sharing change details |
| 2026-10-15 | Presence on a separate ref, written with plumbing | Committing presence to project branches (history noise, conflicts, CI runs) |
| 2026-10-15 | Locks on a single ref with compare-and-swap | Per-user lock refs (no mutual exclusion) |
| 2026-10-16 | Rust | Go |
| 2026-10-17 | English everywhere | — |
| 2026-10-17 | Full git command surface: wrapped commands + passthrough | Re-implementing a subset |
| 2026-10-17 | `gang bang` quick conflict questions; no conflict markers in Unity files | git's markers, external merge tools |
| 2026-10-17 | Quaternions and RectTransform layout are coupled groups | Field-wise merge for all flow maps |
| 2026-10-17 | `--bake` locks bake outputs and settings documents only | Locking the whole scene |
| 2026-10-17 | Auto locks default to `binary` and `unity`; skip files teammates are changing | Auto-locking code |
| 2026-10-17 | Layer collisions remapped automatically and committed | Asking every time |
| 2026-10-17 | Object locks; lock levels 1 (direct) and 2 (+ interacting), plus level 0 (target only) for auto and file-only locks | — |
| 2026-10-17 | Bang re-parent option for delete/modify | Only delete / keep |
| 2026-10-17 | Team panel first; background daemon later | Daemon first |
| 2026-10-17 | Presence on per-user refs `refs/gang/presence/<login>` (spike S1) | One shared `gang-presence` branch: writers contend and starve |
