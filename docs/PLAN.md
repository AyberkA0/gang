# GANG — Implementation Plan

> Status: Draft v0.1 · 2026-10-18
> What to build is specified in [DESIGN.md](DESIGN.md); how a team uses it is shown in [WORKFLOW.md](WORKFLOW.md). This document says **in which order** to build it and **how we know each step is done**.

---

## 1. Milestones

| Milestone | Phases | What the team gets |
|---|---|---|
| **M1 · Safe git** | P0–P4 | `gang` replaces `git`. Cross-OS problems, `.meta` mistakes, junk files and ignore conflicts are gone. |
| **M2 · Team awareness** | P5 | Live panel: who is changing which file, who has unpushed work. |
| **M3 · Scene merging** | P6–P8 | Scenes and prefabs merge semantically; remaining conflicts are answered in `gang bang`. |
| **M4 · Locks** | P9–P10 | File, auto, bake and object locks with levels. |
| **M5 · Release** | P11–P12 | `gang doctor`, installers, complete docs. |
| Later | P13 | Unity Editor package, daemon, property-level blame, … |

Each milestone is usable on its own. From M1 on, the team **dogfoods** GANG on a real Unity project, and every milestone ends with a week of real use before the next one starts in earnest.

Sizes are rough relative estimates for one full-time developer: **S** ≈ a few days · **M** ≈ 1–2 weeks · **L** ≈ 3+ weeks.

---

## 2. Phases

### P0 · Foundations and spikes — M

**Goal:** a building, tested workspace on all three OSes, and answers to the open technical questions before code depends on them.

- [x] Cargo workspace with `gang`, `gang-core`, `unity-yaml`, `gang-merge`, `gang-csharp`; `rust-toolchain.toml`; rustfmt and clippy settings.
- [x] GitHub Actions: macOS, Windows, Linux × (fmt, clippy `-D warnings`, test).
- [x] **Spike S1 — shared refs on GitHub:** on a throwaway GitHub repository, push parentless commits to `refs/gang/locks` and to branch `gang-presence` with `--force-with-lease`; 4 parallel writers for 10 minutes; confirm no lost updates and that `[skip ci]` suppresses workflows. Result: DESIGN §6.2 — custom refs work; presence moved to per-user refs; pushes need retries on transient errors.
- [ ] **Spike S2 — Unity YAML corpus:** collect scenes, prefabs and assets from our own project and public Unity sample projects (2022.3 and Unity 6). Write a throwaway document indexer. In Unity, add objects to a scene and record **where new documents are inserted** (end of file vs. fileID order) — needed for DESIGN §13.7.
- [ ] **Spike S3 — OAuth Device Flow:** register the "GANG" OAuth App with Device Flow enabled; minimal login that prints the user's login.
- [ ] **Spike S4 — editor detection:** confirm the `Temp/UnityLockfile` try-lock approach on macOS, Windows and Linux, including after a Unity crash.
- [ ] **Test Unity project:** a small Unity project in its own GitHub repository (for fixtures and multi-user tests).

**Done when:** CI is green on 3 OSes; S1–S4 results are written into DESIGN.md; the test project exists.

---

### P1 · CLI skeleton and passthrough — M

**Goal:** `gang <anything>` behaves exactly like `git <anything>`.

- [x] Top-level routing: GANG options and git's global options (`-C`, `-c`, `--no-pager`, …) are split off by hand; the command is then routed to a GANG command (parsed with clap), a wrapped git command, or passthrough. git runs with inherited stdio and GANG ignores Ctrl+C while it waits.
- [x] Exit-code propagation (including termination by signal).
- [x] Layer registry mapping each wrapped command to its layers (DESIGN §8.1, §8.3).
- [ ] Layer `before` / `after` hooks and the flag-sniffing helper — added with the first real layer (P3/P4).
- [x] `--raw` and `--json` parsed; GANG options go before the command (`gang --raw log`), since `gang log --raw` is git's flag.
- [x] TTY detection, `NO_COLOR`, `--yes`.
- [x] Git runner: inherited runs, version detection, error mapping with per-OS install hints.
- [ ] Capture helpers and `-z` output parsers — added with P4/P5.
- [x] Minimum git version decided: **2.34** (Ubuntu 22.04's git; Apple's bundled git passes). git-lfs is reported by `gang version` and enforced in P3.
- [x] `gang git`, `gang version [--json]`, `gang help` (+ `help <git-command>`). `GANG_DEBUG=1` prints how a command was routed.

**Done when:** a passthrough test suite runs ~30 representative git commands through `gang` and through `git` in temporary repositories and gets identical output and exit codes; `rebase -i`, `add -p`, pager and editor work (manual check on 3 OSes).

> Status: the automated part is done (`crates/gang/tests/passthrough.rs`, 40 cases). The manual interactive check is still open.

---

### P2 · Authentication — S

**Goal:** a new teammate logs in once and never sees a password prompt.

- [x] Device Flow client (`ureq`), honoring `interval` and `slow_down`. The OAuth App's client ID is built in by release builds (`GANG_GITHUB_CLIENT_ID` at compile time) and can be overridden at run time.
- [x] Keychain storage (`keyring`), with a private file as fallback; `gang auth login | logout | status` (status also checks scopes and clock skew).
- [x] `gang credential` helper (`get` / `store` / `erase`, github.com only) and its registration per repository.
- [x] Sign-in fills `user.name` / `user.email` from GitHub when git has none; `gang init --github [--public] [--name]` creates the repository. Commands do not force a sign-in: existing git credentials keep working.
- [x] Revoked or expired token → clear message and re-login.

**Done when:** a fresh machine on each OS goes from `gang clone` to `gang push` with only the Device Flow step.

> Status: tested end to end against a fake GitHub server. Registering the real OAuth App (one-time, on github.com) is still needed.

---

### P3 · Project setup — M

**Goal:** `gang init` / `gang clone` give every machine the same correct setup.

- [x] Unity project detection: `ProjectSettings/ProjectVersion.txt`, project in a subfolder, several projects in one repository.
- [x] `EditorSettings.asset` check and fix (Force Text, Visible Meta Files).
- [x] Managed `.gitignore`: embedded rules, project detection, deterministic output, project-rules section, migration of an existing `.gitignore`.
- [x] `gang ignore add [--local] | update | check | doctor [--fix]`.
- [x] Managed `.gitattributes`: line endings, `merge=gang`, LFS families, `LightingData.asset` rule.
- [x] `gang-ignore` merge driver for `.gitignore` / `.gitattributes` (regenerate managed block, sorted union of project rules).
- [x] `.gang/config.toml` schema, loader, precedence (DESIGN §19); `gang config --project`.
- [x] Merge driver and hook registration; hook shims in `.git/hooks/` call `gang hook <name>` and chain existing hooks. The shims also run Git LFS's hooks, so LFS is installed with `--skip-repo`. A repository cloned with plain git is set up by the next wrapped `gang` command.
- [x] `gang init`, `gang clone` (`owner/repo`, LFS, presence and locks fetch, Unity version check). `gang init --github` waits for P2 (auth); presence/lock fetch is wired in P5.

**Done when:** running `gang init` on the same messy test project on macOS, Windows and Linux produces byte-identical `.gitignore`, `.gitattributes` and `config.toml` (compared in CI); `gang ignore doctor` finds every seeded problem.

---

### P4 · Guards and safe working-tree commands — M

**Goal:** the mistakes that break Unity projects across machines cannot be committed.

- [x] Guard engine producing findings (block / warn / auto-fix) shared by hooks and commands.
- [x] `.meta` rules: pairing, orphan metas, empty-folder metas, duplicate GUIDs.
- [x] Portability: case collisions, Windows-invalid names, trailing dot/space, path length, NFC normalization.
- [x] Built-in junk deny list; large files outside LFS; signing keys. (CRLF is normalized by the managed `.gitattributes`.)
- [x] Unity version guard (`ProjectVersion.txt` changed locally; `GANG_ALLOW_UNITY_UPGRADE=1` allows it); editor-open detection (`Temp/UnityLockfile` lock, still to be confirmed with a real Unity by spike S4).
- [x] Wrappers: `add`, `commit`, `rm`, `mv` (two-step case-only renames), `restore`, `reset` (`--hard` confirmation + backup ref), `clean` (`Library/` protection), `checkout` / `switch`, `stash`.
- [x] `gang save`; op log; `gang undo` for `save`, `reset`, `clean`, `restore`.
- [x] Dangerous-operation confirmations; `push --force` → `--force-with-lease`. The editor guard is skipped with `gang --ignore-editor <command>`.

**Done when:** each guard rule has a fixture repository with the expected findings, run in CI on all three OSes; the team uses GANG for a week on a real project without falling back to git.

> **M1 reached.**

---

### P5 · Presence and team panel — L

**Goal:** everyone sees, within seconds, who is changing which file.

- [x] Presence writer: parentless commit on the user's own ref, pushed with retries. (The lease-based shared writer comes with locks in P9.)
- [x] Presence model with deterministic JSON; `seq` persisted in `.git/gang/state.json`.
- [x] Flag computation from status, upstream diff and stash; `since` tracking; clearing rules (DESIGN §9.2).
- [x] File watcher (`notify`, own debounce) with ignored folders; heartbeat; poller using `ls-remote` tip comparison. Outside the panel, every state-changing `gang` command and the post-commit/checkout/merge hooks publish in a detached background process.
- [x] Clock-skew detection from GitHub's `Date` header (`gang auth status`).
- [x] `gang who [path]`; teammates' flags in `gang status`; overlap warnings (yellow for Unity YAML, red for binary).
- [x] Notices: `gang unlock --force` and `gang filter-branch` / `filter-repo` publish them; the panel shows the last day's, newest first.
- [x] Team panel v1 (`ratatui`): team, my changes with overlap warnings; keys `q`, `r`, `s` (save), `y` (sync), `b` (bang); locks and notices.

**Done when:** the multi-user harness runs 4 simulated users (4 clones of a local bare remote, then once against GitHub) changing files for 30 minutes: no lost updates, the presence ref always has exactly one commit, 95% of changes are visible to others within 20 s.

> **M2 reached.**

---

### P6 · Unity YAML core and semantic diff — L

**Goal:** GANG understands Unity files without ever changing a byte it should not.

- [x] Lossless parser and writer (DESIGN §11.2). Documents are parsed whole rather than lazily: 366 MB of real projects parse in about 4 s, the largest scene in 0.15 s.
- [x] Round-trip test over the whole corpus (`UNITY_CORPUS=<dir> cargo test -p unity-yaml --test corpus`): 11,004 files from two public projects (Unity 2020.3 and Unity 6), 100% byte-for-byte; re-formatting from scratch matches Unity in all but 2 files (trailing blank line). Fuzzing and proptest are still open.
- [x] Document model, reference extraction, GUID index (no cache yet; a scan is fast enough for now).
- [x] Hierarchy builder including prefab instances and `stripped` documents; all 30,950 objects in the corpus resolve back from their paths.
- [x] Object addressing (`::` paths, `[n]`, `&fileID`, escaping) and `gang tree [--depth n]`.
- [x] Inspector-name table and field nicifying.
- [x] Semantic diff and renderer: `diff`, `show` (on a terminal), `log --unity`, noise notes in `status`.
- [x] Noise rules (bake-derived settings, layout-driven RectTransforms, float epsilon, config rules); presence ignores noise-only files; `restore --noise`; object-level `restore <file>::<object>` (also undoes moves).
- [ ] Benchmarks (`criterion`) on large scenes — timing is measured in the corpus test for now.

**Done when:** 100% of corpus files round-trip byte for byte; indexing a 50 MB scene takes < 500 ms; snapshot tests (`insta`) cover the diff output of 30 hand-made edits.

---

### P7 · Semantic merge engine — L

**Goal:** scenes and prefabs merge unless two people changed the same property, and the result always opens in Unity.

- [x] `gang merge-driver` (Unity YAML and JSON); conflict store in `.git/gang/merge/<file>/` (the three versions, the questions, the answers).
- [x] Document-level 3-way merge (DESIGN §13.3) including fileID collision remapping.
- [x] Property-level merge by field kind; coupled groups (rotation, colors, RectTransform layout); same-length lists merge element by element (layers, material slots).
- [x] Ordered-set list merge with UI order conflicts (RectTransform); keyed lists (prefab overrides by target and property, lists with a `guid`); prefab sets.
- [x] Graph repair (children, components and scene roots follow the parents; cycles broken) and delete/modify conflicts with delete / keep / re-parent. Repair only touches what the merge broke.
- [x] Document insertion order: scenes keep documents sorted by fileID, prefabs follow hierarchy order (measured on the corpus: 685/685 scenes sorted, 22/683 prefabs).
- [x] `.meta` merge; binary `.asset` detection (binary files are left to `gang bang`).
- [x] Special mergers: `manifest.json` / `packages-lock.json` (JSON, key by key), `EditorBuildSettings` (scenes by guid), `TagManager` (layers by index), `ProjectVersion.txt` (always asks). Collision matrices merge as values.
- [x] **Layer remap pre-merge pass** (DESIGN §14.2) in `gang sync` and `gang merge <branch>`: detection, YAML rewrite (objects, masks, prefab overrides, layer-named script fields), C# rewrite of recognized patterns, review list, dedicated commit. Collision matrices are listed for review rather than rewritten.
- [ ] Golden fixtures produced in Unity — needs a Unity install. For now: 16 hand-written scenarios (one per root cause and conflict kind) in `crates/gang-merge/tests/merge.rs`, and corpus properties (merging any of 1,566 real files with itself changes nothing).
- [ ] Unity-in-the-loop check — needs a Unity install.

**Done when:** all golden fixtures pass; merged outputs open in Unity without errors or missing references; a property test confirms conflict markers are never written into Unity files.

---

### P8 · gang bang and sync — M

**Goal:** the remaining real conflicts take seconds to answer.

- [x] Prompt engine (single-key input with `crossterm`), screen per conflict kind, details view.
- [x] Options per kind (DESIGN §15.3), including "both" arithmetic, re-parent and value editing.
- [x] `M` / `T`, undo, quit and resume, final summary with confirmation.
- [x] Applying answers: re-run the merge with the answers, stage, then commit the merge or continue the rebase / cherry-pick / revert.
- [x] Text hunks with `$EDITOR`; binary mine / theirs / save theirs to compare.
- [x] Semantic rerere (`.git/gang/rerere.json`).
- [x] Auto-trigger after merging commands; non-TTY hint; `--abort` clears stored conflicts.
- [x] `gang sync` (DESIGN §18.1) with retry when someone pushed meanwhile; `pull` / `merge` / `rebase` wrappers; automatic-merge summary. Post-merge integrity warnings come with P11.

- [x] pty-driven tests (`crates/gang/tests/interactive.rs`, `portable-pty`, ConPTY on Windows): the sync offer, every conflict kind (value, move, order, deleted by you / them, package, code hunk, binary) with keys m / t / b / o / e / u / M / T / ? / q and delete / keep / re-parent, quit and resume, the release prompt after push, and the extra `gang status` sections.

**Done when:** pty-driven tests (`portable-pty`) cover every conflict kind; the WORKFLOW.md day is replayed end to end by the harness with four clones.

> **M3 reached.**

---

### P9 · File locks — M

**Goal:** nobody loses work on files that cannot be merged.

- [x] `locks.json` model; compare-and-swap writes (`--force-with-lease`, retries with backoff); effective-lock computation for file, folder and glob targets.
- [x] `gang lock | unlock | locks [--tree]` with `-m`, `--force`, `--all`, lock ids, normalization, name search and picker.
- [x] Enforcement: panel warnings, `status`, pre-commit (cached table), pre-push (fresh table), incoming-change warnings in `sync`.
- [x] Release prompt after push; auto locks (`--auto`, `--only`, `--dry-run`, `--watch` / `--off`), skipping code and files teammates are changing, automatic release on push, TTL expiry.
- [x] Bake locks (folder + settings documents + LightingSettings asset) and the lighting policy in pre-commit.

**Done when:** 4 users locking the same file at the same moment → exactly one succeeds, every time (100 runs); enforcement fixtures pass; TTL expiry works in a simulated offline scenario.

---

### P10 · Object locks and levels — L

**Goal:** lock an object and everything that defines it or talks to it.

- [x] `gang-csharp`: type index (declared types and identifiers, comments and strings excluded) with a small lexer instead of tree-sitter, so no C toolchain is needed. Not cached yet.
- [x] Interaction graph: E1 serialized references (with UnityEvent methods), E2 code references, E3 prefab and ScriptableObject references.
- [x] Level expansion for every target kind (DESIGN §10.2); preview with reasons and grouping; confirmation threshold (`--yes`); all-or-nothing and `--partial`.
- [x] `object` and `document` targets; semantic enforcement (changed documents ∩ locked subtrees) in hooks and `status`, with an object-level restore hint.
- [x] Derived snapshots and `--refresh`. Automatic refresh for the owner is still open.
- [x] `gang locks --tree`.

**Done when:** a fixture project with known references produces the expected target set for each level; in the same scene, changes to a locked object are blocked while changes to other objects commit normally.

> **M4 reached.**

---

### P11 · Doctor and integrity — S

- [x] Cross-file reference integrity: missing assets and prefab sources (package assets are checked once Unity has filled `Library/PackageCache`). Missing override targets are still open.
- [x] Scene rename → string references in code (warned by `gang mv`); Build Settings consistency.
- [x] Hotspots from history (scenes and prefabs changed on both sides of recent merges) with suggestions.
- [x] Not needed: spike S1 showed GANG's refs never start workflow runs.
- [x] `gang doctor [--fix] [--json]` for the safe fixes (drivers and hooks, Unity settings, tracked junk, orphan metas). Also checks metas, GUIDs, LFS and the installed Unity version.

**Done when:** a seeded test project produces the expected report.

---

### P12 · Distribution and docs — S

- [x] Release workflow (`.github/workflows/release.yml`, no cargo-dist needed): macOS (arm64, x64), Windows x64, Linux (x64, arm64); GitHub Releases on a `v*` tag; the OAuth client ID comes from a repository secret.
- [x] Installers: `install.sh`, `install.ps1`. Homebrew tap and Scoop bucket need their own repositories — still open.
- [ ] Update notice (no self-update) — still open.
- [x] README, command reference (`docs/COMMANDS.md`, from `gang help`), WORKFLOW.md matched to the real output. A troubleshooting guide is still open.

**Done when:** a new teammate on each OS installs GANG and joins the test project using only the README.

> **M5 reached.**

---

### P13 · Later

- Unity Editor package: open-scene awareness, lock badges in the Project and Hierarchy windows, protection against saving locked objects, safe sync (save → close → sync → reopen).
- Read-only checkout of files locked by others (after testing how Unity handles read-only assets).
- Background daemon so presence runs without the panel.
- Property-level blame (`gang blame` for scenes, better bang attribution).
- Heuristic interactions from string lookups (`GameObject.Find`, tags).
- GitHub pull request helpers.

---

## 3. Dependencies

```
P0 ─▶ P1 ─▶ P2 ─▶ P3 ─▶ P4 ──▶ (M1)
                         │
                         ├─▶ P5 ──────────────▶ (M2)
                         │
                         └─▶ P6 ─▶ P7 ─▶ P8 ──▶ (M3)
                                   │
                      P5 + P6 ─▶ P9 ─▶ P10 ───▶ (M4)
                                          │
                                 P11 ─▶ P12 ──▶ (M5)
```

- The layer remap in P7 needs the C# index from `gang-csharp`; its first version is built in P7 and extended in P10.
- P9 needs the shared-ref writer (P5) and the parser (P6) for bake `document` targets.
- P5 and P6 can be developed in parallel after M1.

---

## 4. Testing strategy

| Level | What | Where |
|---|---|---|
| Unit | Pure functions: normalization, flag state machine, field-kind rules, "both" arithmetic | Each crate |
| Snapshot | Rendered output: diffs, bang screens, panel frames, `status` | `insta` |
| Round trip | Every corpus file parses and re-emits byte for byte | `unity-yaml` |
| Property / fuzz | List merge laws, no conflict markers, parser never panics | `proptest`, `cargo-fuzz` |
| Golden merges | `base / ours / theirs / expected` per root cause | `tests/fixtures/merge/` |
| Multi-user integration | A bare remote plus N clones, each with its own config and a fake token; scripted scenarios (presence, locks, sync, layer remap) | `tests/harness/` |
| Interactive | pty-driven bang and confirmation prompts | `expectrl` |
| Cross-OS | Everything above on macOS, Windows, Linux | GitHub Actions |
| Unity-in-the-loop | Merged scenes opened in Unity batch mode | Script, later self-hosted runner |
| GitHub end-to-end | Auth, shared refs and `[skip ci]` on a real repository | Nightly job on a dedicated test repository |

**Making a golden fixture:** in the test Unity project, commit a base; on branch A make one edit in Unity and save; on branch B make another; export the three versions; write `expected` by hand and verify it by opening it in Unity.

---

## 5. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Unity YAML edge cases break round trips | High | Lossless design; corpus and fuzzing from P6 day one |
| A merge bug corrupts a scene | High | Graph validation; never silent; backup refs; Unity-in-the-loop tests |
| GitHub rejects custom refs or throttles presence pushes | Medium | Spike S1 done: custom refs accepted, Actions not triggered; per-user presence refs; retries with backoff |
| Lock level 2 locks too much and blocks the team | Medium | Preview, confirmation threshold, level 1 default, `--partial` |
| C# heuristics miss layer usages or type references | Medium | Limit rewrites to our own new lines; "needs review" list |
| Serialization differs between Unity versions | Medium | Unity version guard; fixtures per supported version |
| Windows file-system quirks (locking, case, long paths) | Medium | Windows in CI from P0; early dogfooding on Windows |

---

## 6. Definition of done (every feature)

- User-facing text in English, with a "what to do next" hint on every block.
- `--json` output where the command reports state.
- Unit tests plus fixture or harness tests.
- Passes CI on macOS, Windows and Linux.
- Never prompts on a non-TTY.
- DESIGN.md and WORKFLOW.md updated when behavior changes.

---

## 7. Next steps

1. P0: create the workspace and CI.
2. Run spikes S1–S4 and record the results in DESIGN.md.
3. Create the test Unity project repository.
4. Start P1.
