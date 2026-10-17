use anyhow::Result;
use clap::CommandFactory;
use gang_core::git;
use gang_core::invocation::Invocation;

const MAIN_HELP: &str = "\
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

git's global options (-C, -c, --no-pager, ...) are passed to git.";

const GIT_HELP: &str = "\
Run plain git, without GANG's additions

Usage: gang git <args>

GANG still provides GitHub credentials, but runs no checks and changes no
output. Use it when you need git's exact behavior.";

pub fn print_main() {
    println!("{MAIN_HELP}");
}

/// `gang help [command]`.
pub fn run(invocation: &Invocation) -> Result<i32> {
    match invocation.args.first().and_then(|arg| arg.to_str()) {
        None | Some("help") => print_main(),
        Some("git") => println!("{GIT_HELP}"),
        Some("undo") => println!(
            "Undo the last GANG operation\n\nUsage: gang undo\n\nUndoes the last `gang save` (the changes stay staged; refused once pushed),\nor brings back the files from before the last reset --hard, restore or clean."
        ),
        Some("who") => println!(
            "Who on the team is changing what\n\nUsage: gang [--json] who [path]\n\nWith a path, lists everyone changing that file or folder."
        ),
        Some("version") => crate::version::Args::command().print_help()?,
        Some("ignore") => crate::ignore_cmd::Args::command().print_help()?,
        Some("save") => crate::save_cmd::SaveArgs::command().print_help()?,
        Some("tree") => crate::semantic_cmd::TreeArgs::command().print_help()?,
        Some("sync") => crate::sync_cmd::Args::command().print_help()?,
        Some("bang") => crate::bang::Args::command().print_help()?,
        Some("lock") => crate::locks_cmd::LockArgs::command().print_help()?,
        Some("unlock") => crate::locks_cmd::UnlockArgs::command().print_help()?,
        Some("locks") => crate::locks_cmd::LocksArgs::command().print_help()?,
        Some("doctor") => crate::doctor_cmd::Args::command().print_help()?,
        Some("auth") => crate::auth_cmd::Args::command().print_help()?,
        Some(_) => {
            let args = invocation
                .git_globals
                .iter()
                .chain(&invocation.command)
                .chain(&invocation.args);
            return Ok(git::run_inherited(args)?);
        }
    }
    Ok(0)
}
