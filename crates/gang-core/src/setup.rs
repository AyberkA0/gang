//! Setting a repository up for GANG: managed files, merge drivers and hooks
//! (DESIGN.md §8.3 `init` / `clone`, PLAN.md P3).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::repo::Repo;
use crate::unity::UnityProject;
use crate::{attributes, config, git, ignore};

/// Hooks GANG installs. Each one runs `gang hook <name>`.
pub const HOOKS: &[&str] = &[
    "pre-commit",
    "pre-push",
    "post-checkout",
    "post-commit",
    "post-merge",
];

/// Hooks Git LFS needs. GANG's shims call `git lfs <hook>` themselves, so LFS
/// never installs its own hooks (`--skip-repo`).
const LFS_HOOKS: &[&str] = &["pre-push", "post-checkout", "post-commit", "post-merge"];

const HOOK_MARKER: &str = "# Installed by GANG.";

/// Whether the repository is set up for GANG (it has `.gang/config.toml`).
pub fn is_gang_repo(repo: &Repo) -> bool {
    repo.root().join(config::PATH).is_file()
}

/// The command git should run to call this GANG binary, quoted for a shell.
pub fn gang_command() -> Result<String> {
    let exe = std::env::current_exe().context("could not find the gang binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(format!("\"{}\"", exe.to_string_lossy().replace('\\', "/")))
}

/// Writes `.gitignore`, `.gitattributes` and (when missing) `.gang/config.toml`.
/// Returns a line for every file that changed.
pub fn write_managed_files(repo: &Repo, projects: &[UnityProject]) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    let root = repo.root();

    let detected: Vec<_> = projects
        .iter()
        .map(|p| (p.clone(), ignore::Detected::in_project(&p.path(root))))
        .collect();
    let single = (projects.len() == 1).then(|| &projects[0]);
    let gitignore = root.join(".gitignore");
    let existing = read_or_empty(&gitignore)?;
    let updated = ignore::regenerate(&existing, ignore::block(&detected), single);
    if write_if_changed(&gitignore, &existing, &updated)? {
        changed.push(".gitignore".to_owned());
    }

    let gitattributes = root.join(".gitattributes");
    let existing = read_or_empty(&gitattributes)?;
    let config = config::Config::load(root).unwrap_or_default();
    let updated = attributes::regenerate(&existing, config::lfs_enabled(root, &config));
    if write_if_changed(&gitattributes, &existing, &updated)? {
        changed.push(".gitattributes".to_owned());
    }

    let config_path = root.join(config::PATH);
    if !config_path.exists() {
        fs::create_dir_all(config_path.parent().unwrap())?;
        fs::write(&config_path, config::DEFAULT_TEXT)?;
        changed.push(config::PATH.to_owned());
    }
    Ok(changed)
}

fn read_or_empty(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", path.display())),
    }
}

fn write_if_changed(path: &Path, old: &str, new: &str) -> Result<bool> {
    if old == new {
        return Ok(false);
    }
    fs::write(path, new).with_context(|| format!("could not write {}", path.display()))?;
    Ok(true)
}

/// Registers the merge drivers and hooks in the repository's own config and
/// hooks folder, and Git LFS when the project uses it and it is installed.
/// Returns what was done.
pub fn install(repo: &Repo) -> Result<Vec<String>> {
    let gang = gang_command()?;
    let mut done = Vec::new();

    let settings = [
        (
            "merge.gang.name",
            "GANG semantic merge for Unity files".to_owned(),
        ),
        (
            "merge.gang.driver",
            format!("{gang} merge-driver unity %O %A %B %P %Y"),
        ),
        (
            "merge.gang-ignore.name",
            "GANG merge for managed files".to_owned(),
        ),
        (
            "merge.gang-ignore.driver",
            format!("{gang} merge-driver ignore %O %A %B %P"),
        ),
        ("merge.gang-json.name", "GANG merge for JSON".to_owned()),
        (
            "merge.gang-json.driver",
            format!("{gang} merge-driver json %O %A %B %P %Y"),
        ),
    ];
    for (key, value) in settings {
        if repo.config_get(key)?.as_deref() != Some(value.as_str()) {
            repo.config_set(key, &value)?;
        }
    }
    done.push("merge drivers registered".to_owned());

    if crate::auth::load().is_some() {
        crate::auth::install_helper(repo, &gang)?;
        done.push("GitHub sign-in connected to git".to_owned());
    }

    let hooks_dir = repo.hooks_dir()?;
    fs::create_dir_all(&hooks_dir)?;
    for name in HOOKS {
        if let Some(note) = install_hook(&hooks_dir, name, &gang)? {
            done.push(note);
        }
    }
    done.push(format!("hooks installed ({})", HOOKS.join(", ")));

    let config = config::Config::load(repo.root()).unwrap_or_default();
    if !config::lfs_enabled(repo.root(), &config) {
        // Nothing to do: binaries are plain git files ([lfs] enabled = false).
    } else if git::lfs_version().is_some() {
        repo.run(["lfs", "install", "--local", "--skip-repo"])?;
        done.push("Git LFS enabled".to_owned());
    } else {
        done.push("⚠ Git LFS is not installed; large files will not use LFS".to_owned());
    }

    let marker = marker_path(repo);
    fs::create_dir_all(marker.parent().unwrap())?;
    fs::write(&marker, marker_text(&gang))?;
    Ok(done)
}

/// Installs drivers and hooks when this GANG binary has not done it yet in a
/// GANG repository (for example after a plain `git clone`). Cheap when
/// nothing needs doing.
pub fn ensure_installed(repo: &Repo) -> Result<()> {
    if !is_gang_repo(repo) {
        return Ok(());
    }
    let gang = gang_command()?;
    let current = fs::read_to_string(marker_path(repo)).unwrap_or_default();
    if current != marker_text(&gang) {
        install(repo)?;
    }
    Ok(())
}

fn marker_path(repo: &Repo) -> PathBuf {
    repo.gang_dir().join("installed")
}

/// Changes when the installed drivers or hooks change, so existing clones
/// are set up again.
const SETUP_VERSION: u32 = 3;

fn marker_text(gang: &str) -> String {
    format!("{gang}\n{}\n{SETUP_VERSION}\n", env!("CARGO_PKG_VERSION"))
}

/// Writes one hook shim. An existing hook that GANG did not write is kept as
/// `<name>.local` and still runs first.
fn install_hook(dir: &Path, name: &str, gang: &str) -> Result<Option<String>> {
    let path = dir.join(name);
    let mut note = None;
    if let Ok(existing) = fs::read_to_string(&path) {
        if is_lfs_hook(&existing) {
            // GANG's shim runs the same LFS command.
            fs::remove_file(&path)?;
        } else if !existing.contains(HOOK_MARKER) {
            let local = dir.join(format!("{name}.local"));
            if local.exists() {
                return Ok(Some(format!(
                    "⚠ {name} hook not installed: both {name} and {name}.local exist"
                )));
            }
            fs::rename(&path, &local)?;
            note = Some(format!("existing {name} hook kept as {name}.local"));
        }
    }
    fs::write(&path, hook_script(name, gang))?;
    make_executable(&path)?;
    Ok(note)
}

/// The hook Git LFS writes itself (`git lfs install`).
fn is_lfs_hook(text: &str) -> bool {
    text.contains("This repository is configured for Git LFS") && !text.contains(HOOK_MARKER)
}

fn hook_script(name: &str, gang: &str) -> String {
    // pre-push gets the list of refs on stdin; every step needs it.
    let (save_stdin, feed) = if name == "pre-push" {
        ("input=$(cat)\n", "printf '%s\\n' \"$input\" | ")
    } else {
        ("", "")
    };
    let lfs = if LFS_HOOKS.contains(&name) {
        let missing = if name == "pre-push" {
            "  echo \"Git LFS is not installed, so large files cannot be pushed. Install it from https://git-lfs.com\" >&2\n  exit 2\n"
        } else {
            "  echo \"Git LFS is not installed; large files stay as pointers. Install it from https://git-lfs.com\" >&2\n"
        };
        // Missing LFS only matters to projects that use it.
        format!(
            "if command -v git-lfs >/dev/null 2>&1; then\n\
             \x20 {feed}git lfs {name} \"$@\" || exit $?\n\
             elif grep -qs filter=lfs .gitattributes; then\n\
             {missing}\
             fi\n"
        )
    } else {
        String::new()
    };
    format!(
        "#!/bin/sh\n\
         {HOOK_MARKER} An earlier hook, if any, is kept as {name}.local and runs first.\n\
         {save_stdin}\
         local_hook=\"$(dirname \"$0\")/{name}.local\"\n\
         if [ -x \"$local_hook\" ]; then\n\
         \x20 {feed}\"$local_hook\" \"$@\" || exit $?\n\
         fi\n\
         {lfs}\
         gang={gang}\n\
         if [ ! -x \"$gang\" ]; then\n\
         \x20 gang=$(command -v gang) || {{ echo \"gang not found; skipping GANG's {name} checks\" >&2; exit 0; }}\n\
         fi\n\
         {feed}\"$gang\" hook {name} \"$@\"\n"
    )
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_scripts_chain_and_find_gang() {
        let script = hook_script("pre-commit", "\"/opt/gang\"");
        assert!(script.starts_with("#!/bin/sh\n# Installed by GANG."));
        assert!(script.contains("gang=\"/opt/gang\"\n"));
        assert!(script.contains("\"$gang\" hook pre-commit \"$@\"\n"));
        assert!(!script.contains("input=$(cat)"));

        assert!(!script.contains("git lfs"));

        let script = hook_script("pre-push", "\"/opt/gang\"");
        assert!(script.contains("input=$(cat)\n"));
        assert!(script.contains("printf '%s\\n' \"$input\" | git lfs pre-push \"$@\" || exit $?"));
        assert!(script.contains("exit 2"));
        assert!(script.contains("printf '%s\\n' \"$input\" | \"$gang\" hook pre-push"));
    }
}
