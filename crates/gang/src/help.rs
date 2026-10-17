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
  undo             Undo the last save, reset, restore or clean
  who [path]       Who on the team is changing what
  tree <file>      Show the objects of a scene or prefab
  ignore           Manage ignore rules shared by the team
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
        Some("version") => crate::version::Args::command().print_help()?,
        Some("ignore") => crate::ignore_cmd::Args::command().print_help()?,
        Some("save") => crate::save_cmd::SaveArgs::command().print_help()?,
        Some("tree") => crate::semantic_cmd::TreeArgs::command().print_help()?,
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
