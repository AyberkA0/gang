# GANG commands

Generated from `gang help <command>`. Every git command also works through `gang`; see [DESIGN.md §8](DESIGN.md#8-command-model) for what GANG adds to each.

## Overview

```text
gang — version control for Unity teams, built on git

Usage: gang [options] <command> [args]

GANG commands:
  (no command)     Open the team panel (inside a GANG repository)
  init             Set up a repository for GANG (or create one)
  clone <repo>     Clone a repository and set it up (owner/repo works)
  save [-m msg]    Add every change and commit it, after GANG's checks
  sync             Save, merge the team's work, answer conflicts, push
  bang             Answer quick questions about merge conflicts
  undo             Undo the last save, reset, restore or clean
  who [path]       Who on the team is changing what
  lock <target>    Lock files or objects (file::object, --level 0|1|2, --auto, --bake)
  unlock <target>  Release locks
  locks            Show the team's locks
  tree <file>      Show the objects of a scene or prefab
  ignore           Manage ignore rules shared by the team
  doctor [--fix]   Check the project's health
  auth             Sign in to GitHub (login, logout, status)
  git <args>       Run plain git, without GANG's additions
  version          Show GANG, git and git-lfs versions
  help [command]   Show help for a GANG or git command

Every other command is passed to git, so `gang status`, `gang log`,
`gang commit` and the rest work as they do in git.

Options (before the command):
  --raw            Show git's own output, without GANG's additions
  --json           Machine-readable output, where supported
  --ignore-editor  Run even while Unity has the project open
  -h, --help       Show this help
  -v, --version    Show the GANG version

git's global options (-C, -c, --no-pager, ...) are passed to git.
```

## gang init

```text
Usage: gang init [--github [--public] [--name <name>]] [git init options] [directory]
```

Sets up a repository for GANG (creating it with `git init` when needed): the managed `.gitignore` and `.gitattributes`, `.gang/config.toml`, merge drivers, hooks and Git LFS. Checks the Unity project's settings and offers to fix them. With `--github`, also creates the repository on GitHub (private unless `--public`) and connects it as `origin`.

## gang clone

```text
Usage: gang clone <url | owner/repo> [directory] [git clone options]
```

Clones and sets the clone up for GANG: drivers, hooks, LFS files, the team's presence and locks, and a check that the project's Unity version is installed.

## gang save

```text
Add every change and commit it, after GANG's checks

Usage: gang save [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  Only save these paths (default: everything)

Options:
  -m, --message <MESSAGE>  Commit message; several -m become paragraphs, as in git
      --push               Push after committing
  -h, --help               Print help
```

## gang sync

```text
Save your work, merge the team's, and push

Usage: gang sync [OPTIONS]

Options:
      --rebase             Rebase onto the team's work instead of merging
  -m, --message <MESSAGE>  Commit message for uncommitted changes (asked when missing)
  -h, --help               Print help
```

## gang bang

```text
Answer quick questions about merge conflicts

Usage: gang bang [OPTIONS] [PATH]

Arguments:
  [PATH]  Only this file

Options:
      --list             List the conflicts without asking
      --answer <CHOICE>  Answer the next open question only, without a terminal: mine, theirs, original, both, delete, keep, reparent or value=<text>. Every question is answered on its own; there is no way to answer them all at once
      --reset            Forget the answers given so far
  -h, --help             Print help
```

## gang undo

```text
Usage: gang undo
```

Undoes the last `gang save` (the commit is removed, its changes stay staged; refused once pushed), or brings back the files from before the last `reset --hard`, `restore`, `checkout -- <paths>` or `clean`, from the backup GANG took.

## gang who

```text
Usage: gang [--json] who [path]
```

Shows the team: who is online, which branch they are on, which files they are changing and how many commits they have not pushed. With a path, lists everyone changing that file or folder.

## gang tree

```text
Show the objects of a scene or prefab

Usage: gang tree [OPTIONS] <TARGET>

Arguments:
  <TARGET>  A scene or prefab, optionally with an object: Level1.unity::Environment/Player

Options:
      --depth <DEPTH>  How many levels to show
  -h, --help           Print help
```

## gang lock

```text
Lock files or objects so nobody else changes them

Usage: gang lock [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...  Files, folders/, "globs", or objects: Level1.unity::Environment/Player

Options:
      --level <LEVEL>        0: only the target; 1: + its scripts; 2: + what interacts with it
  -m, --message <MESSAGE>    Why, for the team
      --bake                 Lock a scene's lighting and NavMesh bake data
      --partial              Lock what is free when some targets are locked by others
      --dry-run              Show what would be locked
      --yes                  Do not ask for confirmation
      --auto                 Lock your own unpushed changes
      --only <ONLY>          With --auto: only these kinds (binary, unity, code, other)
      --watch                With --auto: keep locking files as you change them (while the panel runs)
      --off                  With --auto: stop --watch
      --refresh <TARGET|ID>  Recompute what one of your locks covers
  -h, --help                 Print help
```

## gang unlock

```text
Release locks

Usage: gang unlock [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...  What you locked (or a lock id from `gang locks`)

Options:
      --bake   Release a scene's bake lock
      --all    Release all your locks
      --force  Release someone else's lock (the team is told)
  -y, --yes    Release even files with changes that are not pushed yet
  -h, --help   Print help
```

## gang locks

```text
Show the team's locks

Usage: gang locks [OPTIONS]

Options:
      --tree  Also show what each lock covers
  -h, --help  Print help
```

## gang layers

```text
Layers, rendering layers and NavMesh areas: who reserved which number

Usage: gang layers [COMMAND]

Commands:
  fix      Move your new layers away from numbers a teammate reserved first, with your objects and scripts (Unity must be closed)
  release  Give up your reservation of a name
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

## gang pr

```text
GitHub pull requests, merged with GANG

Usage: gang pr merge [OPTIONS] [NUMBER]

Arguments:
  [NUMBER]  The pull request's number

Options:
      --into <INTO>  The branch to merge into (default: the pull request's base)
      --continue     Finish a merge that stopped for questions
      --abort        Give up a merge that stopped for questions
      --on-branch    Push the merge to the pull request's own branch instead of the base (for a protected base: then press Merge on GitHub, which keeps GANG's result because the branch is up to date)
  -h, --help         Print help
```

## gang ignore

```text
Manage ignore rules shared by the team

Usage: gang ignore <COMMAND>

Commands:
  add     Add rules (normalized for every OS)
  update  Regenerate the managed rules in .gitignore and .gitattributes
  check   Show whether, and why, paths are ignored
  doctor  Find ignore problems: tracked junk, missing .meta rules, line endings
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

## gang doctor

```text
Check the project's health

Usage: gang doctor [OPTIONS]

Options:
      --fix   Fix what can be fixed safely
  -h, --help  Print help
```

## gang auth

```text
Sign in to GitHub

Usage: gang auth <COMMAND>

Commands:
  login   Sign in with a code on github.com
  logout  Forget the GitHub token
  status  Show who is signed in
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

## gang git

```text
Usage: gang git <args>
```

Runs plain git, without GANG's additions. GANG still provides GitHub credentials.
