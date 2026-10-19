//! Compiler-level C# analysis for the layer remap (DESIGN.md §14.2).
//!
//! The text rules in `gang-csharp` recognise layer numbers by their shape.
//! This module asks the C# compiler instead: a small analyzer
//! (`analyzer/GangAnalyzer.cs`) is compiled on first use with the Roslyn
//! compiler inside the installed Unity editor and run with the editor's own
//! .NET runtime. It builds the project's scripts against Unity's assemblies,
//! finds every value that reaches a layer index or layer mask (method
//! parameters, `GameObject.layer`, `LayerMask`, comparisons, switches, and
//! the project's own methods, fields and constants that pass them on), and
//! traces each one back to the number written in the source.
//!
//! Nothing is downloaded. Without a Unity editor on the machine the remap
//! falls back to the text rules.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::repo::Repo;
use crate::unity::{self, UnityProject};

const SOURCE: &str = include_str!("analyzer/GangAnalyzer.cs");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Index,
    Mask,
    Mixed,
}

/// A value that reaches a layer index or mask.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Use {
    /// Relative to the repository, with `/`.
    pub file: String,
    /// 1-based.
    pub line: usize,
    /// 0-based, in UTF-16 units (the compiler's columns).
    pub column: usize,
    pub text: String,
    pub role: Role,
    pub value: Option<i64>,
    /// Where it goes: `Physics.Raycast(layerMask) ← InteractableMask (1 << n)`.
    pub via: String,
    /// `literal`, `implicit-enum`, `computed`, `external`, `unknown`, `mixed`.
    pub kind: String,
}

/// A serialized script field that holds a layer or a mask.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Field {
    /// The class, with its namespace.
    #[serde(rename = "type")]
    pub type_name: String,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Analysis {
    pub files: usize,
    pub errors: usize,
    /// The first compile errors, for `GANG_DEBUG`.
    #[serde(default)]
    pub messages: Vec<String>,
    pub uses: Vec<Use>,
    pub fields: Vec<Field>,
}

impl Analysis {
    pub fn uses_in<'a>(&'a self, path: &'a str) -> impl Iterator<Item = &'a Use> + 'a {
        self.uses.iter().filter(move |u| u.file == path)
    }

    /// Serialized fields by class name (without namespace) and field name.
    pub fn field_roles(&self) -> BTreeMap<(String, String), Role> {
        self.fields
            .iter()
            .map(|f| {
                let class = f.type_name.rsplit('.').next().unwrap_or(&f.type_name);
                ((class.to_owned(), f.name.clone()), f.role)
            })
            .collect()
    }
}

fn debug(message: impl FnOnce() -> String) {
    if std::env::var_os("GANG_DEBUG").is_some() {
        eprintln!("gang: {}", message());
    }
}

/// The analyzer's view of the project's scripts, or `None` when no Unity
/// editor with a compiler is installed (or the analysis failed; the remap
/// then uses the text rules).
pub fn analyze(repo: &Repo, project: &UnityProject) -> Option<Analysis> {
    if std::env::var_os("GANG_NO_COMPILER").is_some() {
        return None;
    }
    let root = project.path(repo.root());
    let sources = sources(&root);
    if sources.is_empty() {
        return Some(Analysis::default());
    }
    let mut dirs = unity::editor_data_dirs(project.editor_version.as_deref());
    dirs.dedup();
    debug(|| format!("Unity editors with data folders: {dirs:?}"));
    for data in dirs {
        let Some(tools) = Tools::find(&data) else {
            debug(|| format!("no .NET runtime or C# compiler in {}", data.display()));
            continue;
        };
        let Some(analyzer) = tools.build(&repo.gang_dir().join("cache")) else {
            debug(|| format!("could not build the C# analyzer with {}", data.display()));
            continue;
        };
        match run(&tools, &analyzer, &root, &sources, repo.root()) {
            Some(analysis) => return Some(analysis),
            None => continue,
        }
    }
    debug(|| "no Unity editor with a C# compiler found; using the text rules".to_owned());
    None
}

/// The editor's .NET runtime and compiler.
struct Tools {
    data: PathBuf,
    dotnet: PathBuf,
    csc: PathBuf,
    framework: PathBuf,
    framework_version: String,
}

impl Tools {
    fn find(data: &Path) -> Option<Tools> {
        let runtime = data.join("NetCoreRuntime");
        let dotnet = runtime.join(if cfg!(windows) {
            "dotnet.exe"
        } else {
            "dotnet"
        });
        let csc = data.join("DotNetSdkRoslyn").join("csc.dll");
        let shared = runtime.join("shared").join("Microsoft.NETCore.App");
        let mut versions: Vec<String> = fs::read_dir(&shared)
            .ok()?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        versions.sort();
        let framework_version = versions.pop()?;
        if !dotnet.is_file() || !csc.is_file() {
            return None;
        }
        Some(Tools {
            data: data.to_owned(),
            dotnet,
            csc,
            framework: shared.join(&framework_version),
            framework_version,
        })
    }

    fn roslyn(&self, name: &str) -> PathBuf {
        self.data.join("DotNetSdkRoslyn").join(name)
    }

    /// Compiles the analyzer once per analyzer source and editor, into the
    /// user's cache.
    fn build(&self, fallback: &Path) -> Option<PathBuf> {
        let key = {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in SOURCE
                .bytes()
                .chain(self.data.to_string_lossy().bytes())
                .chain(self.framework_version.bytes())
            {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
            format!("{hash:016x}")
        };
        // The user's cache, else this repository's GANG folder (the user
        // profile may be unusual, as in tests or services).
        let cache = cache_roots(fallback)
            .into_iter()
            .find(|dir| fs::create_dir_all(dir).is_ok())?
            .join(key);
        let dll = cache.join("gang-analyzer.dll");
        if dll.is_file() {
            return Some(dll);
        }
        let staging = cache.with_extension(format!("tmp{}", std::process::id()));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).ok()?;
        fs::write(staging.join("GangAnalyzer.cs"), SOURCE).ok()?;
        let mut rsp = vec![
            "-nologo".to_owned(),
            "-nostdlib".to_owned(),
            "-langversion:latest".to_owned(),
            "-nullable:enable".to_owned(),
            "-target:exe".to_owned(),
            "-optimize+".to_owned(),
            format!("-out:\"{}\"", staging.join("gang-analyzer.dll").display()),
        ];
        for entry in fs::read_dir(&self.framework).ok()?.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "dll") && is_managed(&path) {
                rsp.push(format!("-r:\"{}\"", path.display()));
            }
        }
        for name in [
            "Microsoft.CodeAnalysis.dll",
            "Microsoft.CodeAnalysis.CSharp.dll",
        ] {
            let from = self.roslyn(name);
            rsp.push(format!("-r:\"{}\"", from.display()));
            fs::copy(&from, staging.join(name)).ok()?;
        }
        rsp.push(format!("\"{}\"", staging.join("GangAnalyzer.cs").display()));
        fs::write(staging.join("build.rsp"), rsp.join("\n")).ok()?;
        let output = hidden(Command::new(&self.dotnet))
            .arg(&self.csc)
            .arg(format!("@{}", staging.join("build.rsp").display()))
            .output()
            .ok()?;
        if !output.status.success() {
            debug(|| {
                format!(
                    "could not build the C# analyzer: {}",
                    String::from_utf8_lossy(&output.stdout)
                )
            });
            let _ = fs::remove_dir_all(&staging);
            return None;
        }
        let config = format!(
            "{{\"runtimeOptions\":{{\"tfm\":\"net6.0\",\"framework\":{{\"name\":\"Microsoft.NETCore.App\",\"version\":\"{}\"}},\"rollForward\":\"Major\"}}}}",
            self.framework_version
        );
        fs::write(staging.join("gang-analyzer.runtimeconfig.json"), config).ok()?;
        if fs::rename(&staging, &cache).is_err() {
            // Another GANG built it at the same moment.
            let _ = fs::remove_dir_all(&staging);
        }
        dll.is_file().then_some(dll)
    }
}

fn cache_roots(fallback: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("GANG_CACHE") {
        roots.push(PathBuf::from(dir).join("analyzer"));
    }
    if let Some(dirs) = directories::ProjectDirs::from("", "", "gang") {
        roots.push(dirs.cache_dir().join("analyzer"));
    }
    roots.push(fallback.join("analyzer"));
    roots
}

/// Whether a file in the runtime folder is a .NET assembly (the folder also
/// holds the runtime's native libraries).
fn is_managed(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    // PE header → optional header → data directory 14 (the CLI header).
    let read_u32 = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let read_u16 = |at: usize| -> Option<u16> {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let Some(pe) = read_u32(0x3c).map(|p| p as usize) else {
        return false;
    };
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        return false;
    }
    let optional = pe + 24;
    let directories = match read_u16(optional) {
        Some(0x10b) => optional + 96,
        Some(0x20b) => optional + 112,
        _ => return false,
    };
    read_u32(directories + 14 * 8).is_some_and(|rva| rva != 0)
}

/// The project's C# files: `Assets/` and embedded packages, without the
/// folders Unity ignores.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for top in ["Assets", "Packages"] {
        collect(&root.join(top), &mut files);
    }
    files.sort();
    files
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with('~') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if name.ends_with(".cs") {
            files.push(path);
        }
    }
}

/// Assembly names the project's own sources build (their compiled copies in
/// `Library/ScriptAssemblies` would clash with the sources).
fn own_assemblies(root: &Path) -> BTreeSet<String> {
    let mut asmdefs = Vec::new();
    for top in ["Assets", "Packages"] {
        collect_ext(&root.join(top), ".asmdef", &mut asmdefs);
    }
    asmdefs
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .filter_map(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_owned))
        .collect()
}

fn collect_ext(dir: &Path, ext: &str, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with('~') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_ext(&path, ext, files);
        } else if name.ends_with(ext) {
            files.push(path);
        }
    }
}

fn references(tools: &Tools, root: &Path) -> Vec<PathBuf> {
    let mut refs = Vec::new();
    // The module assemblies (and the UnityEngine / UnityEditor facades
    // that forward to them); the monolithic Managed/UnityEngine.dll would
    // define every type twice.
    let managed = tools.data.join("Managed");
    for dir in [managed.join("UnityEngine")] {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let unity = name.starts_with("UnityEngine") || name.starts_with("UnityEditor");
                if unity && name.ends_with(".dll") {
                    refs.push(path);
                }
            }
        }
    }
    let netstandard = tools
        .data
        .join("NetStandard")
        .join("ref")
        .join("2.1.0")
        .join("netstandard.dll");
    refs.push(netstandard);
    let own = own_assemblies(root);
    if let Ok(entries) = fs::read_dir(root.join("Library").join("ScriptAssemblies")) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".dll") else {
                continue;
            };
            if stem.starts_with("Assembly-CSharp") || own.contains(stem) {
                continue;
            }
            refs.push(entry.path());
        }
    }
    // Unity's own modules first, so a facade never hides them.
    refs.sort_by_key(|p| !p.starts_with(managed.join("UnityEngine")));
    refs.dedup_by(|a, b| a.file_name() == b.file_name());
    refs
}

fn run(
    tools: &Tools,
    analyzer: &Path,
    root: &Path,
    sources: &[PathBuf],
    repo_root: &Path,
) -> Option<Analysis> {
    let defines = [
        "UNITY_EDITOR",
        "UNITY_STANDALONE",
        "UNITY_5_3_OR_NEWER",
        "UNITY_2017_1_OR_NEWER",
        "UNITY_2018_1_OR_NEWER",
        "UNITY_2019_1_OR_NEWER",
        "UNITY_2020_1_OR_NEWER",
        "UNITY_2021_1_OR_NEWER",
        "UNITY_2022_1_OR_NEWER",
        "UNITY_6000_0_OR_NEWER",
    ];
    let request = serde_json::json!({
        "files": sources.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "references": references(tools, root).iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "defines": defines,
    });
    let path = std::env::temp_dir().join(format!(
        "gang-analyzer-{}-{}.json",
        std::process::id(),
        crate::clock::now()
    ));
    fs::write(&path, request.to_string()).ok()?;
    let output = hidden(Command::new(&tools.dotnet))
        .arg(analyzer)
        .arg(&path)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .output()
        .ok()?;
    let _ = fs::remove_file(&path);
    if !output.status.success() {
        debug(|| {
            format!(
                "the C# analyzer failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        return None;
    }
    let mut analysis: Analysis = serde_json::from_slice(&output.stdout).ok()?;
    let base = repo_root.to_string_lossy().replace('\\', "/");
    let base = base.trim_end_matches('/').to_owned() + "/";
    for u in &mut analysis.uses {
        let file = u.file.replace('\\', "/");
        u.file = if file.len() >= base.len() && file[..base.len()].eq_ignore_ascii_case(&base) {
            file[base.len()..].to_owned()
        } else {
            file
        };
    }
    debug(|| {
        format!(
            "C# analyzer: {} files, {} compile errors, {} layer uses{}",
            analysis.files,
            analysis.errors,
            analysis.uses.len(),
            analysis
                .messages
                .iter()
                .map(|m| format!(
                    "
  {m}"
                ))
                .collect::<String>()
        )
    });
    Some(analysis)
}

/// No console window for the runtime on Windows.
fn hidden(mut command: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

/// What a set of layer moves does to one script.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rewrite {
    pub text: String,
    /// Lines changed (1-based).
    pub changed: Vec<usize>,
    /// `(line, why)`
    pub review: Vec<(usize, String)>,
}

/// Rewrites the numbers the analysis traced in `text` (the script at
/// `path`): only on `added` lines (our own work), and only literals.
/// Everything else that touches a moved layer is listed for review.
pub fn rewrite(
    text: &str,
    uses: &[&Use],
    added: &BTreeSet<usize>,
    moves: &[(u32, u32)],
) -> Rewrite {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    let mut out = Rewrite::default();
    // Right to left within a line, so earlier columns stay valid.
    let mut ordered: Vec<&&Use> = uses.iter().collect();
    ordered.sort_by_key(|u| (u.line, std::cmp::Reverse(u.column)));
    for u in ordered {
        let ours = added.contains(&u.line);
        let Some(value) = u.value else {
            if ours && (u.kind == "unknown" || u.kind == "mixed") {
                out.review.push((
                    u.line,
                    format!(
                        "{} reaches {} with a value GANG cannot follow; check it",
                        u.text, u.via
                    ),
                ));
            }
            continue;
        };
        let replacement = match u.role {
            Role::Index => moves
                .iter()
                .find(|(from, _)| i64::from(*from) == value)
                .map(|(_, to)| i64::from(*to)),
            Role::Mask => {
                let bits = value as u64 as u32;
                if !moves.iter().any(|(from, _)| bits & (1 << from) != 0) {
                    None
                } else if moves
                    .iter()
                    .any(|(from, to)| bits & (1 << from) != 0 && bits & (1 << to) != 0)
                {
                    out.review.push((
                        u.line,
                        format!(
                            "the mask {} ({}) covers both the moved layer and its new number; left as is",
                            u.text, u.via
                        ),
                    ));
                    continue;
                } else {
                    let mut moved = bits;
                    for (from, to) in moves {
                        if bits & (1 << from) != 0 {
                            moved = (moved & !(1 << from)) | (1 << to);
                        }
                    }
                    Some(i64::from(moved))
                }
            }
            Role::Mixed => None,
        };
        let Some(replacement) = replacement else {
            continue;
        };
        if u.kind != "literal" {
            let why = match u.kind.as_str() {
                "implicit-enum" => "an enum member numbered by its position",
                "external" => "a constant from a compiled library",
                _ => "a computed constant",
            };
            out.review.push((
                u.line,
                format!("{} ({why}) reaches {}; left as is", u.text, u.via),
            ));
            continue;
        }
        if !ours {
            out.review.push((
                u.line,
                format!(
                    "existing code gives {} to {}; it was not written for the moved layer, so it was left as is",
                    u.text, u.via
                ),
            ));
            continue;
        }
        let Some(line) = lines.get_mut(u.line - 1) else {
            continue;
        };
        let Some(start) = locate(line, u.column, &u.text) else {
            out.review.push((
                u.line,
                format!("{} reaches {}; could not rewrite it", u.text, u.via),
            ));
            continue;
        };
        let new_text = format_like(&u.text, replacement);
        line.replace_range(start..start + u.text.len(), &new_text);
        if out.changed.last() != Some(&u.line) {
            out.changed.push(u.line);
        }
    }
    out.text = lines.concat();
    out.changed.sort();
    out.changed.dedup();
    out
}

/// The byte offset of `text` at UTF-16 column `column` of `line`, or the
/// nearest whole-token match.
fn locate(line: &str, column: usize, text: &str) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in line.char_indices() {
        if units == column {
            if line[byte..].starts_with(text) {
                return Some(byte);
            }
            break;
        }
        units += ch.len_utf16();
    }
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(text).map(|(i, _)| i).find(|&i| {
        let before = line[..i].chars().next_back();
        let after = line[i + text.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// `value` written the way `original` was: decimal, hex (keeping the case
/// of its digits) or binary, with the same suffix.
pub fn format_like(original: &str, value: i64) -> String {
    let lower = original.to_ascii_lowercase();
    let suffix_len = lower
        .chars()
        .rev()
        .take_while(|c| matches!(c, 'u' | 'l'))
        .count();
    let (body, suffix) = original.split_at(original.len() - suffix_len);
    if let Some(digits) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        let prefix = &body[..2];
        let upper = digits.chars().any(|c| c.is_ascii_uppercase());
        let hex = if upper {
            format!("{value:X}")
        } else {
            format!("{value:x}")
        };
        return format!("{prefix}{hex}{suffix}");
    }
    if let Some(_digits) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        let prefix = &body[..2];
        return format!("{prefix}{value:b}{suffix}");
    }
    format!("{value}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(line: usize, column: usize, text: &str, role: Role, value: i64) -> Use {
        Use {
            file: "A.cs".into(),
            line,
            column,
            text: text.into(),
            role,
            value: Some(value),
            via: "Physics.Raycast(layerMask)".into(),
            kind: "literal".into(),
        }
    }

    #[test]
    fn formats_like_the_original() {
        assert_eq!(format_like("256", 512), "512");
        assert_eq!(format_like("0x100", 512), "0x200");
        assert_eq!(format_like("0X1F0", 0x3f0), "0X3F0");
        assert_eq!(format_like("256u", 512), "512u");
        assert_eq!(format_like("0b100000000", 512), "0b1000000000");
        assert_eq!(format_like("8", 9), "9");
    }

    #[test]
    fn rewrites_only_our_literals() {
        let text = "class A\n{\n    const int M = 1 << 8;\n    int mask = 0x100 | 256;\n    int old = 8;\n}\n";
        let uses = [
            usage(3, 23, "8", Role::Index, 8),
            usage(4, 15, "0x100", Role::Mask, 256),
            usage(4, 23, "256", Role::Mask, 256),
            usage(5, 14, "8", Role::Index, 8),
        ];
        let refs: Vec<&Use> = uses.iter().collect();
        let added: BTreeSet<usize> = [3, 4].into_iter().collect();
        let result = rewrite(text, &refs, &added, &[(8, 9)]);
        assert_eq!(
            result.text,
            "class A\n{\n    const int M = 1 << 9;\n    int mask = 0x200 | 512;\n    int old = 8;\n}\n"
        );
        assert_eq!(result.changed, vec![3, 4]);
        assert_eq!(result.review.len(), 1);
        assert!(result.review[0].1.starts_with("existing code"));
    }

    #[test]
    fn a_mask_with_both_bits_is_left_for_review() {
        let text = "int m = 768;\n";
        let uses = [usage(1, 8, "768", Role::Mask, 768)];
        let refs: Vec<&Use> = uses.iter().collect();
        let added: BTreeSet<usize> = [1].into_iter().collect();
        let result = rewrite(text, &refs, &added, &[(8, 9)]);
        assert_eq!(result.text, text);
        assert_eq!(result.review.len(), 1);
    }

    #[test]
    fn finds_assemblies() {
        let exe = std::env::current_exe().unwrap();
        assert!(!is_managed(&exe));
    }
}
