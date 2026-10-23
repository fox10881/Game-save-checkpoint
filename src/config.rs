//! Configuration management.
//!
//! Adopts Ludusavi's native `config.yaml` schema (`src/resource/config.rs`)
//! so this app and Ludusavi share one settings file:
//! `%APPDATA%/ludusavi/config.yaml` on Windows.
//!
//! Only the sections this app uses are typed (`backup.path`,
//! `backup.retention`, `restore.path`, `roots`). Every other field is
//! preserved verbatim via `#[serde(flatten)]` catch-alls, so saving the
//! file never drops settings that only Ludusavi understands.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::paths::CommonPath;

/// Ludusavi's `Config` — partial typing, full preservation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Store roots (`- store: steam\n  path: ...`). Extra per-root fields
    /// (e.g. Lutris `database`) are kept in `rest`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<ConfigRoot>,
    pub backup: BackupConfig,
    pub restore: RestoreConfig,
    /// `customGames:` — shared with Ludusavi. `name`/`integration`/`files`
    /// are typed; per-game extras (ignore, alias, registry, installDir,
    /// winePrefix, preferAlias, ...) are preserved in `CustomGame::rest`.
    #[serde(default)]
    pub custom_games: Vec<CustomGame>,
    /// `language:` — Ludusavi's language tag (e.g. `en-US`, `ar-SA`).
    /// Typed so the Settings UI can edit it; empty = Ludusavi's default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// `theme:` — shared with Ludusavi (`theme: light|dark`). This app
    /// defaults to Dark (matching its dark-first UI) when the key is
    /// missing; Ludusavi itself defaults to Light.
    #[serde(default)]
    pub theme: Theme,
    /// `showGameIcons:` — this app's own toggle (not a Ludusavi field;
    /// upstream ignores unknown keys on load). Extract and display each
    /// game's exe icon in the list/details panel. Default off — exe
    /// probing costs disk I/O on large libraries.
    #[serde(default)]
    pub show_game_icons: bool,
    /// `verifiedSaves:` — game names the user manually confirmed as
    /// having correct save locations. Games in this set bypass the
    /// "config only / missing save data" orange warning state.
    /// App-specific field; Ludusavi ignores unknown keys on load.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub verified_saves: HashSet<String>,
    /// `cloud:` — shared with Ludusavi's cloud-sync block (rclone
    /// remote, target folder, auto-sync flag).
    #[serde(default)]
    pub cloud: Cloud,
    /// `apps:` — shared with Ludusavi's external-tool block
    /// (`apps.rclone.path` / `apps.rclone.arguments`).
    #[serde(default)]
    pub apps: Apps,
    /// Everything else (runtime, release, manifest, theme,
    /// redirects, scan, cloud, apps, ...) round-trips here.
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

/// Ludusavi's `Theme` (`src/resource/config.rs::Theme`) — serializes
/// `theme: light|dark`. Default here is Dark (this app's look);
/// Ludusavi's own default is Light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    Light,
    Dark,
}

impl Default for Theme {
    fn default() -> Self {
        Self::Dark
    }
}

/// Ludusavi's `CustomGame` — partial typing, full preservation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CustomGame {
    pub name: String,
    /// "override" replaces the manifest's paths; "extend" merges with them.
    /// Kept as a string so future Ludusavi variants round-trip untouched.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub integration: String,
    /// Literal file/directory paths this entry backs up.
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl CustomGame {
    /// Whether this entry should drive scanning: has files, isn't
    /// disabled (`ignore: true`), and isn't an alias-only entry.
    pub fn is_active(&self) -> bool {
        !self.files.is_empty()
            && self.rest.get("ignore") != Some(&Value::from(true))
            && self.rest.get("alias").is_none_or(Value::is_null)
    }

    /// `integration: extend` merges custom paths with the manifest's
    /// instead of replacing them.
    pub fn extends(&self) -> bool {
        self.integration == "extend"
    }
}

/// Ludusavi's `Cloud` (`src/resource/config.rs::Cloud`). `remote` stays
/// a raw `serde_yaml::Value` so every `Remote` variant — including the
/// credential-carrying Ftp/Smb/WebDav ones configured via Ludusavi —
/// round-trips byte-for-byte. Every variant carries an `id` field,
/// which is all the sync invocation needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Cloud {
    /// Rclone remote, externally tagged: `remote: {Custom: {id: "…"}}`.
    pub remote: Option<Value>,
    /// Cloud folder used for backups (`rclone` remote path).
    pub path: String,
    /// Upload automatically after backups (Ludusavi semantics).
    pub synchronize: bool,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl Default for Cloud {
    fn default() -> Self {
        Self {
            remote: None,
            path: "ludusavi-backup".to_string(),
            synchronize: true,
            rest: BTreeMap::new(),
        }
    }
}

/// Ludusavi's `Apps` — external tool settings. Only `rclone` is typed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Apps {
    pub rclone: App,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl Default for Apps {
    fn default() -> Self {
        Self {
            rclone: App::default(),
            rest: BTreeMap::new(),
        }
    }
}

/// Ludusavi's `App` — an external executable and its global flags.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct App {
    /// Path to `rclone(.exe)`; a bare name resolves via PATH.
    pub path: String,
    /// Global flags prepended (space-separated) to every invocation.
    pub arguments: String,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            path: "rclone".to_string(),
            arguments: "--fast-list --ignore-checksum".to_string(),
            rest: BTreeMap::new(),
        }
    }
}

/// One `roots:` entry. `store` stays a string so unknown store names
/// from future Ludusavi versions can't fail deserialization.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigRoot {
    pub store: String,
    pub path: String,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

/// Ludusavi's `BackupConfig` — path and retention are typed, everything
/// else (ignoredGames, filter, toggledPaths, toggledRegistry, sort,
/// format, onlyConstructive, ...) is preserved in `rest`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackupConfig {
    /// Full path to the directory backups are written to.
    pub path: String,
    #[serde(default)]
    pub retention: Retention,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

/// Ludusavi's `RestoreConfig` — path is typed, everything else
/// (ignoredGames, toggledPaths, toggledRegistry, sort,
/// reverseRedirects, ...) is preserved in `rest`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RestoreConfig {
    /// Full path to the directory restores are read from.
    pub path: String,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

/// Ludusavi's `Retention`: `full` 1-255, `differential` 0-255.
/// (`forceNewFull` is `serde(skip)` upstream and lands in `rest`.)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Retention {
    /// Full backups to keep per game.
    pub full: u8,
    /// Differential backups to keep per game.
    pub differential: u8,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            full: 1,
            differential: 0,
            rest: BTreeMap::new(),
        }
    }
}

impl Config {
    /// `config.yaml` inside the shared Ludusavi app-data dir
    /// (`CommonPath::Config` + `ludusavi`, matching Ludusavi's `app_dir()`).
    pub fn file_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("ludusavi").join("config.yaml")
    }

    /// Like Ludusavi's `default_backup_dir()`: `~/ludusavi-backup`.
    pub fn default_backup_dir() -> String {
        format!("{}/ludusavi-backup", CommonPath::Home.get().unwrap_or("C:/Users/Public"))
    }

    /// Load the shared config, falling back to defaults when the file
    /// is missing or unreadable, then normalizing like Ludusavi's
    /// `migrate`/`initialize` (empty paths get defaults, `full` >= 1).
    pub fn load() -> Self {
        let path = Self::file_path();
        let mut config = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_yaml::from_str::<Config>(&content).ok())
            .unwrap_or_default();
        config.fix();
        config
    }

    /// Fill in defaults for empty values, mirroring Ludusavi's migration.
    pub fn fix(&mut self) {
        if self.backup.path.trim().is_empty() {
            self.backup.path = Self::default_backup_dir();
        }
        if self.restore.path.trim().is_empty() {
            self.restore.path = self.backup.path.clone();
        }
        if self.backup.retention.full == 0 {
            self.backup.retention.full = 1;
        }
        if self.cloud.path.trim().is_empty() {
            self.cloud.path = Cloud::default().path;
        }
        if self.apps.rclone.path.trim().is_empty() {
            self.apps.rclone.path = App::default().path;
        }
    }

    /// Serialize back to `config.yaml`. Writes via a temp file + rename
    /// so a crash can't leave a half-written config.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::file_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let content = serde_yaml::to_string(self).map_err(|e| format!("cannot serialize config: {e}"))?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, content).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    }

    /// Read a config straight from a file (used by tests).
    #[allow(dead_code)]
    pub fn load_from(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let mut config = serde_yaml::from_str::<Config>(&content).ok()?;
        config.fix();
        Some(config)
    }

    /// The configured rclone remote's `id`. `cloud.remote` is an
    /// externally-tagged enum (`{Custom: {id: "…"}}`,
    /// `{GoogleDrive: {id: "…"}}`, …) — every variant carries `id`.
    pub fn cloud_remote_id(&self) -> Option<String> {
        let inner = self.cloud.remote.as_ref()?.as_mapping()?;
        for (_variant, fields) in inner {
            if let Some(id) = fields.get("id").and_then(Value::as_str)
                && !id.trim().is_empty()
            {
                return Some(id.trim().to_string());
            }
        }
        None
    }

    /// Set the rclone remote by name (empty clears it). If the name
    /// matches the existing remote's `id`, the typed variant (and any
    /// credentials/config it holds) is preserved; a *different* name
    /// writes Ludusavi's `Remote::Custom` shape: `{Custom: {id: …}}`.
    pub fn set_cloud_remote(&mut self, id: &str) {
        let id = id.trim();
        if id.is_empty() {
            self.cloud.remote = None;
            return;
        }
        if self.cloud_remote_id().as_deref() == Some(id) {
            return;
        }
        let mut fields = serde_yaml::Mapping::new();
        fields.insert(Value::from("id"), Value::from(id));
        let mut remote = serde_yaml::Mapping::new();
        remote.insert(Value::from("Custom"), Value::Mapping(fields));
        self.cloud.remote = Some(Value::Mapping(remote));
    }

    /// The custom save path configured for `game`, if an active
    /// `customGames` entry exists for it.
    pub fn custom_game_path(&self, game: &str) -> Option<&str> {
        self.custom_games
            .iter()
            .find(|g| g.name == game && g.is_active())
            .and_then(|g| g.files.first().map(String::as_str))
    }

    /// Add or replace the custom save path for `game`, writing an
    /// `integration: override` entry like Ludusavi's UI.
    pub fn set_custom_game_path(&mut self, game: &str, path: &str) {
        match self.custom_games.iter_mut().find(|g| g.name == game) {
            Some(entry) => {
                entry.files = vec![path.to_string()];
                if entry.integration.is_empty() {
                    entry.integration = "override".to_string();
                }
            }
            None => self.custom_games.push(CustomGame {
                name: game.to_string(),
                integration: "override".to_string(),
                files: vec![path.to_string()],
                rest: BTreeMap::new(),
            }),
        }
    }

    /// Remove every `customGames` entry for `game` — restores normal
    /// manifest scanning for it.
    pub fn remove_custom_game(&mut self, game: &str) {
        self.custom_games.retain(|g| g.name != game);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_matches_ludusavi_schema() {
        // `theme: light|dark` — the same string form Ludusavi writes.
        let c: Config = serde_yaml::from_str("---\ntheme: light\n").unwrap();
        assert_eq!(c.theme, Theme::Light);
        let c: Config = serde_yaml::from_str("---\ntheme: dark\n").unwrap();
        assert_eq!(c.theme, Theme::Dark);
        // Missing key -> this app's default (Dark).
        let c: Config = serde_yaml::from_str("---\n{}").unwrap();
        assert_eq!(c.theme, Theme::Dark);
        // Serializes back as the bare string.
        let v = serde_yaml::to_value(&c).unwrap();
        assert_eq!(v["theme"].as_str(), Some("dark"));
    }

    #[test]
    fn cloud_block_matches_ludusavi_schema() {
        let yaml = r#"---
cloud:
  remote:
    GoogleDrive:
      id: remote-id
  path: ludusavi-backup
  synchronize: false
apps:
  rclone:
    path: rclone.exe
    arguments: "--fast-list"
"#;
        let mut c: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(c.cloud_remote_id().as_deref(), Some("remote-id"));
        assert_eq!(c.cloud.path, "ludusavi-backup");
        assert!(!c.cloud.synchronize);
        assert_eq!(c.apps.rclone.path, "rclone.exe");
        assert_eq!(c.apps.rclone.arguments, "--fast-list");

        // Renaming to the SAME id keeps the GoogleDrive variant; a
        // different name becomes Remote::Custom.
        c.set_cloud_remote("remote-id");
        let v = serde_yaml::to_value(&c).unwrap();
        assert!(v["cloud"]["remote"]["GoogleDrive"].is_mapping());
        c.set_cloud_remote("other");
        let v = serde_yaml::to_value(&c).unwrap();
        assert_eq!(v["cloud"]["remote"]["Custom"]["id"].as_str(), Some("other"));

        // Empty name clears the remote; defaults populate via fix().
        let mut d: Config = serde_yaml::from_str("---\n{}").unwrap();
        d.fix();
        assert_eq!(d.cloud.path, "ludusavi-backup");
        assert!(d.cloud.synchronize);
        assert_eq!(d.apps.rclone.path, "rclone");
        assert!(d.apps.rclone.arguments.contains("--fast-list"));
        assert!(d.cloud_remote_id().is_none());
    }

    #[test]
    fn verified_saves_roundtrip() {
        // Loads as an optional list; serializes as `verifiedSaves`.
        let mut c: Config = serde_yaml::from_str("---\n{}").unwrap();
        assert!(c.verified_saves.is_empty());
        c.verified_saves.insert("Game A".to_string());
        c.verified_saves.insert("Game B".to_string());
        let yaml = serde_yaml::to_string(&c).unwrap();
        assert!(yaml.contains("verifiedSaves:"));
        let c2: Config = serde_yaml::from_str(&yaml).unwrap();
        assert!(c2.verified_saves.contains("Game A"));
        assert!(c2.verified_saves.contains("Game B"));
        // An empty set is omitted entirely (keeps config.yaml clean).
        let empty = serde_yaml::to_string(&Config::default()).unwrap();
        assert!(!empty.contains("verifiedSaves"));
    }

    #[test]
    fn roundtrip_preserves_unknown_fields() {
        let yaml = r#"---
runtime:
  threads: ~
  networkSecurity: safe
roots:
  - store: microsoft
    path: "C:/Program Files/WindowsApps"
  - store: lutris
    path: "~/.config/lutris"
    database: "~/.local/share/lutris/pga.db"
backup:
  path: "C:/Users/yahya/ludusavi-backup"
  ignoredGames: []
  filter:
    excludeStoreScreenshots: false
  toggledPaths: {}
  sort:
    key: status
    reversed: false
  retention:
    full: 3
    differential: 0
  onlyConstructive: false
restore:
  path: "D:\\ludusavi\\Backup-2"
  reverseRedirects: false
cloud:
  remote: ~
  path: ludusavi-backup
  synchronize: true
customGames: []
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.backup.path, "C:/Users/yahya/ludusavi-backup");
        assert_eq!(config.backup.retention.full, 3);
        assert_eq!(config.restore.path, "D:\\ludusavi\\Backup-2");
        assert_eq!(config.roots.len(), 2);
        assert_eq!(config.roots[1].rest["database"], Value::from("~/.local/share/lutris/pga.db"));

        // Unknown fields survive a round-trip.
        let out = serde_yaml::to_string(&config).unwrap();
        let reparsed: serde_yaml::Mapping = serde_yaml::from_str(&out).unwrap();
        assert!(reparsed.contains_key("runtime"));
        assert!(reparsed.contains_key("cloud"));
        assert!(reparsed.contains_key("customGames"));
        let backup = reparsed["backup"].as_mapping().unwrap();
        assert!(backup.contains_key("ignoredGames"));
        assert!(backup.contains_key("filter"));
        assert!(backup.contains_key("onlyConstructive"));
        let restore = reparsed["restore"].as_mapping().unwrap();
        assert_eq!(restore["reverseRedirects"], Value::from(false));
    }

    #[test]
    fn custom_game_path_roundtrip() {
        let mut config = Config::default();
        assert_eq!(config.custom_game_path("Palworld"), None);

        // New entry gets integration:override like Ludusavi's UI.
        config.set_custom_game_path("Palworld", "D:/My Saves/Palworld");
        assert_eq!(
            config.custom_game_path("Palworld"),
            Some("D:/My Saves/Palworld")
        );
        let entry = &config.custom_games[0];
        assert_eq!(entry.integration, "override");
        assert!(entry.is_active());
        assert!(!entry.extends());

        // Serializes as Ludusavi's `customGames` and survives a round trip.
        let out = serde_yaml::to_string(&config).unwrap();
        let reparsed: Config = serde_yaml::from_str(&out).unwrap();
        assert_eq!(
            reparsed.custom_game_path("Palworld"),
            Some("D:/My Saves/Palworld")
        );

        // An existing entry is updated in place.
        config.set_custom_game_path("Palworld", "E:/Elsewhere");
        assert_eq!(config.custom_games.len(), 1);
        assert_eq!(config.custom_game_path("Palworld"), Some("E:/Elsewhere"));

        // Removal restores manifest scanning.
        config.remove_custom_game("Palworld");
        assert_eq!(config.custom_game_path("Palworld"), None);
        assert!(config.custom_games.is_empty());
    }

    #[test]
    fn fix_fills_defaults() {
        let mut config = Config::default();
        config.fix();
        assert!(config.backup.path.ends_with("/ludusavi-backup"));
        assert_eq!(config.restore.path, config.backup.path);
        assert_eq!(config.backup.retention.full, 1);
    }
}
