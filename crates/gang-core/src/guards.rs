//! Checks that keep a Unity repository healthy on every OS (DESIGN.md §16).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use unicode_normalization::{UnicodeNormalization, is_nfc};

use crate::repo::{Repo, split_nul};
use crate::unity::{self, UnityProject};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warn,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub path: String,
    pub message: String,
    /// A command or action that fixes it.
    pub fix: Option<String>,
}

impl Finding {
    fn block(path: &str, message: impl Into<String>, fix: Option<String>) -> Finding {
        Finding {
            severity: Severity::Block,
            path: path.to_owned(),
            message: message.into(),
            fix,
        }
    }

    fn warn(path: &str, message: impl Into<String>, fix: Option<String>) -> Finding {
        Finding {
            severity: Severity::Warn,
            path: path.to_owned(),
            message: message.into(),
            fix,
        }
    }
}

/// A staged change (`git diff --cached --name-status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// `A`dded, `M`odified, `D`eleted, `R`enamed, `C`opied, `T`ype changed.
    pub status: char,
    pub path: String,
}

impl Change {
    pub fn adds(&self) -> bool {
        matches!(self.status, 'A' | 'R' | 'C')
    }
    pub fn deletes(&self) -> bool {
        self.status == 'D'
    }
}

/// Changes staged for the next commit. A rename is a deletion plus an addition.
pub fn staged_changes(repo: &Repo) -> Result<Vec<Change>> {
    let output = repo.run(["diff", "--cached", "--name-status", "--no-renames", "-z"])?;
    let fields = split_nul(&output);
    Ok(fields
        .chunks(2)
        .filter_map(|pair| match pair {
            [status, path] => Some(Change {
                status: status.chars().next().unwrap_or('M'),
                path: path.clone(),
            }),
            _ => None,
        })
        .collect())
}

/// File names that are never committed, whatever `.gitignore` says.
const JUNK: &[&str] = &[
    ".ds_store",
    "thumbs.db",
    "ehthumbs.db",
    "desktop.ini",
    ".directory",
    ".spotlight-v100",
    ".trashes",
    ".fseventsd",
];

pub fn is_junk(path: &str) -> bool {
    let name = file_name(path);
    let lower = name.to_ascii_lowercase();
    JUNK.contains(&lower.as_str())
        || name.starts_with("._")
        || path.split('/').any(|part| {
            let lower = part.to_ascii_lowercase();
            lower == "$recycle.bin" || lower.starts_with(".trash-")
        })
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Why a path cannot be checked out on Windows, if it cannot.
pub fn windows_problem(path: &str) -> Option<String> {
    for part in path.split('/') {
        if let Some(c) = part
            .chars()
            .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (*c as u32) < 32)
        {
            return Some(format!(
                "{part:?} contains {c:?}, which Windows does not allow"
            ));
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Some(format!(
                "{part:?} ends with a dot or space, which Windows drops"
            ));
        }
        let stem = part.split('.').next().unwrap_or(part).to_ascii_lowercase();
        if WINDOWS_RESERVED.contains(&stem.as_str()) {
            return Some(format!("{part:?} is a reserved name on Windows"));
        }
    }
    None
}

/// Paths longer than this risk Windows' 260-character limit once the
/// repository's own folder is added.
pub const LONG_PATH: usize = 200;

/// Files Unity does not import, so they have no `.meta`.
fn unity_skips(relative: &str) -> bool {
    relative.split('/').any(|part| {
        part.starts_with('.')
            || part.ends_with('~')
            || part.eq_ignore_ascii_case("cvs")
            || part.to_ascii_lowercase().ends_with(".tmp")
    })
}

/// The Unity project and the path inside it, for paths under a project's
/// `Assets/` folder (`Assets/...` itself has no `.meta`).
/// Folders the staged deletions leave with nothing tracked in them while
/// their `.meta` stays (HARDENING.md R31): on a teammate's machine the
/// folder does not exist, and Unity deletes the lone `.meta`.
pub fn emptied_folders(repo: &Repo) -> Result<Vec<String>> {
    let changes = staged_changes(repo)?;
    if !changes.iter().any(Change::deletes) {
        return Ok(Vec::new());
    }
    let index: BTreeSet<String> = repo.tracked_files()?.into_iter().collect();
    Ok(emptied(
        &changes,
        &index,
        &unity::find_projects(repo.root()),
    ))
}

fn emptied(changes: &[Change], index: &BTreeSet<String>, projects: &[UnityProject]) -> Vec<String> {
    let has_children = |dir: &str| {
        let prefix = format!("{dir}/");
        index
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
    };
    let mut folders = BTreeSet::new();
    for change in changes.iter().filter(|c| c.deletes()) {
        let Some((parent, _)) = change.path.rsplit_once('/') else {
            continue;
        };
        if in_assets(projects, parent).is_some()
            && index.contains(&format!("{parent}.meta"))
            && !has_children(parent)
        {
            folders.insert(parent.to_owned());
        }
    }
    folders.into_iter().collect()
}

fn in_assets<'a>(projects: &'a [UnityProject], path: &str) -> Option<(&'a UnityProject, String)> {
    projects.iter().find_map(|project| {
        let assets = project.join("Assets/");
        let relative = path.strip_prefix(&assets)?;
        (!relative.is_empty() && !unity_skips(relative))
            .then(|| (project, format!("Assets/{relative}")))
    })
}

/// The other half of an asset/`.meta` pair, for paths inside a Unity
/// project's `Assets/` folder.
pub fn meta_partner(projects: &[UnityProject], path: &str) -> Option<String> {
    in_assets(projects, path)?;
    Some(match path.strip_suffix(".meta") {
        Some(asset) => asset.to_owned(),
        None => format!("{path}.meta"),
    })
}

/// `.meta` files of the folders above `path`, inside `Assets/`.
pub fn folder_metas(projects: &[UnityProject], path: &str) -> Vec<String> {
    let Some((project, relative)) = in_assets(projects, path) else {
        return Vec::new();
    };
    let mut metas = Vec::new();
    let mut dir = relative.as_str();
    while let Some(pos) = dir.rfind('/') {
        dir = &dir[..pos];
        if dir == "Assets" {
            break;
        }
        metas.push(format!("{}.meta", project.join(dir)));
    }
    metas
}

/// Options that relax checks on purpose.
#[derive(Debug, Clone, Default)]
pub struct Allow {
    /// The team is upgrading Unity: `ProjectVersion.txt` may change.
    pub unity_upgrade: bool,
}

/// Checks the staged changes, as the pre-commit hook does.
pub fn check_staged(repo: &Repo, allow: &Allow) -> Result<Vec<Finding>> {
    let changes = staged_changes(repo)?;
    let index: BTreeSet<String> = repo.tracked_files()?.into_iter().collect();
    let projects = unity::find_projects(repo.root());
    let mut findings = Vec::new();

    check_names(&changes, &index, &mut findings);
    check_metas(repo, &changes, &index, &projects, &mut findings);
    for folder in emptied(&changes, &index, &projects) {
        findings.push(Finding::warn(
            &format!("{folder}.meta"),
            format!(
                "stays, but nothing is left in {folder}; git keeps no empty folders, so teammates' Unity deletes this .meta"
            ),
            Some(format!("gang rm {}", quote(&format!("{folder}.meta")))),
        ));
    }
    check_guids(repo, &changes, &mut findings)?;
    check_markers(repo, &changes, &mut findings)?;
    check_local_only(repo, &changes, &index, &projects, &mut findings)?;
    check_sizes(repo, &changes, &mut findings)?;
    check_unity_version(&changes, &projects, allow, &mut findings);

    findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.path.cmp(&b.path)));
    findings.dedup();
    Ok(findings)
}

fn check_names(changes: &[Change], index: &BTreeSet<String>, findings: &mut Vec<Finding>) {
    let added: Vec<&Change> = changes.iter().filter(|c| c.adds()).collect();
    for change in &added {
        let path = &change.path;
        if is_junk(path) {
            findings.push(Finding::block(
                path,
                "is an OS junk file",
                Some(format!("gang rm --cached {}", quote(path))),
            ));
            continue;
        }
        if let Some(problem) = windows_problem(path) {
            findings.push(Finding::block(path, problem, Some("rename it".to_owned())));
        }
        if !is_nfc(path) {
            let nfc: String = path.nfc().collect();
            findings.push(Finding::block(
                path,
                "has a name in macOS's decomposed Unicode form (NFD); other systems see a different name",
                Some(format!("gang mv {} {}", quote(path), quote(&nfc))),
            ));
        }
        if path.chars().count() > LONG_PATH {
            findings.push(Finding::warn(
                path,
                format!("is longer than {LONG_PATH} characters; Windows may fail to check it out"),
                None,
            ));
        }
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".keystore") || lower.ends_with(".jks") {
            findings.push(Finding::warn(
                path,
                "is a signing key; make sure the team wants it in the repository",
                None,
            ));
        }
    }

    // Paths and folders that differ only in case.
    let mut by_lower: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for path in index {
        let mut prefix = String::new();
        for (i, part) in path.split('/').enumerate() {
            if i > 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            by_lower
                .entry(prefix.to_lowercase())
                .or_default()
                .insert(prefix.clone());
        }
    }
    for change in &added {
        let mut prefix = String::new();
        for (i, part) in change.path.split('/').enumerate() {
            if i > 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(variants) = by_lower.get(&prefix.to_lowercase()) {
                if variants.len() > 1 {
                    let others: Vec<&String> = variants.iter().filter(|v| **v != prefix).collect();
                    if !others.is_empty() {
                        findings.push(Finding::block(
                            &change.path,
                            format!(
                                "{prefix} differs only in case from {}; on macOS and Windows they are the same",
                                others.iter().map(|o| o.as_str()).collect::<Vec<_>>().join(", ")
                            ),
                            Some(format!("gang mv {} {}", quote(&change.path), quote(&replace_prefix(&change.path, &prefix, others[0])))),
                        ));
                        break;
                    }
                }
            }
        }
    }
}

fn replace_prefix(path: &str, prefix: &str, replacement: &str) -> String {
    format!("{replacement}{}", &path[prefix.len()..])
}

fn check_metas(
    repo: &Repo,
    changes: &[Change],
    index: &BTreeSet<String>,
    projects: &[UnityProject],
    findings: &mut Vec<Finding>,
) {
    let has_children = |dir: &str| {
        let prefix = format!("{dir}/");
        index
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
    };
    for change in changes {
        let path = change.path.as_str();
        let Some((project, relative)) = in_assets(projects, path) else {
            continue;
        };
        if let Some(asset) = path.strip_suffix(".meta") {
            if change.adds() && !index.contains(asset) && !has_children(asset) {
                let on_disk = repo.root().join(asset);
                // Decided by the .meta itself, not the disk: a teammate merging
                // this commit has no folder, since git keeps no empty folders,
                // and must get the same warning, not a block (field test A6).
                let folder_meta = repo
                    .try_run(["cat-file", "-p", &format!(":{path}")])
                    .ok()
                    .flatten()
                    .is_some_and(|text| text.contains("folderAsset: yes"));
                if on_disk.is_dir() || folder_meta {
                    findings.push(Finding::warn(
                        path,
                        format!(
                            "belongs to the empty folder {asset}; git does not keep empty folders"
                        ),
                        Some(format!("gang rm --cached {}", quote(path))),
                    ));
                } else {
                    findings.push(Finding::block(
                        path,
                        format!("has no asset: {asset} is not in the commit"),
                        Some(if on_disk.exists() {
                            format!("gang add {}", quote(asset))
                        } else {
                            format!("gang rm --cached {}", quote(path))
                        }),
                    ));
                }
            }
            if change.deletes() && (index.contains(asset) || has_children(asset)) {
                findings.push(Finding::block(
                    path,
                    format!("is removed but {asset} stays; Unity would give it a new GUID and break references"),
                    Some(format!("gang restore --staged {}", quote(path))),
                ));
            }
            continue;
        }

        let meta = format!("{path}.meta");
        if change.adds() && !index.contains(&meta) {
            let fix = if repo.root().join(&meta).exists() {
                format!("gang add {}", quote(&meta))
            } else {
                "open the project in Unity so it creates the .meta, then add it".to_owned()
            };
            findings.push(Finding::block(
                path,
                "has no .meta; every teammate's Unity would give it a different GUID",
                Some(fix),
            ));
        }
        if change.deletes() && index.contains(&meta) {
            findings.push(Finding::block(
                path,
                format!("is removed but {meta} stays"),
                Some(format!("gang rm --cached {}", quote(&meta))),
            ));
        }
        if change.adds() {
            // New folders need their .meta too.
            let mut dir = relative.as_str();
            while let Some(pos) = dir.rfind('/') {
                dir = &dir[..pos];
                if dir == "Assets" {
                    break;
                }
                let folder = project.join(dir);
                let folder_meta = format!("{folder}.meta");
                if !index.contains(&folder_meta) {
                    let fix = if repo.root().join(&folder_meta).exists() {
                        format!("gang add {}", quote(&folder_meta))
                    } else {
                        "open the project in Unity so it creates the folder's .meta".to_owned()
                    };
                    findings.push(Finding::block(
                        &folder,
                        "is a new folder without a .meta",
                        Some(fix),
                    ));
                }
            }
        }
    }
}

fn check_guids(repo: &Repo, changes: &[Change], findings: &mut Vec<Finding>) -> Result<()> {
    let changed_metas: BTreeSet<&str> = changes
        .iter()
        .filter(|c| !c.deletes() && c.path.ends_with(".meta"))
        .map(|c| c.path.as_str())
        .collect();
    if changed_metas.is_empty() {
        return Ok(());
    }
    let output = repo
        .try_run([
            "grep", "--cached", "-I", "-z", "-e", "^guid: ", "--", "*.meta",
        ])?
        .unwrap_or_default();
    let mut by_guid: HashMap<String, Vec<String>> = HashMap::new();
    // `-z` output: path NUL "guid: <hex>" NEWLINE
    for line in output.lines() {
        let Some((path, text)) = line.split_once('\0') else {
            continue;
        };
        if let Some(guid) = text.strip_prefix("guid: ") {
            by_guid
                .entry(guid.trim().to_owned())
                .or_default()
                .push(path.to_owned());
        }
    }
    for (guid, paths) in by_guid {
        if paths.len() < 2 {
            continue;
        }
        for path in paths.iter().filter(|p| changed_metas.contains(p.as_str())) {
            let others: Vec<&str> = paths
                .iter()
                .filter(|p| *p != path)
                .map(String::as_str)
                .collect();
            findings.push(Finding::block(
                path,
                format!(
                    "has GUID {guid}, already used by {}; one of them was copied outside Unity",
                    others.join(", ")
                ),
                Some("delete the copy's .meta and let Unity create a new one".to_owned()),
            ));
        }
    }
    Ok(())
}

/// Files with git's conflict markers in them, at `rev` (`None`: the
/// index), among `paths`.
pub fn with_markers(repo: &Repo, rev: Option<&str>, paths: &[&str]) -> Result<Vec<String>> {
    let mut found = Vec::new();
    // In batches: Windows limits a command line to 32k characters.
    for batch in paths.chunks(200) {
        let mut args: Vec<&str> = vec![
            "grep",
            "-l",
            "-z",
            "-I",
            "-E",
            "-e",
            "^<<<<<<< ",
            "-e",
            "^>>>>>>> ",
        ];
        match rev {
            Some(rev) => args.push(rev),
            None => args.push("--cached"),
        }
        args.push("--");
        args.extend(batch.iter().copied());
        let output = repo.try_run(args)?.unwrap_or_default();
        for path in split_nul(&output) {
            let path = match rev {
                Some(rev) => path
                    .strip_prefix(rev)
                    .and_then(|p| p.strip_prefix(':'))
                    .map(str::to_owned)
                    .unwrap_or(path),
                None => path,
            };
            found.push(path);
        }
    }
    Ok(found)
}

/// Leftover conflict markers (R33): Unity cannot open the asset, or the
/// team's scripts stop compiling. `gang bang` never leaves any; a merge
/// finished by hand in an editor, or with plain git, can.
fn check_markers(repo: &Repo, changes: &[Change], findings: &mut Vec<Finding>) -> Result<()> {
    let paths: Vec<&str> = changes
        .iter()
        .filter(|c| !c.deletes() && crate::attributes::breaks_with_markers(&c.path))
        .map(|c| c.path.as_str())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    for path in with_markers(repo, None, &paths)? {
        let what = if crate::attributes::is_code(&path) {
            "the team's scripts would stop compiling"
        } else {
            "Unity cannot open it"
        };
        findings.push(Finding::block(
            &path,
            format!("still has conflict markers (<<<<<<< … >>>>>>>); {what}"),
            Some(format!(
                "remove the markers (keep the lines you want from each side), then gang add {}",
                quote(&path)
            )),
        ));
    }
    Ok(())
}

/// A scene or prefab in the commit that uses an asset only this machine
/// has (R34): untracked, left out of a path-limited save, or ignored. It
/// works here and shows as Missing for everyone else.
fn check_local_only(
    repo: &Repo,
    changes: &[Change],
    index: &BTreeSet<String>,
    projects: &[UnityProject],
    findings: &mut Vec<Finding>,
) -> Result<()> {
    let users: Vec<&str> = changes
        .iter()
        .filter(|c| !c.deletes() && crate::attributes::is_unity_yaml(&c.path))
        .map(|c| c.path.as_str())
        .collect();
    if users.is_empty() {
        return Ok(());
    }
    // Every GUID the commit's metas have.
    let committed: BTreeSet<String> = repo
        .try_run([
            "grep", "--cached", "-h", "-I", "-e", "^guid: ", "--", "*.meta",
        ])?
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("guid: "))
        .map(|g| g.trim().to_owned())
        .collect();
    let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for user in &users {
        let Some(text) = crate::semantic::blob(repo, &format!(":{user}"))? else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for (i, _) in text.match_indices("guid: ") {
            let Some(guid) = text.get(i + 6..i + 38) else {
                continue;
            };
            if guid.bytes().all(|b| b.is_ascii_hexdigit())
                && !committed.contains(guid)
                && seen.insert(guid.to_owned())
            {
                wanted
                    .entry(guid.to_owned())
                    .or_default()
                    .push((*user).to_owned());
            }
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    // Only GUIDs of assets this machine has and the commit lacks: the
    // untracked (or ignored) metas in the projects. Package and built-in
    // assets are in no project, here or anywhere.
    let mut local: BTreeMap<String, String> = BTreeMap::new();
    for project in projects {
        for folder in ["Assets/", "Packages/"] {
            let spec = format!("{}*.meta", project.join(folder));
            let metas = repo
                .try_run(["ls-files", "-z", "--others", "--", &spec])?
                .unwrap_or_default();
            for meta in split_nul(&metas) {
                let Ok(text) = std::fs::read_to_string(repo.root().join(&meta)) else {
                    continue;
                };
                if let Some(guid) = crate::guid_moves::guid_of(&text) {
                    let asset = meta.strip_suffix(".meta").unwrap_or(&meta).to_owned();
                    local.insert(guid, asset);
                }
            }
        }
    }
    for (guid, users) in wanted {
        let Some(asset) = local.get(&guid).map(String::as_str) else {
            continue;
        };
        if index.contains(asset) && index.contains(&format!("{asset}.meta")) {
            continue;
        }
        let ignored = repo.try_run(["check-ignore", "-q", "--", asset])?.is_some();
        let (why, fix) = if ignored {
            (
                "is ignored (.gitignore)",
                format!(
                    "gang add -f {} {}",
                    quote(asset),
                    quote(&format!("{asset}.meta"))
                ),
            )
        } else {
            (
                "is not in the commit",
                format!(
                    "gang add {} {}",
                    quote(asset),
                    quote(&format!("{asset}.meta"))
                ),
            )
        };
        for user in users {
            findings.push(Finding::block(
                &user,
                format!("uses {asset}, which {why}: teammates would get a Missing reference"),
                Some(fix.clone()),
            ));
        }
    }
    Ok(())
}

const WARN_SIZE: u64 = 50 * 1024 * 1024;
const BLOCK_SIZE: u64 = 100 * 1024 * 1024;

fn check_sizes(repo: &Repo, changes: &[Change], findings: &mut Vec<Finding>) -> Result<()> {
    let paths: Vec<&str> = changes
        .iter()
        .filter(|c| !c.deletes())
        .map(|c| c.path.as_str())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["ls-files", "-s", "-z", "--"];
    args.extend(&paths);
    let entries = split_nul(&repo.run(&args)?);
    let mut objects = String::new();
    let mut names = Vec::new();
    for entry in &entries {
        // "<mode> <object> <stage>\t<path>"
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        if let Some(object) = meta.split(' ').nth(1) {
            objects.push_str(object);
            objects.push('\n');
            names.push(path.to_owned());
        }
    }
    let sizes = batch_check(repo, &objects)?;
    for (path, size) in names.iter().zip(sizes) {
        if size > BLOCK_SIZE {
            findings.push(Finding::block(
                path,
                format!(
                    "is {} MB; GitHub rejects files over 100 MB",
                    size / 1024 / 1024
                ),
                Some(
                    "keep it out of git, or turn on Git LFS ([lfs] enabled = true in .gang/config.toml; GitHub's LFS storage is paid)"
                        .to_owned(),
                ),
            ));
        } else if size > WARN_SIZE {
            findings.push(Finding::warn(
                path,
                format!(
                    "is {} MB; GitHub warns about files over 50 MB and rejects them over 100 MB",
                    size / 1024 / 1024
                ),
                Some(
                    "keep large source files (raw audio, video, PSDs) out of git if you can"
                        .to_owned(),
                ),
            ));
        }
    }
    Ok(())
}

fn batch_check(repo: &Repo, objects: &str) -> Result<Vec<u64>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = repo
        .command()
        .args(["cat-file", "--batch-check=%(objectsize)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(objects.as_bytes())?;
    let output = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().parse().unwrap_or(0))
        .collect())
}

fn check_unity_version(
    changes: &[Change],
    projects: &[UnityProject],
    allow: &Allow,
    findings: &mut Vec<Finding>,
) {
    if allow.unity_upgrade {
        return;
    }
    for project in projects {
        let path = project.join("ProjectSettings/ProjectVersion.txt");
        if changes.iter().any(|c| c.path == path && c.status == 'M') {
            findings.push(Finding::block(
                &path,
                "changed: this project was opened with a different Unity version",
                Some(
                    "open it with the team's version, or, if the team is upgrading on purpose: \
                     GANG_ALLOW_UNITY_UPGRADE=1 gang commit …"
                        .to_owned(),
                ),
            ));
        }
    }
}

fn quote(path: &str) -> String {
    if path
        .chars()
        .any(|c| c.is_whitespace() || "'\"$`\\!*?[]()&;|<>".contains(c))
    {
        format!("'{}'", path.replace('\'', r"'\''"))
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_names() {
        assert!(is_junk(".DS_Store"));
        assert!(is_junk("Assets/Art/Thumbs.db"));
        assert!(is_junk("Assets/._Hero.png"));
        assert!(is_junk("Desktop.ini"));
        assert!(!is_junk("Assets/Hero.png"));
        assert!(!is_junk("Assets/DS_Store.cs"));
    }

    #[test]
    fn windows_names() {
        assert!(windows_problem("Assets/Settings:Debug.asset").is_some());
        assert!(windows_problem("Assets/Readme.").is_some());
        assert!(windows_problem("Assets/Readme /a.txt").is_some());
        assert!(windows_problem("Assets/CON.txt").is_some());
        assert!(windows_problem("Assets/aux").is_some());
        assert!(windows_problem("Assets/console.txt").is_none());
        assert!(windows_problem("Assets/Çiçek.png").is_none());
    }

    #[test]
    fn unity_skipped_files() {
        assert!(unity_skips("Hidden~/x.cs"));
        assert!(unity_skips(".git-keep"));
        assert!(unity_skips("Foo/.hidden/a.png"));
        assert!(!unity_skips("Scripts/Player.cs"));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("Assets/a.png"), "Assets/a.png");
        assert_eq!(quote("Assets/My Scene.unity"), "'Assets/My Scene.unity'");
    }
}
