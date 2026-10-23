//! Manifest cache metadata.
//!
//! Adopts Ludusavi's `cache.yaml` schema (`src/resource/cache.rs`) so this
//! app and Ludusavi share manifest update bookkeeping:
//! `%APPDATA%/ludusavi/cache.yaml` on Windows.
//!
//! Only the `manifests` section is typed (etag + checked/updated
//! timestamps). Everything else (version, release, migrations, roots,
//! backup/restore recent games, ...) is preserved verbatim.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::manifest::MANIFEST_URL;

/// Ludusavi's `Cache` — partial typing, full preservation.
/// Note: `cache.yaml` keeps snake_case keys (no `rename_all` upstream).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    /// Per-URL manifest bookkeeping: `{etag, checked, updated}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub manifests: BTreeMap<String, ManifestCache>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

/// Ludusavi's `cache::Manifest` entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManifestCache {
    /// HTTP ETag used for `If-None-Match` revalidation.
    pub etag: Option<String>,
    /// When the manifest was last checked for updates.
    pub checked: Option<chrono::DateTime<chrono::Utc>>,
    /// When the manifest content last actually changed.
    pub updated: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

impl Cache {
    /// `cache.yaml` inside the shared Ludusavi app-data dir.
    pub fn file_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("ludusavi").join("cache.yaml")
    }

    pub fn load() -> Self {
        let path = Self::file_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_yaml::from_str::<Cache>(&content).ok())
            .unwrap_or_default()
    }

    /// Write via temp file + rename, like `Config::save`.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::file_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let content = serde_yaml::to_string(self).map_err(|e| format!("cannot serialize cache: {e}"))?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, content).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    }

    /// Record the result of a manifest update, mirroring
    /// Ludusavi's `Cache::update_manifest`.
    pub fn update_manifest(&mut self, etag: Option<String>, timestamp: chrono::DateTime<chrono::Utc>, modified: bool) {
        let entry = self.manifests.entry(MANIFEST_URL.to_string()).or_default();
        entry.etag = etag;
        entry.checked = Some(timestamp);
        if modified {
            entry.updated = Some(timestamp);
        }
    }

    /// ETag + timestamps for the primary manifest (for the Settings UI).
    pub fn manifest_info(&self) -> (Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>) {
        self.manifests
            .get(MANIFEST_URL)
            .map(|m| (m.checked, m.updated))
            .unwrap_or_default()
    }
}
