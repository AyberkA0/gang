//! Which git commands GANG wraps, and with which middleware layers
//! (DESIGN.md §8.1 and §8.3). Commands not listed here are passed to git
//! unchanged.

/// A middleware layer that runs around a wrapped git command.
///
/// The presence layer runs after every command, so it is not listed per
/// command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// Token and credential helper before network operations.
    Auth,
    /// Meta, junk, file-name and Unity version checks.
    Guard,
    /// Keeps assets and their `.meta` files together.
    Meta,
    /// Stops rewriting scenes and prefabs while Unity is open.
    Editor,
    /// Semantic merge and `gang bang`.
    Merge,
    /// Readable output for Unity files.
    Display,
    /// Blocks changes to other people's locks.
    Locks,
}

/// The layers of a wrapped git command, or `None` when GANG passes the
/// command to git unchanged.
pub fn wrapped_layers(command: &str) -> Option<&'static [Layer]> {
    use Layer::*;

    let layers: &'static [Layer] = match command {
        "init" => &[Guard],
        "clone" => &[Auth],
        "add" => &[Guard, Meta],
        "commit" => &[Guard, Locks],
        "status" | "diff" | "log" | "show" | "blame" => &[Display],
        "restore" | "checkout" => &[Editor, Meta],
        "rm" | "mv" => &[Meta],
        "reset" | "clean" | "switch" | "bisect" => &[Editor],
        "stash" | "merge" | "rebase" | "cherry-pick" | "revert" => &[Editor, Merge],
        "mergetool" => &[Merge],
        "fetch" | "ls-remote" | "submodule" | "lfs" => &[Auth],
        "pull" => &[Auth, Editor, Merge],
        "push" => &[Auth, Guard, Locks],
        "config" | "remote" | "branch" | "tag" | "worktree" | "bugreport" | "filter-branch"
        | "filter-repo" => &[],
        _ => return None,
    };
    Some(layers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_commands_have_their_layers() {
        assert_eq!(
            wrapped_layers("pull"),
            Some(&[Layer::Auth, Layer::Editor, Layer::Merge][..])
        );
        assert_eq!(
            wrapped_layers("commit"),
            Some(&[Layer::Guard, Layer::Locks][..])
        );
        assert_eq!(wrapped_layers("tag"), Some(&[][..]));
    }

    #[test]
    fn everything_else_is_passed_through() {
        for command in ["cat-file", "rev-parse", "gc", "frobnicate", ""] {
            assert_eq!(wrapped_layers(command), None, "{command}");
        }
    }
}
