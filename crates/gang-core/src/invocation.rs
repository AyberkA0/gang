//! Splits a `gang` command line into GANG's options, git's global options,
//! the command and its arguments.
//!
//! GANG's own options must come before the command (`gang --raw log`), because
//! everything after the command belongs to it: `gang log --raw` is git's
//! `--raw`.

use std::ffi::OsString;

/// GANG's own options, given before the command.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Options {
    /// `--raw`: show git's own output, without GANG's additions.
    pub raw: bool,
    /// `--json`: machine-readable output where supported.
    pub json: bool,
    /// `--ignore-editor`: run even while Unity has the project open.
    pub ignore_editor: bool,
    /// `-h` / `--help`.
    pub help: bool,
    /// `-v` / `--version`.
    pub version: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub options: Options,
    /// git's global options (`-C <path>`, `-c <name>=<value>`, `--no-pager`, …),
    /// passed to git unchanged.
    pub git_globals: Vec<OsString>,
    pub command: Option<OsString>,
    pub args: Vec<OsString>,
}

/// git's global options that take their value as the next argument.
const GIT_GLOBALS_WITH_VALUE: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--attr-source",
];

impl Invocation {
    /// Parses the arguments after the program name.
    pub fn parse<I>(args: I) -> Invocation
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut invocation = Invocation::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            let Some(text) = arg.to_str() else {
                invocation.command = Some(arg);
                break;
            };
            match text {
                "--raw" => invocation.options.raw = true,
                "--json" => invocation.options.json = true,
                "--ignore-editor" => invocation.options.ignore_editor = true,
                "-h" | "--help" => invocation.options.help = true,
                "-v" | "--version" => invocation.options.version = true,
                _ if GIT_GLOBALS_WITH_VALUE.contains(&text) => {
                    invocation.git_globals.push(arg);
                    invocation.git_globals.extend(args.next());
                }
                // Any other option belongs to git; git reports unknown ones itself.
                _ if text.starts_with('-') => invocation.git_globals.push(arg),
                _ => {
                    invocation.command = Some(arg);
                    break;
                }
            }
        }

        invocation.args = args.collect();
        invocation
    }

    /// The command name, if there is one and it is valid UTF-8.
    pub fn command_name(&self) -> Option<&str> {
        self.command.as_ref().and_then(|command| command.to_str())
    }

    /// The arguments for running the whole command line through git: global
    /// options, command, arguments.
    pub fn git_args(&self) -> Vec<OsString> {
        self.git_globals
            .iter()
            .chain(&self.command)
            .chain(&self.args)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Invocation {
        Invocation::parse(args.iter().map(OsString::from))
    }

    fn strings(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments() {
        assert_eq!(parse(&[]), Invocation::default());
    }

    #[test]
    fn command_and_arguments() {
        let invocation = parse(&["commit", "-m", "Add trees"]);
        assert_eq!(invocation.command_name(), Some("commit"));
        assert_eq!(invocation.args, strings(&["-m", "Add trees"]));
        assert!(invocation.git_globals.is_empty());
    }

    #[test]
    fn gang_options_only_before_the_command() {
        let invocation = parse(&["--raw", "--json", "log", "--raw"]);
        assert!(invocation.options.raw);
        assert!(invocation.options.json);
        assert_eq!(invocation.args, strings(&["--raw"]));
    }

    #[test]
    fn git_global_options_are_kept_in_order() {
        let invocation = parse(&[
            "-C",
            "game",
            "-c",
            "core.abbrev=12",
            "--no-pager",
            "--git-dir=.git",
            "log",
            "-1",
        ]);
        assert_eq!(
            invocation.git_globals,
            strings(&[
                "-C",
                "game",
                "-c",
                "core.abbrev=12",
                "--no-pager",
                "--git-dir=.git"
            ])
        );
        assert_eq!(
            invocation.git_args(),
            strings(&[
                "-C",
                "game",
                "-c",
                "core.abbrev=12",
                "--no-pager",
                "--git-dir=.git",
                "log",
                "-1"
            ])
        );
    }

    #[test]
    fn help_and_version_flags() {
        assert!(parse(&["--help"]).options.help);
        assert!(parse(&["-h"]).options.help);
        assert!(parse(&["--version"]).options.version);
        assert!(parse(&["-v"]).options.version);
    }

    #[test]
    fn options_without_a_command_go_to_git() {
        let invocation = parse(&["--exec-path"]);
        assert_eq!(invocation.command, None);
        assert_eq!(invocation.git_globals, strings(&["--exec-path"]));
    }

    #[test]
    fn missing_value_at_the_end() {
        let invocation = parse(&["-C"]);
        assert_eq!(invocation.git_globals, strings(&["-C"]));
        assert_eq!(invocation.command, None);
    }
}
