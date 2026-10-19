//! `.gang/config.toml`, the team's GANG settings (DESIGN.md §19).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const PATH: &str = ".gang/config.toml";

/// The file `gang init` writes.
pub const DEFAULT_TEXT: &str = r#"# GANG settings for this project. Shared by the whole team.

[presence]
poll_interval = "15s"
debounce = "4s"
heartbeat = "60s"
offline_after = "5m"

[locks]
default_level = 1
confirm_over = 10
auto_allowed = ["binary", "unity"]
auto_ttl = "8h"

[scenes]
lighting = "lock"

[merge]
float_epsilon = 1e-6
ui_order_conflicts = "ask"

[layers]
auto_remap = true

[sync]
strategy = "merge"
editor_open = "block"

[lfs]
# Store images, models, audio and other large binaries in Git LFS.
# GitHub's LFS storage is paid and has a quota; off keeps GANG free.
enabled = false
"#;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub project: ProjectConfig,
    pub presence: PresenceConfig,
    pub locks: LocksConfig,
    pub scenes: ScenesConfig,
    pub merge: MergeConfig,
    pub layers: LayersConfig,
    pub sync: SyncConfig,
    pub lfs: LfsConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    /// The branch flags are compared against when a branch has no upstream.
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresenceConfig {
    pub poll_interval: String,
    pub debounce: String,
    pub heartbeat: String,
    pub offline_after: String,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        PresenceConfig {
            poll_interval: "15s".into(),
            debounce: "4s".into(),
            heartbeat: "60s".into(),
            offline_after: "5m".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocksConfig {
    pub default_level: u8,
    pub confirm_over: usize,
    pub auto_allowed: Vec<String>,
    pub auto_ttl: String,
}

impl Default for LocksConfig {
    fn default() -> Self {
        LocksConfig {
            default_level: 1,
            confirm_over: 10,
            auto_allowed: vec!["binary".into(), "unity".into()],
            auto_ttl: "8h".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScenesConfig {
    pub lighting: String,
    pub policy: std::collections::BTreeMap<String, String>,
}

impl Default for ScenesConfig {
    fn default() -> Self {
        ScenesConfig {
            lighting: "lock".into(),
            policy: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MergeConfig {
    pub float_epsilon: f64,
    pub ui_order_conflicts: String,
    pub noise: Vec<NoiseRule>,
}

impl Default for MergeConfig {
    fn default() -> Self {
        MergeConfig {
            float_epsilon: 1e-6,
            ui_order_conflicts: "ask".into(),
            noise: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoiseRule {
    pub class: String,
    pub script: Option<String>,
    pub path: String,
    pub rule: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LayersConfig {
    pub auto_remap: bool,
}

impl Default for LayersConfig {
    fn default() -> Self {
        LayersConfig { auto_remap: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    pub strategy: String,
    pub editor_open: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            strategy: "merge".into(),
            editor_open: "block".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LfsConfig {
    /// `None` when the setting is missing: projects set up before the
    /// setting existed keep what their `.gitattributes` already does.
    pub enabled: Option<bool>,
}

/// Whether the project stores large binaries in Git LFS: the `[lfs]`
/// setting, or for older projects without it, whether the managed
/// `.gitattributes` already sends files to LFS.
pub fn lfs_enabled(repo_root: &Path, config: &Config) -> bool {
    config.lfs.enabled.unwrap_or_else(|| {
        std::fs::read_to_string(repo_root.join(".gitattributes"))
            .map(|text| text.lines().any(|l| l.contains("filter=lfs")))
            .unwrap_or(false)
    })
}

impl Config {
    /// Reads `.gang/config.toml` from the repository root; defaults when the
    /// file does not exist.
    pub fn load(repo_root: &Path) -> Result<Config> {
        let path = repo_root.join(PATH);
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text).with_context(|| format!("invalid {PATH}")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(err).with_context(|| format!("could not read {PATH}")),
        }
    }

    pub fn parse(text: &str) -> Result<Config> {
        Ok(toml::from_str(text)?)
    }
}

/// Parses `15s`, `4m`, `8h`, `2d` or a bare number of seconds.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .with_context(|| format!("invalid duration {text:?}"))?;
    let seconds = match unit.trim() {
        "" | "s" => number,
        "m" => number * 60,
        "h" => number * 3600,
        "d" => number * 86400,
        other => bail!("invalid duration unit {other:?} in {text:?} (use s, m, h or d)"),
    };
    Ok(Duration::from_secs(seconds))
}

/// Reads one dotted key (`locks.auto_ttl`) from config text.
pub fn get(text: &str, key: &str) -> Result<Option<String>> {
    let doc: toml_edit::DocumentMut = text.parse()?;
    let mut item = doc.as_item();
    for part in key.split('.') {
        match item.get(part) {
            Some(next) => item = next,
            None => return Ok(None),
        }
    }
    Ok(Some(match item.as_value() {
        Some(toml_edit::Value::String(s)) => s.value().clone(),
        Some(value) => value.to_string().trim().to_owned(),
        None => item.to_string().trim().to_owned(),
    }))
}

/// Sets one dotted key in config text, keeping the rest of the formatting.
/// `value` is read as TOML when it parses (`true`, `10`, `["a"]`), otherwise
/// as a string. The result must still be a valid configuration.
pub fn set(text: &str, key: &str, value: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let parts: Vec<&str> = key.split('.').collect();
    let (last, tables) = parts.split_last().context("empty key")?;
    let mut table = doc.as_table_mut();
    for part in tables {
        let entry = table
            .entry(part)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        table = entry
            .as_table_mut()
            .with_context(|| format!("{part} is not a table"))?;
    }
    let parsed: toml_edit::Value = value
        .parse()
        .unwrap_or_else(|_| toml_edit::Value::from(value));
    table[*last] = toml_edit::value(parsed);
    let result = doc.to_string();
    Config::parse(&result).with_context(|| format!("{key} = {value} is not a valid setting"))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_matches_defaults() {
        let parsed = Config::parse(DEFAULT_TEXT).unwrap();
        let defaults = Config::default();
        assert_eq!(parsed.locks.auto_allowed, defaults.locks.auto_allowed);
        assert_eq!(
            parsed.presence.poll_interval,
            defaults.presence.poll_interval
        );
        assert_eq!(parsed.sync.editor_open, defaults.sync.editor_open);
        assert_eq!(parsed.merge.float_epsilon, defaults.merge.float_epsilon);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[locks]\nauto_alowed = []\n").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("15s").unwrap(), Duration::from_secs(15));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("8h").unwrap(), Duration::from_secs(28800));
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("5y").is_err());
    }

    #[test]
    fn get_and_set_keep_formatting() {
        assert_eq!(
            get(DEFAULT_TEXT, "locks.auto_ttl").unwrap().as_deref(),
            Some("8h")
        );
        assert_eq!(
            get(DEFAULT_TEXT, "locks.default_level").unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(get(DEFAULT_TEXT, "locks.nope").unwrap(), None);

        let text = set(DEFAULT_TEXT, "locks.auto_ttl", "12h").unwrap();
        assert!(text.starts_with("# GANG settings for this project."));
        assert_eq!(
            get(&text, "locks.auto_ttl").unwrap().as_deref(),
            Some("12h")
        );

        let text = set(&text, "locks.auto_allowed", r#"["binary"]"#).unwrap();
        assert_eq!(
            Config::parse(&text).unwrap().locks.auto_allowed,
            vec!["binary"]
        );

        let text = set(&text, "project.default_branch", "develop").unwrap();
        assert_eq!(
            Config::parse(&text)
                .unwrap()
                .project
                .default_branch
                .as_deref(),
            Some("develop")
        );

        assert!(set(DEFAULT_TEXT, "locks.default_level", "high").is_err());
    }
}
