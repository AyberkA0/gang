# A Day with GANG

This walkthrough follows a four-person Unity team through one working day with GANG. It shows the commands they run, what GANG prints, and what GANG does behind the scenes.

> The output shown here is illustrative. Exact wording may change as GANG is built; the behavior is specified in [DESIGN.md](DESIGN.md).

---

## The team

| Person | Role | OS |
|---|---|---|
| **ayberk** | Gameplay programmer, team lead | macOS |
| **mehmet** | Level designer | Windows |
| **zeynep** | 2D/3D artist | Windows |
| **deniz** | UI programmer, joins the team today | Linux |

**Project:** `AyberkA0/forest-run`, Unity `6000.0.23f1`. Everyone works on `main`.

**Relevant project policy** (`.gang/config.toml`):

```toml
[locks]
auto_allowed = ["binary", "unity"]   # code is never auto-locked

[scenes]
lighting = "lock"                    # bake outputs need a bake lock
```

Prompts: `$` is macOS/Linux, `>` is Windows.

---

## 08:55 · Deniz joins the team

```
$ gang clone AyberkA0/forest-run
No GitHub credentials found.
! First copy your one-time code: 7C4D-91AF
  Open https://github.com/login/device and enter the code.
  Waiting for authorization... ✓ Logged in as deniz

Cloning AyberkA0/forest-run...           ✓
Fetching LFS objects (1.2 GB)...         ✓
Registering merge drivers and hooks...   ✓
Fetching team presence and locks...      ✓

Unity version: the project uses 6000.0.23f1
  ✗ Not installed on this machine (found 6000.0.19f1)
  Install 6000.0.23f1 with Unity Hub before opening the project.
```

Deniz installs the right version first. Opening the project with an older editor would have re-serialized hundreds of files and flooded everyone's next sync with changes nobody made.

**Behind the scenes:** Device Flow login and token stored in the keychain → `git clone` → `git lfs pull` → merge driver and hook registration → `gang credential` registered as git's credential helper → fetch of the team's presence refs (`refs/gang/presence/*`) and `refs/gang/locks`.

---

## 09:00 · Ayberk opens the team panel

```
$ gang
┌ forest-run · main ─────────────────────────────── synced 3s ago ┐
│ TEAM                                                            │
│  ● ayberk (you)  macOS    clean                                 │
│  ● mehmet        Windows  Level1.unity (modified)               │
│  ● zeynep        Windows  clean                                 │
│  ● deniz         Linux    clean                  joined 5m ago  │
│ LOCKS                                                           │
│  (none)                                                         │
└─────────────────────────────────────────────────────────────────┘
```

While the panel is open, Ayberk's state is published within seconds of every change, and teammates' states are read every 15 seconds.

---

## 09:20 · Zeynep locks a file nobody can merge

```
> gang lock Assets/Art/Characters/Hero.psd -m "armor repaint"
🔒 Locked Assets/Art/Characters/Hero.psd (+ .meta)
```

Within seconds every panel shows:

```
│ LOCKS                                                           │
│  🔒 Art/Characters/Hero.psd   zeynep   09:20   armor repaint     │
```

A `.psd` has no scripts, so the default lock level (1) locks just the file.

---

## 09:30 · Ayberk locks the Player object

Ayberk is about to rework the player and wants nobody else touching it — or the things that talk to it — until the rework is done. First, Ayberk looks at the object:

```
$ gang tree Assets/Scenes/Level1.unity::Player
Player                       &4821937561   PlayerController, PlayerHealth
├─ Model                     &4821937570
├─ WeaponMount               &4821937588   WeaponMount
│  └─ Rifle (prefab)         &4821937602
└─ GroundCheck               &4821937599
```

Then Ayberk locks it at level 2:

```
$ gang lock Assets/Scenes/Level1.unity::Player --level 2 -m "player rework"
Lock preview — level 2

  Object
    Level1.unity › Player                         + 4 descendants
  Scripts on Player and its descendants
    Assets/Scripts/Player/PlayerController.cs
    Assets/Scripts/Player/PlayerHealth.cs
    Assets/Scripts/Combat/WeaponMount.cs
  Nested prefabs
    Assets/Prefabs/Weapons/Rifle.prefab          + Assets/Scripts/Combat/Rifle.cs
  Interacting objects
    Level1.unity › Main Camera                    CameraFollow.target → Player
    Level1.unity › EnemySpawner                   EnemySpawner.player → Player
    Level1.unity › HUD/HealthBar                  PlayerHealth.onDamaged → HealthBar.Refresh()
    Level1.unity › Enemies/* (12 objects)         Enemy.cs uses PlayerHealth
    Assets/Prefabs/Bullet.prefab                  WeaponMount.bulletPrefab
  Scripts on interacting objects
    Assets/Scripts/Camera/CameraFollow.cs
    Assets/Scripts/Enemies/EnemySpawner.cs
    Assets/Scripts/Enemies/Enemy.cs
    Assets/Scripts/UI/HealthBar.cs
    Assets/Scripts/Combat/Bullet.cs

  Total: 16 objects, 2 prefabs, 9 scripts
Lock all 27 targets? [Y/n] y
🔒 Locked Level1.unity › Player (level 2, 27 targets)
```

What the levels mean here:

- **Level 1** would have stopped after *Nested prefabs*: the Player subtree, its four scripts and the Rifle prefab.
- **Level 2** adds everything that interacts with Player — through a serialized reference (Main Camera, EnemySpawner, the HealthBar UnityEvent), through code (`Enemy.cs` uses the `PlayerHealth` type, so the 12 enemies in this scene interact with Player), or through an asset reference (`Bullet.prefab`) — plus their scripts. It does not follow interactions any further.

The rest of Level1 stays open: others can still add trees, move the light, or add new enemies under `Enemies`.

---

## 09:40 · Two people, one scene

- **Mehmet** adds six trees and two enemies to Level1, moves the `SpawnPoint`, and lowers the Directional Light intensity to 0.8 for an evening mood.
- **Ayberk** reworks Player (jump height, position) and raises the light to 1.2 to see better while testing.

When Ayberk saves the scene, the panel says:

```
⚠ mehmet is also changing Assets/Scenes/Level1.unity
  Scene files merge automatically. You'll only be asked if you both change the same property.
```

A yellow notice, not a block. Both keep working.

While placing the new enemies, Mehmet accidentally drags Player. `gang status` catches it:

```
> gang status
On main · up to date with origin/main
Changes:
  M Assets/Scenes/Level1.unity
    ⚠ Player is locked by ayberk ("player rework") — you changed Player › Transform › Position
      Undo only that object: gang restore Assets/Scenes/Level1.unity::Player

> gang restore Assets/Scenes/Level1.unity::Player
✓ Restored Level1.unity › Player (1 change). Your other changes in Level1.unity are kept.
⚠ Unity is open: let it reload Level1 before you keep editing.
```

---

## 10:30 · Mehmet sends the forest work

```
> gang save -m "Add forest section and two enemies"
✓ Checks passed (meta files, file names, Unity version, locks)
✓ Committed 3 files

> gang sync
✓ Up to date with origin/main
✓ Pushed 1 commit
```

---

## 10:35 · Ayberk syncs — the first conflict

```
$ gang save -m "Tune jump"
✓ Checks passed (meta files, file names, Unity version, locks)
✓ Committed 1 file

$ gang sync
Pulling 1 commit from mehmet...
Merging Assets/Scenes/Level1.unity
  ✓ 11 changes merged automatically
  ✗ 1 conflict

1 conflict found. Resolve it now? [Y/n] y

Level1.unity · conflict 1/1
──────────────────────────────────────────────────────────
Directional Light › Light › Intensity

  base     1
  you      1.2    ayberk · today 10:20 · "Tune jump"
  mehmet   0.8    mehmet · today 10:28 · "Add forest section and two enemies"

Why: You both changed the intensity of Directional Light.

[m] mine   [t] theirs   [b] both (1 +0.2 −0.2 = 1)
[o] original   [e] edit   [?] details   [u] undo
> t

✓ All conflicts resolved. Merge committed.
✓ Pushed 2 commits

Merged automatically from mehmet:
  Level1.unity: added 6 trees and 2 enemies, moved SpawnPoint
```

The trees, the enemies, the spawn point and Ayberk's jump tuning merged silently. Only the one property both of them changed was asked about, and Ayberk took Mehmet's value because lighting is Mehmet's call.

**Behind the scenes:** `git fetch` → pre-merge passes (nothing to do) → `git merge origin/main`. Git calls `gang merge-driver` for `Level1.unity`; the driver writes a valid scene with a default for the conflicting property and records the conflict in `.git/gang/conflicts.json`. Bang applies the answer → `git add` → `git commit` → `git push` → presence updated.

---

## 11:10 · Deniz is saved from a cross-platform bug

On Linux, Deniz creates `Assets/Ui/MainMenu.prefab` — but the project already has `Assets/UI/`. On Linux those are two folders; on macOS and Windows they are the same one.

```
$ gang save -m "Main menu prefab"
✗ Assets/Ui/MainMenu.prefab: path differs only in case from existing Assets/UI/
  On macOS and Windows these are the same folder; this would break the project there.
  Fix: gang mv Assets/Ui/MainMenu.prefab Assets/UI/MainMenu.prefab

$ gang mv Assets/Ui/MainMenu.prefab Assets/UI/MainMenu.prefab
✓ Moved Assets/Ui/MainMenu.prefab → Assets/UI/MainMenu.prefab (+ .meta)

$ gang save -m "Main menu prefab"
✓ Checks passed (meta files, file names, Unity version, locks)
✓ Committed 2 files
```

Meanwhile, the `.directory` files Deniz's desktop environment drops into folders never show up anywhere: the managed `.gitignore` already covers Linux, macOS and Windows junk.

---

## 11:30 · A layer conflict resolves itself

At 11:02, Ayberk named layer 8 `Pickup` and pushed. Not knowing that, Deniz named layer 8 `UI3D`, put three menu objects on it, set the UI camera's culling mask to it, and wrote `1 << 8` in `MenuCamera.cs`.

```
$ gang sync
Layer conflict on layer 8: yours "UI3D", origin/main "Pickup" (ayberk, 11:02)
  → Moving "UI3D" to layer 9 and updating everything that uses it:
    ✓ ProjectSettings/TagManager.asset          layer 8 → 9
    ✓ ProjectSettings/DynamicsManager.asset     collision matrix row/column 8 → 9
    ✓ Assets/Prefabs/UI/MainMenu.prefab         3 objects (Layer)
    ✓ Assets/Scenes/Menu.unity                  UI Camera culling mask
    ✓ Assets/Scripts/UI/MenuCamera.cs:42        1 << 8  →  1 << 9
    ⚠ Assets/Scripts/UI/MenuRaycaster.cs:17     literal 8 next to "layer" — please review
  ✓ Committed "gang: move layer UI3D from 8 to 9"
Pulling 2 commits from ayberk...
  ✓ Merged without conflicts
✓ Pushed 3 commits
```

No question was asked. Whoever pushed first keeps the index; GANG moved Deniz's layer to the first free one, rewrote every use it could recognize, and recorded it all in a commit:

```
$ gang log -1 --format=%B HEAD^1
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

Deniz opens `MenuRaycaster.cs:17` and replaces the hard-coded number with `LayerMask.NameToLayer("UI3D")`, so it can never drift again.

---

## 12:00 · Mehmet takes a bake lock

```
> gang lock Assets/Scenes/Level1.unity --bake -m "lighting pass"
🔒 Locked bake data for Level1.unity:
    Assets/Scenes/Level1/                     lightmaps, LightingData, NavMesh
    Level1.unity › lighting and NavMesh settings
  The rest of Level1.unity stays editable.
```

Ayberk keeps working on Player in Level1 without interruption — only the bake outputs and the scene's lighting and NavMesh settings are locked. If anyone else tried to commit new lightmaps for Level1, the commit would be blocked.

---

## 13:30 · Zeynep tries to sync with Unity open

```
> gang sync
✗ Unity is open, and this sync would change 2 scene/prefab files:
    Assets/Scenes/Level1.unity
    Assets/Prefabs/Hero.prefab
  Save your work and close Unity, then run `gang sync` again.
  (Use --force to sync anyway.)
```

Zeynep saves, closes Unity and syncs again without trouble. Without this check, pressing Ctrl+S in an editor still holding the old Level1 would have overwritten the morning's merged work by Ayberk and Mehmet.

---

## 14:00 · Zeynep finishes the binary work

```
> gang save -m "Hero armor repaint" --push
✓ Checks passed (meta files, file names, Unity version, locks)
✓ Committed 2 files (Hero.psd via LFS, 48 MB)
✓ Pushed 1 commit

You pushed changes to a locked file: Assets/Art/Characters/Hero.psd
Release your lock? [Y/n] y
🔓 Released Assets/Art/Characters/Hero.psd
```

---

## 15:00 · Ayberk auto-locks the pickup work

Ayberk starts on coin pickups and wants that unpushed work protected:

```
$ gang lock --auto
Unpushed changes in 5 files:
  🔒 Assets/Prefabs/Pickups/Coin.prefab          modified
  🔒 Assets/Art/Pickups/coin_icon.png            added
  ·  Assets/Scenes/Level1.unity                  skipped — mehmet is also changing it
  ·  Assets/Scripts/Pickups/Pickup.cs            skipped — code merges automatically
  ·  Assets/Scripts/Pickups/PickupSpawner.cs     skipped — code merges automatically

Locked 2 files (auto, level 0). They will be released when you push.
```

- Code is skipped because the project's `auto_allowed` policy is `binary` and `unity`: git merges code, so locking it would only make teammates wait.
- Level1 is skipped because Mehmet is actively changing it — an auto lock never grabs shared work.

At 15:20 Deniz spots a typo in `Pickup.cs`, fixes it and pushes. Nothing stops Deniz, and nothing needs to.

---

## 16:10 · Ayberk pushes

```
$ gang save -m "Coin pickups and player rework"
✓ Checks passed (meta files, file names, Unity version, locks)
✓ Committed 6 files

$ gang sync
Pulling 2 commits from deniz, mehmet...
  ✓ Assets/Scripts/Pickups/Pickup.cs merged automatically (text)
  ✓ Level1.unity: 4 changes merged automatically
✓ Pushed 2 commits
🔓 Released 2 auto locks (Coin.prefab, coin_icon.png)

You pushed changes to locked targets: Level1.unity › Player (level 2)
Release your lock? [Y/n] y
🔓 Released Level1.unity › Player and 26 derived targets
```

Auto locks are released silently on push. The manual Player lock asks first, because Ayberk might not be done yet.

---

## 16:30 · A deleted object that someone else changed

In Level2, Mehmet deleted the old bridge (`OldBridge`). At the same time, Ayberk added a `BridgeTrigger` child under it.

```
$ gang sync
Pulling 1 commit from mehmet...
Merging Assets/Scenes/Level2.unity

Level2.unity · conflict 1/1
──────────────────────────────────────────────────────────
Environment › OldBridge

Why: mehmet deleted OldBridge. You added a child, BridgeTrigger, under it.

[d] delete (BridgeTrigger is deleted too)
[k] keep OldBridge with BridgeTrigger
[r] keep only BridgeTrigger, move it to OldBridge's parent (Environment)
[?] details   [u] undo
> r

✓ BridgeTrigger moved to Environment. OldBridge stays deleted.
✓ All conflicts resolved. Merge committed.
✓ Pushed 2 commits
```

Plain git would have merged this "successfully" and left `BridgeTrigger` pointing at a parent that no longer exists.

---

## 17:30 · End of day

Ayberk opened Level3 earlier just to look at it:

```
$ gang status
On main · up to date with origin/main
Changes:
  M Assets/Scenes/Level3.unity   noise only (RenderSettings, 2 RectTransforms)
Run `gang restore --noise` to discard noise-only changes.

$ gang restore --noise
✓ Restored 1 file
```

A last look at the panel before leaving:

```
┌ forest-run · main ─────────────────────────────── synced 2s ago ┐
│ TEAM                                                            │
│  ● ayberk (you)  macOS    clean                                 │
│  ● mehmet        Windows  clean                                 │
│  ○ zeynep        Windows  offline 20m · clean                   │
│  ● deniz         Linux    2 unpushed commits (UI/MainMenu…)     │
│ LOCKS                                                           │
│  🔒 Scenes/Level1 (bake)   mehmet   12:00   lighting pass        │
└─────────────────────────────────────────────────────────────────┘
```

Two things stand out: Deniz has two commits that never got pushed, and Mehmet's bake lock is still held. Ayberk pings both; Deniz runs `gang sync`, Mehmet runs `gang unlock Assets/Scenes/Level1.unity --bake`.

---

## What GANG did for this team today

| | |
|---|---|
| **Questions asked** | 2 (light intensity, deleted bridge) |
| **Merged automatically** | Every other change in Level1 and Level2, `Pickup.cs`, and the `TagManager` after the layer remap |
| **Fixed automatically** | Layer 8 collision — layer moved, 3 objects, a camera mask, a collision matrix and a line of code updated, and committed |
| **Prevented** | Wrong Unity version, a case-only folder clash, a change to a locked object, an editor overwriting a synced scene, a dangling child in a deleted object, a noise-only commit |
| **Broken scenes** | 0 |

## Commands used

| Command | Used for |
|---|---|
| `gang clone owner/repo` | Joining a project (with GitHub login) |
| `gang` | Team panel |
| `gang tree <file>::<object>` | Finding an object's path |
| `gang lock <target> [--level n] [-m reason]` | Locking a file or an object |
| `gang lock <scene> --bake` | Locking lighting and NavMesh bake data |
| `gang lock --auto` | Locking your own unpushed work |
| `gang unlock <target>` | Releasing a lock |
| `gang status` | Your changes, with team and lock warnings |
| `gang restore <file>::<object>` | Undoing changes to one object |
| `gang restore --noise` | Dropping changes nobody made on purpose |
| `gang save -m msg [--push]` | Checked commit |
| `gang sync` | Pull, merge, answer questions, push |
| `gang mv` | Moving or renaming an asset with its `.meta` |
| `gang log` | History (any git command works through `gang`) |
