//! Unity projects inside a repository: where they are, which editor version
//! they use, and whether their settings work with version control.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A Unity project found in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnityProject {
    /// Directory of the project relative to the repository root, with `/`
    /// separators; empty when the project is the repository root.
    pub dir: String,
    /// `m_EditorVersion` from `ProjectSettings/ProjectVersion.txt`.
    pub editor_version: Option<String>,
}

impl UnityProject {
    /// `prefix` + `path`, e.g. `game/Assets` for a project in `game/`.
    pub fn join(&self, path: &str) -> String {
        if self.dir.is_empty() {
            path.to_owned()
        } else {
            format!("{}/{}", self.dir, path)
        }
    }

    pub fn path(&self, repo_root: &Path) -> PathBuf {
        if self.dir.is_empty() {
            repo_root.to_owned()
        } else {
            repo_root.join(&self.dir)
        }
    }
}

/// Folders never searched for Unity projects.
const SKIP_DIRS: &[&str] = &[
    "Library",
    "Temp",
    "Logs",
    "obj",
    "Build",
    "Builds",
    "UserSettings",
    "node_modules",
    "Packages",
];

const MAX_DEPTH: usize = 4;

/// Finds Unity projects (folders with `Assets/` and
/// `ProjectSettings/ProjectVersion.txt`) up to four levels below `root`.
pub fn find_projects(root: &Path) -> Vec<UnityProject> {
    let mut projects = Vec::new();
    search(root, "", 0, &mut projects);
    projects.sort_by(|a, b| a.dir.cmp(&b.dir));
    projects
}

fn search(dir: &Path, rel: &str, depth: usize, projects: &mut Vec<UnityProject>) {
    let version_file = dir.join("ProjectSettings").join("ProjectVersion.txt");
    if version_file.is_file() && dir.join("Assets").is_dir() {
        projects.push(UnityProject {
            dir: rel.to_owned(),
            editor_version: read_editor_version(&version_file),
        });
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            (!name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()))
                .then(|| (name, entry.path()))
        })
        .collect();
    children.sort();
    for (name, path) in children {
        let child_rel = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        search(&path, &child_rel, depth + 1, projects);
    }
}

fn read_editor_version(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("m_EditorVersion:"))
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty())
}

/// Version-control related project settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Asset Serialization mode: `Some(true)` for Force Text. `None` when it
    /// cannot be read (missing or binary `EditorSettings.asset`).
    pub force_text: Option<bool>,
    /// Version Control mode: `Some(true)` for Visible Meta Files. `None` when
    /// not set explicitly (Unity's default is visible meta files).
    pub visible_meta: Option<bool>,
}

impl Settings {
    pub fn is_ok(&self) -> bool {
        self.force_text == Some(true) && self.visible_meta != Some(false)
    }
}

const EDITOR_SETTINGS: &str = "ProjectSettings/EditorSettings.asset";
const VERSION_CONTROL_SETTINGS: &str = "ProjectSettings/VersionControlSettings.asset";
const SERIALIZATION_KEY: &str = "m_SerializationMode:";
const FORCE_TEXT: &str = "2";
const META_KEYS: &[(&str, &str)] = &[
    (VERSION_CONTROL_SETTINGS, "m_Mode:"),
    (EDITOR_SETTINGS, "m_ExternalVersionControlSupport:"),
];
const VISIBLE_META: &str = "Visible Meta Files";

/// Reads the project's serialization and version control settings.
pub fn read_settings(project_dir: &Path) -> Settings {
    let editor = read_text(&project_dir.join(EDITOR_SETTINGS));
    let force_text = editor
        .as_deref()
        .and_then(|text| value_of(text, SERIALIZATION_KEY))
        .map(|value| value == FORCE_TEXT);
    let visible_meta = META_KEYS.iter().find_map(|(file, key)| {
        let text = read_text(&project_dir.join(file))?;
        value_of(&text, key).map(|value| value == VISIBLE_META)
    });
    Settings {
        force_text,
        visible_meta,
    }
}

/// Switches the project to Force Text and Visible Meta Files by editing the
/// settings files in place. Returns what was changed. Fails when a settings
/// file is binary; Unity has to change it then.
pub fn fix_settings(project_dir: &Path) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    let editor_path = project_dir.join(EDITOR_SETTINGS);
    let editor = read_text(&editor_path).with_context(|| {
        format!(
            "{} is missing or not text; switch Asset Serialization to Force Text in Unity \
             (Project Settings › Editor)",
            EDITOR_SETTINGS
        )
    })?;
    if let Some(updated) = set_value(&editor, SERIALIZATION_KEY, FORCE_TEXT) {
        fs::write(&editor_path, updated)?;
        changed.push("Asset Serialization → Force Text".to_owned());
    }
    for (file, key) in META_KEYS {
        let path = project_dir.join(file);
        if let Some(text) = read_text(&path) {
            if let Some(updated) = set_value(&text, key, VISIBLE_META) {
                fs::write(&path, updated)?;
                changed.push("Version Control → Visible Meta Files".to_owned());
            }
        }
    }
    Ok(changed)
}

fn read_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes)
        .ok()
        .filter(|text| text.starts_with("%YAML") || text.contains(':'))
}

fn value_of<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix(key))
        .map(str::trim)
}

/// Replaces the value of `key`, keeping the rest of the file byte for byte.
/// `None` when the key is missing or already has `value`.
fn set_value(text: &str, key: &str, value: &str) -> Option<String> {
    let mut found = false;
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if !found && trimmed.starts_with(key) {
            found = true;
            let indent = &line[..line.len() - trimmed.len()];
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            let current = trimmed[key.len()..].trim();
            if current != value {
                changed = true;
                out.push_str(&format!("{indent}{key} {value}{ending}"));
                continue;
            }
        }
        out.push_str(line);
    }
    changed.then_some(out)
}

/// Whether a Unity editor has the project open. Unity holds a lock on
/// `Temp/UnityLockfile` while it runs; a file left behind by a crash is not
/// locked. (To be confirmed on every OS by spike S4.)
pub fn is_editor_open(project_dir: &Path) -> bool {
    let path = project_dir.join("Temp").join("UnityLockfile");
    let file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return false,
        // Windows refuses to open a file another process holds exclusively.
        Err(_) => return true,
    };
    let mut lock = fd_lock::RwLock::new(file);
    lock.try_write().is_err()
}

/// Unity editor versions installed through Unity Hub in its default location.
pub fn installed_editors() -> Vec<String> {
    let mut versions: Vec<String> = hub_editor_dirs()
        .iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    versions.sort();
    versions.dedup();
    versions
}

/// The data folders of installed editors (where Unity keeps its .NET
/// runtime, its C# compiler and its assemblies), the `preferred` version
/// first, then the newest. `GANG_UNITY_DATA` overrides the search.
pub fn editor_data_dirs(preferred: Option<&str>) -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("GANG_UNITY_DATA") {
        return vec![PathBuf::from(dir)];
    }
    let mut versions = installed_editors();
    versions.reverse();
    if let Some(preferred) = preferred {
        if let Some(i) = versions.iter().position(|v| v == preferred) {
            let v = versions.remove(i);
            versions.insert(0, v);
        }
    }
    let mut dirs = Vec::new();
    for version in versions {
        for hub in hub_editor_dirs() {
            let data = if cfg!(target_os = "macos") {
                hub.join(&version).join("Unity.app").join("Contents")
            } else {
                hub.join(&version).join("Editor").join("Data")
            };
            if data.is_dir() {
                dirs.push(data);
            }
        }
    }
    dirs
}

fn hub_editor_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Applications/Unity/Hub/Editor"));
    } else if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432"] {
            if let Some(base) = std::env::var_os(var) {
                dirs.push(PathBuf::from(base).join("Unity").join("Hub").join("Editor"));
            }
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Unity").join("Hub").join("Editor"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path, dir: &str, version: &str) {
        let base = root.join(dir);
        fs::create_dir_all(base.join("Assets")).unwrap();
        fs::create_dir_all(base.join("ProjectSettings")).unwrap();
        fs::write(
            base.join("ProjectSettings/ProjectVersion.txt"),
            format!("m_EditorVersion: {version}\nm_EditorVersionWithRevision: {version} (abc)\n"),
        )
        .unwrap();
    }

    #[test]
    fn finds_projects_at_root_and_below() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "", "6000.0.23f1");
        assert_eq!(
            find_projects(dir.path()),
            vec![UnityProject {
                dir: String::new(),
                editor_version: Some("6000.0.23f1".into())
            }]
        );

        let dir = tempfile::tempdir().unwrap();
        project(dir.path(), "game", "2022.3.10f1");
        project(dir.path(), "tools/editor-test", "6000.0.1f1");
        fs::create_dir_all(dir.path().join("game/Library/Foo/ProjectSettings")).unwrap();
        let found: Vec<String> = find_projects(dir.path())
            .into_iter()
            .map(|p| p.dir)
            .collect();
        assert_eq!(found, vec!["game", "tools/editor-test"]);
    }

    #[test]
    fn reads_and_fixes_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("ProjectSettings");
        fs::create_dir_all(&settings).unwrap();
        fs::write(
            settings.join("EditorSettings.asset"),
            "%YAML 1.1\n--- !u!159 &1\nEditorSettings:\n  m_ObjectHideFlags: 0\n  m_SerializationMode: 0\n  m_LineEndingsForNewScripts: 0\n",
        )
        .unwrap();
        fs::write(
            settings.join("VersionControlSettings.asset"),
            "%YAML 1.1\n--- !u!890905787 &1\nVersionControlSettings:\n  m_Mode: Hidden Meta Files\n",
        )
        .unwrap();

        let before = read_settings(dir.path());
        assert_eq!(before.force_text, Some(false));
        assert_eq!(before.visible_meta, Some(false));
        assert!(!before.is_ok());

        let changed = fix_settings(dir.path()).unwrap();
        assert_eq!(changed.len(), 2);
        assert!(read_settings(dir.path()).is_ok());
        let editor = fs::read_to_string(settings.join("EditorSettings.asset")).unwrap();
        assert!(editor.contains("  m_SerializationMode: 2\n  m_LineEndingsForNewScripts: 0\n"));
        assert!(fix_settings(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn editor_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_editor_open(dir.path()));
        fs::create_dir_all(dir.path().join("Temp")).unwrap();
        let path = dir.path().join("Temp/UnityLockfile");
        fs::write(&path, "").unwrap();
        assert!(
            !is_editor_open(dir.path()),
            "a stale lockfile is not an open editor"
        );

        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let mut lock = fd_lock::RwLock::new(file);
        let _guard = lock.write().unwrap();
        assert!(is_editor_open(dir.path()));
    }

    #[test]
    fn binary_settings_cannot_be_fixed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("ProjectSettings")).unwrap();
        fs::write(dir.path().join(EDITOR_SETTINGS), [0u8, 159, 0, 0, 255]).unwrap();
        assert_eq!(read_settings(dir.path()).force_text, None);
        assert!(fix_settings(dir.path()).is_err());
    }
}
