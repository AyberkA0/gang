//! The managed `.gitattributes` (DESIGN.md §17.2).

use crate::managed::{self, ManagedFile};

/// Version of the generated block. Bump it whenever the generated lines change.
pub const VERSION: u32 = 1;

/// Unity's text-serialized (YAML) asset types, merged by `gang merge-driver`.
pub const UNITY_YAML: &[&str] = &[
    "unity",
    "prefab",
    "asset",
    "mat",
    "anim",
    "controller",
    "overrideController",
    "mask",
    "physicMaterial",
    "physicsMaterial2D",
    "playable",
    "signal",
    "lighting",
    "terrainlayer",
    "brush",
    "flare",
    "guiskin",
    "fontsettings",
    "spriteatlas",
    "spriteatlasv2",
    "renderTexture",
    "cubemap",
    "mixer",
    "preset",
    "shadervariants",
    "giparams",
];

const CODE: &[&str] = &["cs", "shader", "hlsl", "cginc", "compute", "uss", "uxml"];

/// Binary file types: stored in Git LFS when the project uses it, marked
/// binary otherwise. Neither can be merged.
const LFS: &[&str] = &[
    // Images
    "psd",
    "psb",
    "png",
    "jpg",
    "jpeg",
    "tga",
    "tif",
    "tiff",
    "exr",
    "hdr",
    "bmp",
    "gif",
    "iff",
    // Models
    "fbx",
    "obj",
    "blend",
    "max",
    "ma",
    "mb",
    "3ds",
    "dae",
    // Audio
    "wav",
    "mp3",
    "ogg",
    "aif",
    "aiff",
    "flac",
    // Video
    "mp4",
    "mov",
    "webm",
    // Fonts
    "ttf",
    "otf",
    // Archives and packages
    "zip",
    "7z",
    "rar",
    "unitypackage",
    // Native and managed libraries
    "dll",
    "so",
    "a",
    "dylib",
    // Raw data
    "bytes",
];

/// Binary outputs that have a Unity YAML extension.
const LFS_FILES: &[&str] = &["**/LightingData.asset"];

const LFS_ATTRS: &str = "filter=lfs diff=lfs merge=lfs -text";
const BINARY_ATTRS: &str = "binary";

/// `psd` → `*.[pP][sS][dD]`, so `HERO.PSD` matches on every OS.
fn any_case_extension(extension: &str) -> String {
    let mut pattern = String::from("*.");
    for c in extension.chars() {
        if c.is_ascii_alphabetic() {
            pattern.push('[');
            pattern.push(c.to_ascii_lowercase());
            pattern.push(c.to_ascii_uppercase());
            pattern.push(']');
        } else {
            pattern.push(c);
        }
    }
    pattern
}

/// Whether the path is a binary type in the managed rules (and so cannot be
/// merged), whether or not the project stores it in Git LFS.
pub fn is_binary_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "LightingData.asset" {
        return true;
    }
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    extension.is_some_and(|e| LFS.contains(&e.as_str()))
}

/// Whether the path is a Unity YAML asset GANG merges semantically.
pub fn is_unity_yaml(path: &str) -> bool {
    let extension = path.rsplit_once('.').map(|(_, e)| e);
    extension.is_some_and(|e| UNITY_YAML.contains(&e))
}

/// The generated block. With `lfs`, binaries go to Git LFS; without it they
/// stay plain git files marked binary, so git never treats them as text.
pub fn block(lfs: bool) -> String {
    let unity = UNITY_YAML
        .iter()
        .map(|ext| format!("*.{ext} merge=gang"))
        .chain(std::iter::once("*.meta merge=gang".to_owned()))
        .collect();
    let code = CODE
        .iter()
        .map(|ext| format!("*.{ext} text eol=lf"))
        .chain(std::iter::once("*.cs diff=csharp".to_owned()))
        .collect();
    let gang = vec![
        ".gitignore text eol=lf merge=gang-ignore".to_owned(),
        ".gitattributes text eol=lf merge=gang-ignore".to_owned(),
        "**/ProjectSettings/ProjectVersion.txt merge=gang".to_owned(),
        "**/Packages/manifest.json merge=gang-json".to_owned(),
        "**/Packages/packages-lock.json merge=gang-json".to_owned(),
    ];
    let (title, attrs) = if lfs {
        ("Large binary files (Git LFS)", LFS_ATTRS)
    } else {
        (
            "Binary files (Git LFS is off: [lfs] enabled = false)",
            BINARY_ATTRS,
        )
    };
    let binaries = LFS
        .iter()
        .map(|ext| format!("{} {attrs}", any_case_extension(ext)))
        .chain(LFS_FILES.iter().map(|file| format!("{file} {attrs}")))
        .collect();

    managed::block(
        VERSION,
        "gang init",
        &[
            ("", vec!["* text=auto eol=lf".to_owned()]),
            ("Unity YAML (semantic merge)", unity),
            ("Code", code),
            ("Files GANG merges specially", gang),
            (title, binaries),
        ],
    )
}

/// The new `.gitattributes` text: the generated block, plus the existing lines
/// the block does not already contain.
pub fn regenerate(existing: &str, lfs: bool) -> String {
    let block = block(lfs);
    let current = ManagedFile::parse(existing);
    let rules = current
        .rules
        .into_iter()
        .filter(|rule| !block.lines().any(|line| line == rule))
        .collect();
    ManagedFile {
        version: Some(VERSION),
        block,
        rules,
    }
    .render(managed::Kind::Attributes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_match_any_case() {
        assert_eq!(any_case_extension("psd"), "*.[pP][sS][dD]");
        assert_eq!(any_case_extension("7z"), "*.7[zZ]");
    }

    #[test]
    fn lighting_data_is_lfs_after_the_asset_merge_rule() {
        let block = block(true);
        let asset = block.find("*.asset merge=gang").unwrap();
        let lighting = block.find("**/LightingData.asset filter=lfs").unwrap();
        assert!(asset < lighting, "later lines override earlier ones");
    }

    #[test]
    fn regenerate_keeps_custom_lines_once() {
        let first = regenerate(
            "*.psd filter=lfs diff=lfs merge=lfs -text\n*.txt text\n",
            true,
        );
        assert!(first.ends_with(
            "# Project rules — kept sorted\n*.psd filter=lfs diff=lfs merge=lfs -text\n*.txt text\n"
        ));
        assert_eq!(regenerate(&first, true), first);
    }

    #[test]
    fn without_lfs_binaries_are_marked_binary() {
        let block = block(false);
        assert!(!block.contains("filter=lfs"), "{block}");
        assert!(block.contains("*.[pP][nN][gG] binary"), "{block}");
        assert!(block.contains("**/LightingData.asset binary"), "{block}");
    }
}
