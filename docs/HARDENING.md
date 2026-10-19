# GANG — Hardening Plan after the Field Test

> Status: v0.1 · 2026-10-19
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
| L5 | **Collision matrices** (`DynamicsManager`, `Physics2DSettings`) | Not rewritten, only listed for review, and only when our commits changed them. The moved layer keeps default collisions; the settings we made for it now apply to the other side's layer. The merge summary shows the matrix as two 256-character hex strings. | Code reading + field test (§D3) |
| L6 (**fixed**) | **Layer numbers in C# that are not `1 << n` or `xLayer = n`** (`LayerMask m = 256;`, `const int Interact = 8;`, `Physics.Raycast(..., 256)`) | Were not changed, and only reported when the line contained the word "layer"; a probe showed masks, constants, `Physics.*` arguments, multi-line enums and locals skipped without a word. Now the recognized forms are rewritten and every other occurrence on our added lines is listed (see the plan). | Code reading, probe (`gang-csharp::rewrite_layer_number`) |

| L7 | **No free layer left** | The move is skipped without a word; the clash falls through to L3. | Code reading |
| L8 (**fixed**) | **Review list hidden** | Field test: the review list was only in the commit message, nothing on screen, not even "nothing to review". | Field test (C8) |
| L9 | **Other indexed name tables** — sorting layers, rendering layers (URP/HDRP), NavMesh areas | Not considered at all. Sorting layers are referenced by `uniqueID` and are probably safe; rendering layers and NavMesh areas are indexed like physics layers and have the same problem. | Not verified; needs a Unity check |

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

### 1.3 Conflict markers can still reach a Unity file

DESIGN G2 promises GANG never writes conflict markers into a Unity file. The merge driver falls back to `git merge-file` — which writes markers — when a file is not UTF-8, when the repository cannot be found, or when `driver_merge` returns `None` (the parser rejected one of the three versions). **Plan (P1):** for Unity YAML, never fall back to a text merge; keep *ours* in the working tree, record a whole-file conflict and let `gang bang` ask mine / theirs / save theirs to compare.

### 1.4 The test's shape hid other risks

| Area | Why the test could not see it | Plan |
|---|---|---|
| **Same asset added by both sides** (`Assets/Art/Logo.png` with two GUIDs) | Folder `.meta` add/add was seen (C1); asset `.meta` add/add with references was not. Picking one GUID breaks every reference to the other. | Detect: GUID differs on add/add → keep the GUID with references on both sides; if both are referenced, rewrite the other side's references (we have the GUID index). P1 |
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
| A1 | Rebuilt prefab (new fileIDs) silently breaks instance overrides | P0 | Planned: warning in diff/save/merge (E2) |
| A4 | Re-parent keeps the local, not the world position | P1 | **Fixed in this round** (position, rotation, scale) |
| A8 | `index.lock` race between the background publisher and the next command | P1 | **Fixed in this round** (`GIT_OPTIONAL_LOCKS=0`) |
| A9 | `bang --reset` keeps answers; an interrupted merge cannot be finished | P1 | **Fixed in this round**: `--reset` also forgets remembered answers; `sync` and `bang` finish a merge left open |
| A5 | `gang who` shows a teammate's in-progress merge as their own work | P1 | **Fixed in this round** (also: unmerged files were dropped from presence as "noise") |
| A6 | Empty-folder `.meta`: warning for the author, block for the merger | P1 | **Fixed in this round**: decided by the `.meta` content, not the disk |
| A7 | `unlock` of a binary with unpushed changes does not warn | P1 | **Fixed in this round**: asks, `--yes` skips |
| C1 | Folder `.meta` add/add asks an empty question | P2 | **Fixed in this round** (the upstream GUID is kept); asset add/add (§1.4) still planned |
| C2 | `sync` refuses because of one untracked file | P2 | **Fixed in this round** (untracked files; modified tracked files still need a save) |
| D6 | `gang save` accepts only one `-m` | P3 | **Fixed in this round** |
| C3 | `lock --auto` skips the work it should protect; `--dry-run` silent for objects | P2 | Planned: object-level auto locks |
| C4–C6 | Scene rename gaps, bake lock coverage, `mv` into a missing folder | P2 | Planned |
| C7 | No GANG summary when the push fails after a merge | P2 | Planned (with the LFS message above) |
| C8 | Layer review list not shown | P2 | Planned (L8) |
| D1–D6 | Float noise in "both", raw prefab/matrix diffs, built-in component names, labels, multiple `-m` | P3 | Planned |
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
