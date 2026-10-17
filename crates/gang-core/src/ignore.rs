//! The managed `.gitignore` (DESIGN.md §17.1).

use unicode_normalization::UnicodeNormalization;

use crate::managed::{self, ManagedFile};
use crate::unity::UnityProject;

/// Version of the generated block. Bump it whenever the generated rules change.
pub const VERSION: u32 = 1;

/// Unity's generated folders, relative to a project.
const UNITY_FOLDERS: &[&str] = &[
    "/[Ll]ibrary/",
    "/[Tt]emp/",
    "/[Oo]bj/",
    "/[Bb]uild/",
    "/[Bb]uilds/",
    "/[Ll]ogs/",
    "/[Uu]ser[Ss]ettings/",
    "/[Mm]emory[Cc]aptures/",
    "/[Rr]ecordings/",
    "/[Aa]ssets/[Aa]sset[Ss]tore[Tt]ools*",
];

const UNITY_FILES: &[&str] = &[
    "*_BackUpThisFolder_ButDontShipItWithYourGame",
    "*_BurstDebugInformation_DoNotShip",
    "sysinfo.txt",
    "crashlytics-build.properties",
    "*.apk",
    "*.aab",
    "*.pidb.meta",
    "*.pdb.meta",
    "*.mdb.meta",
];

const GENERATED_IDE: &[&str] = &[
    "*.csproj",
    "*.unityproj",
    "*.sln",
    "*.suo",
    "*.tmp",
    "*.user",
    "*.userprefs",
    "*.pidb",
    "*.booproj",
    "*.svd",
    "*.pdb",
    "*.mdb",
    "*.opendb",
    "*.VC.db",
];

const IDE: &[&str] = &[".vs/", ".vscode/", ".idea/", ".consulo/", "*.swp"];

const MACOS: &[&str] = &[
    ".DS_Store",
    ".AppleDouble",
    ".LSOverride",
    "._*",
    ".Spotlight-V100",
    ".Trashes",
    ".fseventsd",
    ".TemporaryItems",
    ".DocumentRevisions-V100",
];

const WINDOWS: &[&str] = &[
    "Thumbs.db",
    "ehthumbs.db",
    "[Dd]esktop.ini",
    "$RECYCLE.BIN/",
    "*.lnk",
    "*.stackdump",
];

const LINUX: &[&str] = &["*~", ".directory", ".fuse_hidden*", ".Trash-*", ".nfs*"];

const ADDRESSABLES: &[&str] = &[
    "/ServerData/",
    "/[Aa]ssets/[Ss]treaming[Aa]ssets/aa/",
    "/[Aa]ssets/[Ss]treaming[Aa]ssets/aa.meta",
];

/// What GANG detected in a Unity project that needs extra rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Detected {
    pub addressables: bool,
}

impl Detected {
    pub fn in_project(project_dir: &std::path::Path) -> Detected {
        let manifest =
            std::fs::read_to_string(project_dir.join("Packages/manifest.json")).unwrap_or_default();
        Detected {
            addressables: manifest.contains("\"com.unity.addressables\""),
        }
    }
}

/// The generated block for the given Unity projects.
pub fn block(projects: &[(UnityProject, Detected)]) -> String {
    let anchored = |rules: &[&str]| -> Vec<String> {
        projects
            .iter()
            .flat_map(|(project, _)| rules.iter().map(move |rule| anchor(project, rule)))
            .collect()
    };
    let lines = |rules: &[&str]| rules.iter().map(|r| r.to_string()).collect::<Vec<_>>();

    let mut unity = anchored(UNITY_FOLDERS);
    if !projects.is_empty() {
        unity.extend(lines(UNITY_FILES));
    }
    let detected: Vec<String> = projects
        .iter()
        .filter(|(_, detected)| detected.addressables)
        .flat_map(|(project, _)| ADDRESSABLES.iter().map(move |rule| anchor(project, rule)))
        .collect();
    let generated = if projects.is_empty() {
        Vec::new()
    } else {
        lines(GENERATED_IDE)
    };

    managed::block(
        VERSION,
        "gang ignore update",
        &[
            ("Unity", unity),
            ("Generated IDE files", generated),
            ("IDEs", lines(IDE)),
            ("macOS", lines(MACOS)),
            ("Windows", lines(WINDOWS)),
            ("Linux", lines(LINUX)),
            ("Detected for this project", detected),
        ],
    )
}

/// Anchors a project-relative rule (`/[Ll]ibrary/`) to the repository root.
fn anchor(project: &UnityProject, rule: &str) -> String {
    match rule.strip_prefix('/') {
        Some(rest) if !project.dir.is_empty() => format!("/{}/{rest}", project.dir),
        _ => rule.to_owned(),
    }
}

/// Turns a rule someone typed into the rules GANG stores (DESIGN.md §17.1):
/// NFC, case-insensitive word starts (`library/` → `[Ll]ibrary/`), anchored to
/// the Unity project when it starts with `Assets/`, and paired with its
/// `.meta` rule under `Assets/`.
pub fn normalize_rule(rule: &str, project: Option<&UnityProject>) -> Vec<String> {
    let rule: String = rule.trim().nfc().collect();
    if rule.is_empty() || rule.starts_with('#') {
        return Vec::new();
    }
    let (negation, body) = match rule.strip_prefix('!') {
        Some(body) => ("!", body),
        None => ("", rule.as_str()),
    };

    let unanchored = body.trim_start_matches('/');
    let mut body = body.to_owned();
    let under_assets = starts_with_word(unanchored, "assets/");
    if under_assets {
        body = match project {
            Some(project) => anchor(project, &format!("/{unanchored}")),
            None => format!("/{unanchored}"),
        };
    }
    // Unity always writes `.meta` in lower case, so that suffix stays literal.
    let mut body = case_insensitive(&body);
    if let Some(stem) = body.strip_suffix(".[Mm]eta") {
        body = format!("{stem}.meta");
    }

    let mut rules = vec![format!("{negation}{body}")];
    if under_assets && !plain(&body).to_ascii_lowercase().ends_with(".meta") {
        let target = body.trim_end_matches('/');
        if !target.ends_with('*') {
            rules.push(format!("{negation}{target}.meta"));
        }
    }
    rules
}

/// Whether `text`, ignoring case and `[Xx]` brackets, starts with `word`.
fn starts_with_word(text: &str, word: &str) -> bool {
    plain(text).to_ascii_lowercase().starts_with(word)
}

/// `[Ll]ibrary` → `library`.
fn plain(rule: &str) -> String {
    let mut out = String::new();
    let mut chars = rule.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            let class: String = chars.by_ref().take_while(|&c| c != ']').collect();
            out.push(class.chars().next().unwrap_or('?'));
        } else {
            out.push(c);
        }
    }
    out
}

/// Brackets the first letter of every word: `user_settings/Sandbox` →
/// `[Uu]ser_[Ss]ettings/[Ss]andbox`. A word starts at the beginning, after
/// any character that is not a letter or digit, or at a capital following a
/// small letter (`streamingAssets` → `[Ss]treaming[Aa]ssets`). Existing bracket
/// expressions and escaped characters are kept as they are.
pub fn case_insensitive(rule: &str) -> String {
    let mut out = String::with_capacity(rule.len() * 2);
    let mut prev: Option<char> = None;
    let mut chars = rule.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                    prev = Some(next);
                }
                continue;
            }
            '[' => {
                out.push(c);
                for inner in chars.by_ref() {
                    out.push(inner);
                    if inner == ']' {
                        break;
                    }
                }
                prev = Some('a');
                continue;
            }
            _ => {}
        }
        let word_start = match prev {
            None => true,
            Some(p) => !p.is_alphanumeric() || (c.is_ascii_uppercase() && p.is_ascii_lowercase()),
        };
        if c.is_ascii_alphabetic() && word_start {
            out.push('[');
            out.push(c.to_ascii_uppercase());
            out.push(c.to_ascii_lowercase());
            out.push(']');
        } else {
            out.push(c);
        }
        prev = Some(c);
    }
    out
}

/// Builds the new `.gitignore` text: the generated block, plus the existing
/// rules that the block does not already cover.
pub fn regenerate(existing: &str, block: String, project: Option<&UnityProject>) -> String {
    let current = ManagedFile::parse(existing);
    let covered: Vec<String> = ManagedFile::parse(&block)
        .block
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| plain(line).trim_start_matches('/').to_ascii_lowercase())
        .collect();
    let rules = current
        .rules
        .iter()
        .flat_map(|rule| normalize_rule(rule, project))
        .filter(|rule| {
            let key = plain(rule).trim_start_matches('/').to_ascii_lowercase();
            !covered.contains(&key)
        })
        .collect();
    ManagedFile {
        version: Some(VERSION),
        block,
        rules,
    }
    .render(managed::Kind::Ignore)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &str) -> UnityProject {
        UnityProject {
            dir: dir.into(),
            editor_version: None,
        }
    }

    #[test]
    fn case_insensitive_word_starts() {
        assert_eq!(case_insensitive("library/"), "[Ll]ibrary/");
        assert_eq!(case_insensitive("/Library/"), "/[Ll]ibrary/");
        assert_eq!(case_insensitive("user_settings"), "[Uu]ser_[Ss]ettings");
        assert_eq!(case_insensitive("StreamingAssets"), "[Ss]treaming[Aa]ssets");
        assert_eq!(case_insensitive("*.psd"), "*.[Pp]sd");
        assert_eq!(case_insensitive("[Ll]ibrary/"), "[Ll]ibrary/");
        assert_eq!(case_insensitive("\\#notes"), "\\#[Nn]otes");
    }

    #[test]
    fn case_insensitive_is_idempotent() {
        for rule in [
            "Assets/_Sandbox/",
            "user_settings",
            "*.tmp",
            "Build/**/x.txt",
        ] {
            let once = case_insensitive(rule);
            assert_eq!(case_insensitive(&once), once, "{rule}");
        }
    }

    #[test]
    fn rules_under_assets_get_anchored_and_paired_with_meta() {
        assert_eq!(
            normalize_rule("Assets/_Sandbox/", Some(&project(""))),
            vec!["/[Aa]ssets/_[Ss]andbox/", "/[Aa]ssets/_[Ss]andbox.meta"]
        );
        assert_eq!(
            normalize_rule("assets/Temp.txt", Some(&project("game"))),
            vec![
                "/[Gg]ame/[Aa]ssets/[Tt]emp.[Tt]xt",
                "/[Gg]ame/[Aa]ssets/[Tt]emp.[Tt]xt.meta"
            ]
        );
        assert_eq!(normalize_rule("*.log", None), vec!["*.[Ll]og"]);
        assert_eq!(
            normalize_rule("!Assets/Keep/", None),
            vec!["!/[Aa]ssets/[Kk]eep/", "!/[Aa]ssets/[Kk]eep.meta"]
        );
        assert!(normalize_rule("# comment", None).is_empty());
    }

    #[test]
    fn block_anchors_unity_rules_per_project() {
        let text = block(&[
            (project(""), Detected::default()),
            (project("tools/viewer"), Detected { addressables: true }),
        ]);
        assert!(text.contains("\n/[Ll]ibrary/\n"));
        assert!(text.contains("\n/tools/viewer/[Ll]ibrary/\n"));
        assert!(text.contains("\n/tools/viewer/ServerData/\n"));
        assert!(!text.contains("\n/ServerData/\n"));
        assert!(text.contains("\n.DS_Store\n"));
        assert!(text.contains("\nThumbs.db\n"));
        assert!(text.contains("\n.directory\n"));
    }

    #[test]
    fn block_without_unity_project_keeps_os_and_ide_rules() {
        let text = block(&[]);
        assert!(!text.contains("[Ll]ibrary"));
        assert!(!text.contains("*.csproj"));
        assert!(text.contains(".DS_Store"));
    }

    #[test]
    fn block_is_deterministic() {
        let projects = [(project(""), Detected::default())];
        assert_eq!(block(&projects), block(&projects));
    }

    #[test]
    fn regenerate_migrates_old_rules() {
        let existing = "# Unity\n[Ll]ibrary/\nLibrary/\n.DS_Store\nAssets/_Sandbox/\n*.log\n";
        let text = regenerate(
            existing,
            block(&[(project(""), Detected::default())]),
            Some(&project("")),
        );
        let parsed = ManagedFile::parse(&text);
        let rules: Vec<&str> = parsed.rules.iter().map(String::as_str).collect();
        assert_eq!(
            rules,
            vec![
                "*.[Ll]og",
                "/[Aa]ssets/_[Ss]andbox.meta",
                "/[Aa]ssets/_[Ss]andbox/"
            ]
        );
        assert_eq!(parsed.version, Some(VERSION));
        // Regenerating again changes nothing.
        let again = regenerate(
            &text,
            block(&[(project(""), Detected::default())]),
            Some(&project("")),
        );
        assert_eq!(again, text);
    }
}
