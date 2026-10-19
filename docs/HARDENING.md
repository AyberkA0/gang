# GANG — Hardening Plan after the Field Test

> Status: v0.2 · 2026-10-19 (second round: §5)
> Inputs: the two-person *coop-doom* field test (Unity 6, Windows, real GitHub, 101 commits, 19 merges) and a code review of the areas that test did not reach. Companion to [PLAN.md](PLAN.md) (milestones M1–M5) and [DESIGN.md](DESIGN.md).

The field test showed that the core idea works: no merge broke a scene. It also found four ways GANG can lose data silently, one blocker (LFS), and a list of flow problems. This document does two things:

1. **Looks for what the test could not see.** The test was planned around one branch, two people, merge-only syncs and conflicts set up on purpose. §1 lists the weaknesses that this shape hides, starting with the layer remap, which the test counted as its "most magical" moment.
2. **Turns everything into a plan** with priorities (§3) and an order of work (§4).

Priorities: **P0** silent data loss or a blocker · **P1** wrong result or serious friction · **P2** confusing · **P3** cosmetic.

---

## 1. Blind spots of the field test

### 1.1 The layer remap is only correct for the case the test tried

The test tried exactly one shape: two people, one branch, each adds a *new* layer at the same empty index, the second pusher moves. The remap (`gang-core/src/layers.rs`) handles that well. Every other shape was untested, and reading the code shows several that break layers **silently** — the sync ends with "✓ Pushed", and Unity has nothing to complain about because a wrong layer is still a valid layer.

| # | Case | What happens today | Verified |
|---|---|---|---|
| L1 | **Both renamed the same, already used layer** (base `8 = Enemy`, one side `Enemies`, the other `Foes`) | Treated as two different layers: `Foes` moves to 9, and only the objects *we changed* move with it. Objects already on layer 8 stay. One layer is split in two. | Reproduced by `renaming_the_same_layer_twice_does_not_split_it` — **fixed** |
| L2 | **The moved layer was already shared** (a feature branch, or a 3+ person team) | Teammates who already used the old number have it in their merge base, so `plan()` skips the index (`oi == bi`). Their new objects on 8 silently become the other layer. The `Gang-Remap:` trailer the remap writes is never read by anyone. | Reproduced by `a_moved_layer_follows_teammates_work_in_progress` — **fixed** |
| L3 | **Merging without `gang sync` / `gang merge`** (`gang pull`, `gang rebase`, `gang cherry-pick`, plain git) | The pre-merge pass did not run. `TagManager` merges index by index, so the clash became a `gang bang` question about a *name*; whichever name is picked, the other side's objects keep the number and silently change meaning. | Reproduced by `layer_clash_is_remapped_by_{pull,pull_rebase,rebase,sync_rebase,cherry_pick}` and `layer_clash_through_plain_git_explains_itself` — **fixed** for the GANG commands; plain git gets an explained question |
| L4 (**fixed**) | **"Everything except X" masks** (a camera culling all but UI: every bit except one set) | `move_bit` clears bit 8 and sets bit 9. Bit 9 was already set, so the result is "everything except X **and except the other side's layer**". | Code reading |
| L5 (**fixed**) | **Collision matrices** (`DynamicsManager`, `Physics2DSettings`) | Not rewritten, only listed for review, and only when our commits changed them. The moved layer keeps default collisions; the settings we made for it now apply to the other side's layer. The merge summary shows the matrix as two 256-character hex strings. | Code reading + field test (§D3) |
| L6 (**fixed**) | **Layer numbers in C# that are not `1 << n` or `xLayer = n`** (`LayerMask m = 256;`, `const int Interact = 8;`, `Physics.Raycast(..., 256)`) | Were not changed, and only reported when the line contained the word "layer"; a probe showed masks, constants, `Physics.*` arguments, multi-line enums and locals skipped without a word. Now the recognized forms are rewritten and every other occurrence on our added lines is listed (see the plan). | Code reading, probe (`gang-csharp::rewrite_layer_number`) |

| L7 (**fixed**) | **No free layer left** | The move was skipped without a word; the clash fell through to L3. Now the sync stops before merging and says to free a layer. | `a_clash_with_no_free_layer_stops_and_says_why` |
| L8 (**fixed**) | **Review list hidden** | Field test: the review list was only in the commit message, nothing on screen, not even "nothing to review". | Field test (C8) |
| L9 (**mitigated**) | **Other indexed name tables** — sorting layers, rendering layers (URP/HDRP), NavMesh areas | Sorting layers are referenced by `uniqueID` and are safe. Rendering layers (`TagManager.m_RenderingLayers`) and NavMesh areas (`NavMeshAreas.asset`) are indexed like physics layers (checked in a Unity 6 project): they are now **reserved** like layers (§5.2), so a clash is caught before it is shared; one that still reaches `gang bang` explains what picking a name means. They are not moved automatically. | `store::tests::*_clashes_are_explained`, Unity 6 project files |

**Plan (P0 for L1–L3, P1 for L4–L8, P2 for L9):**
- L1: a clash is only a clash when the base index was **empty** and both sides filled it. When the base had a name and both renamed it, it is a rename conflict: ask which name, never move.
- L2: write a machine-readable remap record (keep the `Gang-Remap:` trailer, and also store it in the shared `refs/gang/locks` table or a new `refs/gang/remaps` ref). When a sync brings in a remap commit that our unpushed commits predate, apply the same move to *our* commits' changes before merging. Test: the three-person branch scenario above.
- L3 (**done**): the layer pass runs from `pull` (merge and `--rebase`), `rebase [<upstream>]` and `cherry-pick` through the wrappers, as it did for `sync` and `merge`. For a rebase it records the moves so the merge driver replays our TagManager with the layer at its new number (otherwise every replayed commit clashes again; `sync --rebase` had the same gap). A TagManager index clash that reaches `gang bang` anyway (plain git) explains that either name moves the other side's objects and suggests aborting and `gang sync`. Not covered: `revert`, `rebase --onto`/`--root`, `merge --squash`.
- L4: only rewrite masks whose bit `to` is clear; anything else goes to the review list with a plain-language line ("camera Main Camera: the culling mask also covered layer 9; check it").
- L5: rewrite collision matrices (move row and column `from` → `to`, both 3D and 2D), and render them as "Projectile × Projectile: off" in diffs and merge summaries.
- L6 (**done**): `gang-csharp` now tokenizes the script, so context can span lines. **Rewritten** on the lines our commits added: `.layer = n` / `==` / `!=`, `n == x.layer`, `case n:` in a `switch` on a layer, `1 << n` (also `1u`, `1L`), names that say "layer" (`UI3DLayer = n`; not `player`, sorting or rendering layers, NavMesh areas or counts), members of enums named for layers, the layer arguments of `Physics.IgnoreLayerCollision` / `GetIgnoreLayerCollision` and `Physics2D.Get/SetLayerCollisionMask`, `LayerMask.LayerToName(n)`, and `const` / `static readonly` numbers and enum members anywhere in the file whose name is used as a layer. Single-bit masks (`256`, `0x100`, `0b…`, in the same base) are rewritten when assigned to a `LayerMask` or a name containing "mask", passed as `layerMask:`, passed to a `Physics.*` / `Physics2D.*` call right after a distance (`100f`, `Mathf.Infinity`, `maxDistance`) or written in hex there, and in constants used as masks. **Listed under "Needs review"**: every other whole number `n` or `1 << n` (any base) in code on those lines, whatever the line says (`int l = 8; go.layer = l;` says that `l` is used as a layer); masks with more bits that include the layer, where they are used as masks or written in hex/binary; members of layer enums that are layer `n` by their position. Strings, comments and preprocessor lines are skipped. `gang doctor` lists scripts under `Assets/` that use user layers by number and suggests `LayerMask.NameToLayer("…")` / `LayerMask.GetMask("…")`. Tests: `gang-csharp` unit tests, `layer_numbers_in_code_move_or_are_listed` (sync), `doctor_finds_and_fixes_problems`.
  **Hard limits** (not solvable by reading added lines): values computed at run time (`baseLayer + 1`, `1 << (i + 8)`, masks built in loops) show up only when a literal `n` or `1 << n` is on an added line; numbers in compiled DLLs and packages; layers that arrive as data at run time (JSON, ScriptableObjects, save files, server config, `PlayerPrefs`); numbers in lines our commits did not add (a teammate's or older code that already used `n` for the other layer is theirs by then); constants declared in another file (only same-file use is linked); and names that hide their purpose (`const int X = 8` never used as a layer in the same file is listed, not changed). Review entries can be noisy (`var count = 8` is listed), by design: nothing that may be the layer is dropped silently. The only complete fix is name-based layer calls, which `gang doctor` recommends.

- L7/L8: always print the review list in the sync output, or "Nothing needs review."; say clearly when no free layer is left.
- L9: check in Unity 6 where rendering layers and NavMesh areas are stored; if indexed, generalise the remap to a table of "indexed name lists".

### 1.2 Rebase is a second-class citizen

`gang sync --rebase`, `sync.strategy = "rebase"` and `gang pull --rebase` are offered, but the test only merged. During a rebase git swaps the sides: *ours* is the upstream commit and *theirs* is the user's own commit being replayed. GANG's merge driver and `gang bang` labelled *ours* as "you" and named *theirs* after `REBASE_HEAD`'s author — the user. **On every rebase conflict, the screen called the teammate's value "you" and "mine" picked the teammate's value.**

**Fixed in this round (P0):** `Repo::rebasing` detects `rebase-merge`, or `rebase-apply` with `rebasing` (not `git am`). During a rebase the merge driver swaps git's versions before the semantic merge, so the stored versions, "you", "mine" and the delete/modify wording are the user's side; the other side is named after the author of the commit the rebase goes onto. `gang bang` swaps text hunks and uses `--theirs` / stage 2 for "mine" on text and binary files. Regression tests: `same_property_when_rebasing_mine_is_still_mine` (`gang sync --rebase`), `same_property_when_rebasing_theirs_is_the_teammates` (`sync.strategy = "rebase"`), `code_conflicts_when_rebasing_keep_mine`, `deleted_object_when_rebasing_names_the_teammate` (plain `gang rebase`). The remaining `sync.rs` conflict tests (A2, A5, A9 flows) have no rebase variant yet.

### 1.3 Conflict markers can still reach a Unity file (**fixed**)

DESIGN G2 promises GANG never writes conflict markers into a Unity file. The merge driver falls back to `git merge-file` — which writes markers — when a file is not UTF-8, when the repository cannot be found, or when `driver_merge` returns `None` (the parser rejected one of the three versions). **Plan (P1):** for Unity YAML, never fall back to a text merge; keep *ours* in the working tree, record a whole-file conflict and let `gang bang` ask mine / theirs / save theirs to compare.

**Done (round 2):** the line merge is kept only when it is clean and the result still parses when the inputs did; otherwise the file keeps the current side and `gang bang` asks for the whole file. Test: `unreadable_unity_files_never_get_conflict_markers`.

### 1.4 The test's shape hid other risks

| Area | Why the test could not see it | Plan |
|---|---|---|
| **Same asset added by both sides** (`Assets/Art/Logo.png` with two GUIDs) | Folder `.meta` add/add was seen (C1); asset `.meta` add/add with references was not. Picking one GUID breaks every reference to the other. | **Done (round 2):** the shared GUID is kept without a question and our references follow it after the merge. Test: `an_asset_added_twice_keeps_one_guid_and_every_reference` |
| **More than two people** | Presence, locks and remaps were only exercised by two clones. L2 is a direct consequence. | Multi-user harness with 4 clones in CI (PLAN P5's "done when"); one 3-person field test day. P1 |
| **Long-lived branches** | Everything was on `main`. | Branch scenarios in the harness: remap (L2), lock enforcement across branches, presence per branch. P1 |
| **Interrupted operations** | A9 showed that a merge interrupted at the commit question cannot be finished. The same class of problem exists for a push that fails after the merge (C7) and for Ctrl+C during `gang bang`. | One "finish what is open" function used by `sync`, `bang` and `status`. P1 |
| **Large scenes / many files** | 2 scenes, 13 prefabs. | Benchmarks (`criterion`) on the 11,000-file corpus; time `status`, presence and the merge driver. P2 |
| **People who bypass GANG** | Agents never used plain git. | Plain `git commit` already runs the hooks; add a test that plain `git pull` with a layer clash is caught (L3), and that plain `git push` of a locked file is stopped by pre-push. P2 |
| **"The merge is right" ≠ "the game works"** | The test validated references, not gameplay (§6 of the test). | Opt-in Unity batch smoke check (E3/E4). P2 |

---

## 2. Field-test findings

Mapped from `GANG-NEXT.md` (sections A–D). Status is as of this document.

| ID | Finding | Pri | Status |
|---|---|---|---|
| B1 | LFS always on and cannot be turned off; raw error on quota | P0 | **Fixed in this round**: LFS is off by default (`[lfs] enabled = false`), the managed `.gitattributes`, `init`, `clone` and `doctor` follow the setting, and a push that fails on LFS quota gets a GANG message |
| A2 | A lock-refused `save` stages everything and closes an open conflict | P0 | **Fixed in this round** |
| A3 | No "both" for lists where both sides appended; details show "[10 items]" | P0 | **Fixed in this round**: append-only on both sides merges without a question |
| A1 | Rebuilt prefab (new fileIDs) silently breaks instance overrides | P0 | **Fixed in round 2**: old ids are matched to new ones by place and references are repointed at save and after a merge (DESIGN §13.9) |
| A4 | Re-parent keeps the local, not the world position | P1 | **Fixed in this round** (position, rotation, scale) |
| A8 | `index.lock` race between the background publisher and the next command | P1 | **Fixed in this round** (`GIT_OPTIONAL_LOCKS=0`) |
| A9 | `bang --reset` keeps answers; an interrupted merge cannot be finished | P1 | **Fixed in this round**: `--reset` also forgets remembered answers; `sync` and `bang` finish a merge left open |
| A5 | `gang who` shows a teammate's in-progress merge as their own work | P1 | **Fixed in this round** (also: unmerged files were dropped from presence as "noise") |
| A6 | Empty-folder `.meta`: warning for the author, block for the merger | P1 | **Fixed in this round**: decided by the `.meta` content, not the disk |
| A7 | `unlock` of a binary with unpushed changes does not warn | P1 | **Fixed in this round**: asks, `--yes` skips |
| C1 | Folder `.meta` add/add asks an empty question | P2 | **Fixed in this round** (the upstream GUID is kept); asset add/add (§1.4) still planned |
| C2 | `sync` refuses because of one untracked file | P2 | **Fixed in this round** (untracked files; modified tracked files still need a save) |
| D6 | `gang save` accepts only one `-m` | P3 | **Fixed in this round** |
| C3 | `lock --auto` skips the work it should protect; `--dry-run` silent for objects | P2 | **Fixed in round 2**: the objects we changed in a shared scene are auto-locked |
| C4 | Scene rename gaps | P2 | **Fixed in round 2**: the bake folder moves with the scene, Build Settings follow, exact renames need no bake lock |
| C5 | Bake lock does not cover NavMeshSurface | P2 | **Fixed in round 2**: NavMeshSurface components and their data are part of the bake lock |
| C6 | `mv` into a missing folder | P2 | **Fixed in round 2**: the folder is created with its `.meta` |
| C7 | No GANG summary when the push fails after a merge | P2 | **Fixed** (`push_failed`) |
| C8 | Layer review list not shown | P2 | **Fixed** (L8) |
| D1–D5 | Float noise in "both", raw prefab/matrix diffs, built-in component names, labels | P3 | **Fixed in round 2** |
| F1–F7 | OAuth App, CI billing, releases, pty tests branch, Unity golden fixtures, macOS/Linux use, packaging | — | Need the owner (F1–F3) or follow the fixes (F4–F7) |

---

## 3. Work streams

### H1 · Stop silent data loss — P0
A2, A3, A1 (warning first), B1, L1–L3, rebase sides (§1.2).
**Done when:** every item has a regression test built from its field-test scenario; the two layer tests in `crates/gang/tests/sync.rs` pass; each `sync.rs` conflict test also runs with `--rebase`.

### H2 · Correct results — P1
A4, A8, L4–L8, §1.3 (no text fallback for Unity files), asset `.meta` add/add (§1.4), A7.
**Done when:** collision matrices move with the layer; a corpus property test confirms no merge path writes `<<<<<<<` into a Unity file.

### H3 · Flow — P1/P2
A9 + "finish what is open", A5, A6, C1, C2, C3, C7, E5 (one-screen bang).
**Done when:** the field test's 7-question day can be answered with one `gang bang` run per sync; an interrupted merge, bang or push is always finished by the next `gang sync`.

### H4 · Readability — P3
D1–D6, matrix and prefab-override rendering, built-in component names.

### H5 · Infrastructure (owner) — F1–F7
OAuth App registration, Actions billing, first release, merge the pty-test branch (as a plain merge on `main`, the branch itself is not pushed), Unity golden fixtures from the coop-doom history, a macOS + Windows test day.

### H6 · Beyond the merge — E1–E4, E6–E8
Free binary storage options (LFS off + size guards → partial clone → own LFS endpoint/R2), prefab identity guard, team-contract checks and an opt-in Unity smoke test, rename impact analysis, presence notes, per-author merge summaries.

---

## 4. Order of work

1. **Done in this round:** B1, A2, A3, A4, A5, A6, A7, A8, A9, C1, C2, D6, L1, L2, L4, L8 — each with a regression test.
2. **Done next:** L3 + rebase sides (§1.2) — both were "the clash goes through a path the test never used". Left: rebase variants of the A2/A5/A9 flow tests.
3. L5–L7, L9; C3–C8.
4. A1/E2 prefab identity warning; §1.3; asset add/add.
5. H4, then the second field test (3 people, one on macOS, alternating who pushes first, one feature branch, one rebase day, gameplay smoke check).

---

## 5. Second round (2026-10-19)

The owner's brief for this round: **ask every conflict one by one**, find layer numbers in code as precisely as a compiler can, keep GANG free (git and GitHub only), put the least work on the team for the deepest control, and look for what is still missing by thought experiments.

### 5.1 `gang bang` asks every conflict on its own

There is no longer any way to answer several conflicts at once: `--prefer mine|theirs`, the `M`/`T` keys ("this whole object") and remembered answers applied without asking are gone. A remembered answer is offered under `Enter` on the question itself. Without a terminal, `gang bang --answer <choice>` answers the *next* open question only, after printing it (one hunk of a text file, one binary file). Test: `every_question_is_answered_on_its_own`; the pty test checks that `T` does nothing. The git side doors are closed too (R27, R28 below): `gang checkout/restore --ours/--theirs` during a merge and `-X ours|theirs` on merge, pull, rebase, cherry-pick and revert stop with a pointer to `gang bang`; `gang git …` remains plain git.

### 5.2 Layer numbers are reserved before anyone uses them (DESIGN §14.6)

The layer remap repairs a clash after the fact; reservations prevent it. A new physics layer, rendering layer or NavMesh area is reserved for its author on `refs/gang/claims` (compare-and-swap) by the background publisher after any GANG command and by `gang save`. The first claim wins; the second person learns it from `gang status` / `gang layers` within seconds, a plain commit is stopped, and `gang save` moves their layer before staging (Unity closed), with objects, masks, collision matrices and script numbers. A layer already on the default branch is left to the merge-time remap, so the two never disagree. On a branch, the layer reserved first keeps its number (L2 no longer arises). Tests: `crates/gang/tests/claims.rs`, `a_layer_reserved_on_a_branch_keeps_its_number`.

### 5.3 The C# compiler finds layer numbers (DESIGN §14.2)

An analyzer embedded in GANG is compiled on first use with the Roslyn compiler inside the installed Unity editor and run with the editor's .NET runtime (nothing downloaded). It compiles the scripts against Unity's assemblies, finds every value that reaches a layer index or mask (overloads and named/default arguments resolved like Unity's compiler), and traces each back through locals, fields, constants, enums and the project's own methods to the literal. On coop-doom it found `const int InteractableMask = 1 << 9` via `Physics.Raycast(layerMask) ← InteractableMask (1 << n)` and a layer passed through two locals into `IgnoreLayerCollision`, with 0 compile errors, in 8 s. The remap rewrites traced literals on our lines, moves serialized fields the compiler says hold a layer (instead of guessing from the field name), and the text rules still cover what the compiler did not see: the compiler only ever adds. `gang layers code` and `gang doctor` show its findings.

Remaining hard limits: data that arrives at run time, reflection, compiled DLL internals, Visual Scripting, animation events.

### 5.4 Thought experiments

Forty-eight team scenarios were walked through, then GANG's own new features were put through the same questions (R-rows) ("what would GANG do?"). Most were already handled (moves vs. edits, renames of scripts and objects, prefab variants, branch switching with Unity open, Windows path rules, append-only lists, sorting layers, package manifests…). These were not, and are now:

| # | Scenario | Before | Now |
|---|---|---|---|
| TE7 | A field renamed with `[FormerlySerializedAs]` on one side, its old key changed on the other (in a scene, a prefab, or an instance's override) | The teammate's change was dropped or became a meaningless question | Renames read from both sides' scripts; old keys and override paths are merged under their new names (DESIGN §13.10) |
| TE9 | A texture edited on one side, its folder moved on the other | — | Checked: git's `ort` merge follows the rename; nothing to do |
| TE24 | Scripting define symbols changed on both sides (`A;B;NETCODE` vs `B;DEBUG_AI`) | A value conflict | Merged as a set |
| TE27 | A pull request merged with GitHub's button | GitHub's line merge, without GANG's driver: valid YAML, wrong scene | `gang pr merge <n>` (DESIGN §18.4) |
| TE65 | A layer reserved on a feature branch, its author back on main | The reservation was released; a teammate could take the number | Kept while a local branch has the name |
| TE74 | A list with an insert at the front on one side and an edit at the end on the other | A whole-list question | diff3 on the items; the same stretch changed twice still asks |
| R3 | The merge-time layer remap with Unity open | The remap rewrote the TagManager under the editor, which would write its copy back | The layer pass waits for Unity to close (`--ignore-editor` goes ahead) |
| R7 | `gang pr merge` into a protected base branch | The push was refused | The merge goes to the pull request's branch; GitHub's button then keeps GANG's result |
| R11 | `gang pr merge` with Unity open | The working tree switched under the editor | Waits for Unity to close |
| R27 | `gang checkout --theirs .` during a merge | Every conflict in the named files settled at once, for one side | Refused while questions are open; `gang bang` asks them one by one |
| R28 | `gang merge -X theirs` (or `pull -Xours`, `rebase --strategy-option=theirs`) | git settled every text conflict for one side before GANG saw any | Refused; other `-X` options go through |
| R29 | Alice deletes a material while Bob puts it on a new object | A clean merge; Unity shows a Missing material | Reported after the merge, with who deleted it; on a terminal *Bring Red.mat back?* restores it with its texture and folder in a commit of its own; a commit deleting a used asset warns; `gang doctor` names the deleting commit (DESIGN §13.11) |
| R30 | `gang pull` / `gang merge` that merges cleanly | No reference checks (only `gang sync` and `gang bang` ran them) | The same checks as `gang sync`: rebuilt prefabs, shared GUIDs, deleted assets in use; also after a rebase finished in `gang bang` |
| R31 | The last asset of a folder deleted | The folder's `.meta` stayed; teammates' Unity deleted it, the next save here added a new one | `gang save` takes the empty folder along; a plain commit warns |
| R32 | `gang stash pop` onto teammates' changes | git's "ours" is the committed version: bang showed the teammate's value as "you", and "mine" took it; the stash stayed after every answer | "Mine" is the stash; the other side is "the committed version"; the stash is dropped once every conflict is answered (its id printed) |
| R33 | A conflict finished by hand in an editor, a marker left in a script or scene | Committed: Unity could not open the scene, or the team's scripts stopped compiling | The commit is blocked; markers that came in past the hooks are reported after the merge and by `gang doctor`, with the commit that brought them |
| R34 | A new material assigned in a scene, the scene committed, the material not (a path-limited save, plain git, an ignored folder) | Pushed: Missing for everyone else; DESIGN §16 promised a pre-push check that did not exist | The commit is blocked, naming the asset and `gang add` (or `add -f` when it is ignored) |
| R35 | A team that wants `Main.unity` edited by one person at a time sets `[scenes.policy]` | Parsed and ignored: anyone could change it, nothing was locked | `gang save` locks it when nobody holds it (released at the push), `gang status` mentions it, a commit without a lock is stopped; values other than `lock-required` / `merge` are refused |
| TE42 | A layer removed on one side while the other puts objects on it | Silent: the objects end up on a layer with no name | Reported during the sync; `gang doctor` lists objects on unnamed layers and undefined tags |
| A1 | A prefab rebuilt with new ids | Instances lost their overrides silently | References repointed (DESIGN §13.9) |
| §1.3 | A Unity file the parser rejects | Conflict markers in the file | Clean line merge or a whole-file question |
| §1.4 | One asset added by both sides | Either GUID breaks the other's references | Shared GUID kept, references follow |

### 5.5 Free storage

LFS stays off by default. `gang clone` now makes a **partial clone** (`--filter=blob:limit=5m`, free on GitHub): older versions of large files are fetched only when something needs them, which gives most of LFS's download savings at no cost. `gang doctor` reports the history's size against GitHub's limits and the files whose versions take the most room. Files over 100 MB are still refused (GitHub's hard limit).

A binary store on a side ref ("a GANG branch for binaries", with pointer files in the project branch) was considered. It would allow pruning old versions and splitting files over 100 MB, but it is LFS's design rebuilt on git objects: a clean/smudge filter, a pointer format, a pre-push step that must never lose an object, and GitHub's garbage collection of pruned objects on its own schedule. Partial clone gives the main benefit with none of that risk; the store stays a design option if a team outgrows it.

### 5.6 Still open

- Owner tasks (F1–F3): OAuth App, GitHub Actions billing, first release.
- A second field test: three people, one on macOS, alternating who pushes first, a feature branch, a rebase day, and a gameplay smoke check.
