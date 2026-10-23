//! Manifest schema and loading.
//!
//! Adapted from Ludusavi's `src/resource/manifest.rs` (MIT licensed).
//! The manifest is a big YAML file derived from PCGamingWiki that maps
//! game titles to their save/config locations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::i18n::{tr, trf};

/// Primary manifest URL, same as Ludusavi's default
/// (see `src/resource/config.rs` in the Ludusavi repo).
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

pub const MANIFEST_FILE_NAME: &str = "manifest.yaml";

/// Path placeholders used by manifest entries.
/// Copied from Ludusavi's `resource/manifest.rs` `placeholder` module.
pub mod placeholder {
    pub const ROOT: &str = "<root>";
    pub const GAME: &str = "<game>";
    pub const BASE: &str = "<base>";
    pub const HOME: &str = "<home>";
    pub const STORE_GAME_ID: &str = "<storeGameId>";
    pub const STORE_USER_ID: &str = "<storeUserId>";
    pub const OS_USER_NAME: &str = "<osUserName>";
    pub const WIN_APP_DATA: &str = "<winAppData>";
    pub const WIN_LOCAL_APP_DATA: &str = "<winLocalAppData>";
    pub const WIN_LOCAL_APP_DATA_LOW: &str = "<winLocalAppDataLow>";
    pub const WIN_DOCUMENTS: &str = "<winDocuments>";
    pub const WIN_PUBLIC: &str = "<winPublic>";
    pub const WIN_PROGRAM_DATA: &str = "<winProgramData>";
    pub const WIN_DIR: &str = "<winDir>";
    pub const XDG_DATA: &str = "<xdgData>";
    pub const XDG_CONFIG: &str = "<xdgConfig>";
}

/// Sentinel for path segments that cannot be resolved on this system.
/// Same trick as Ludusavi's `SKIP`: it keeps the `<` so the candidate
/// gets filtered out at the end of path parsing.
pub const SKIP: &str = "<skip>";

/// Result of a manifest update check (`Manifest::update`).
#[derive(Clone, Debug)]
pub struct ManifestUpdate {
    /// The ETag to store for the next `If-None-Match` check.
    pub etag: Option<String>,
    /// Whether new content was downloaded (false on HTTP 304).
    pub modified: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Os {
    Windows,
    Linux,
    Mac,
    #[default]
    #[serde(other)]
    Other,
}

impl Os {
    const WINDOWS: bool = cfg!(target_os = "windows");
    const MAC: bool = cfg!(target_os = "macos");
    const LINUX: bool = cfg!(target_os = "linux");
    pub const HOST: Os = if Self::WINDOWS {
        Self::Windows
    } else if Self::MAC {
        Self::Mac
    } else if Self::LINUX {
        Self::Linux
    } else {
        Self::Other
    };

    #[allow(dead_code)]
    pub fn is_case_sensitive(&self) -> bool {
        match self {
            Self::Windows | Self::Mac => false,
            Self::Linux | Self::Other => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Store {
    Ea,
    Epic,
    Faugus,
    Gog,
    GogGalaxy,
    Heroic,
    Legendary,
    Lutris,
    Microsoft,
    Origin,
    Prime,
    Steam,
    Uplay,
    OtherHome,
    OtherWine,
    OtherWindows,
    OtherLinux,
    OtherMac,
    #[default]
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tag {
    Save,
    Config,
    #[default]
    #[serde(other)]
    Other,
}

/// The whole manifest: map of game title -> game data.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Manifest(pub BTreeMap<String, Game>);

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Game {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, GameFileEntry>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub install_dir: BTreeMap<String, GameInstallDirEntry>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub registry: BTreeMap<String, GameRegistryEntry>,
    #[serde(skip_serializing_if = "SteamMetadata::is_empty")]
    pub steam: SteamMetadata,
    #[serde(skip_serializing_if = "GogMetadata::is_empty")]
    pub gog: GogMetadata,
    #[serde(skip_serializing_if = "IdMetadata::is_empty")]
    pub id: IdMetadata,
}

impl Game {
    /// All Steam IDs that may apply to this game (primary + extras).
    pub fn steam_ids(&self, shortcut: Option<u32>) -> Vec<u32> {
        let mut out = vec![];
        if let Some(id) = self.steam.id {
            out.push(id);
        }
        out.extend(self.id.steam_extra.iter().copied());
        if let Some(id) = shortcut {
            out.push(id);
        }
        out
    }

    pub fn gog_ids(&self) -> Vec<u64> {
        let mut out = vec![];
        if let Some(id) = self.gog.id {
            out.push(id);
        }
        out.extend(self.id.gog_extra.iter().copied());
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameFileEntry {
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<Tag>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub when: BTreeSet<GameFileConstraint>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GameInstallDirEntry {}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameRegistryEntry {
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub tags: BTreeSet<Tag>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub when: BTreeSet<GameRegistryConstraint>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameFileConstraint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<Os>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<Store>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameRegistryConstraint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<Store>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SteamMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
}

impl SteamMetadata {
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GogMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

impl GogMetadata {
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IdMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatpak: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub gog_extra: BTreeSet<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lutris: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub steam_extra: BTreeSet<u32>,
}

impl IdMetadata {
    pub fn is_empty(&self) -> bool {
        self.flatpak.is_none() && self.gog_extra.is_empty() && self.lutris.is_none() && self.steam_extra.is_empty()
    }
}

impl Manifest {
    pub fn parse(content: &str) -> Result<Self, String> {
        serde_yaml::from_str(content).map_err(|e| format!("Failed to parse manifest: {e}"))
    }

    /// Load the manifest, trying in order:
    /// 1. A local cached copy next to our data (`cache/manifest.yaml`).
    /// 2. Ludusavi's own cached manifest (`%APPDATA%/ludusavi/manifest.yaml`).
    /// 3. Downloading from the primary URL (and caching it locally).
    ///
    /// A file that is missing, unreadable, unparsable, or parses to an
    /// empty manifest (e.g. a truncated write) falls through to the next
    /// source instead of silently producing a zero-game scan.
    ///
    /// `status` is invoked with progress messages for the UI.
    pub fn load(cache_dir: &Path, status: &mut dyn FnMut(String)) -> Result<Self, String> {
        let cached = cache_dir.join(MANIFEST_FILE_NAME);
        let shared = Self::ludusavi_manifest_path().filter(|p| p.is_file());

        for path in [cached.clone()].into_iter().chain(shared) {
            if !path.is_file() {
                continue;
            }
            status(trf("scan.loading", &[("path", &path.display())]));
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|content| Self::parse(&content))
            {
                Ok(m) if !m.0.is_empty() => {
                    // Keep our local cache in sync with the shared one.
                    if path != cached {
                        let _ = std::fs::create_dir_all(cache_dir);
                        let _ = std::fs::copy(&path, &cached);
                    }
                    return Ok(m);
                }
                Ok(_) => status(trf("scan.corrupt", &[("path", &path.display())])),
                Err(e) => status(trf(
                    "scan.unusable",
                    &[("path", &path.display()), ("err", &e)],
                )),
            }
        }

        status(tr("scan.downloading"));
        let content = Self::download()?;
        let parsed = Self::parse(&content)?;
        if parsed.0.is_empty() {
            return Err(tr("scan.download_err"));
        }
        std::fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
        // Temp + rename so a killed process can't leave a truncated file.
        let tmp = cached.with_extension("yaml.tmp");
        std::fs::write(&tmp, &content).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &cached).map_err(|e| e.to_string())?;
        Ok(parsed)
    }

    /// Read a response body to a string. `into_string()` caps out at
    /// ~10 MiB — the manifest is ~17 MB, so stream the bytes instead.
    fn read_body(response: ureq::Response) -> Result<String, String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Failed to read manifest response: {e}"))?;
        String::from_utf8(bytes).map_err(|e| format!("Manifest was not valid UTF-8: {e}"))
    }

    fn download() -> Result<String, String> {
        let response = ureq::get(MANIFEST_URL)
            .timeout(std::time::Duration::from_secs(180))
            .call()
            .map_err(|e| format!("Failed to download manifest: {e}"))?;
        Self::read_body(response)
    }

    /// Where Ludusavi itself caches the manifest (`%APPDATA%/ludusavi/manifest.yaml`).
    /// This is the shared cache both apps read — the update target for
    /// "Update Manifest".
    pub fn ludusavi_manifest_path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("ludusavi").join(MANIFEST_FILE_NAME))
    }

    /// Download/revalidate the primary manifest into `path` (the shared
    /// Ludusavi cache). Mirrors Ludusavi's `Manifest::update_one`:
    /// sends `If-None-Match` with the cached ETag, treats 304 as
    /// "checked but unchanged", and validates the YAML before writing.
    /// Returns the new ETag + whether the content was modified.
    pub fn update(path: &Path, old_etag: Option<&str>) -> Result<ManifestUpdate, String> {
        let mut request = ureq::get(MANIFEST_URL)
            .timeout(std::time::Duration::from_secs(180))
            .set("User-Agent", "game-save-checkpoint");
        if let Some(etag) = old_etag.filter(|_| path.exists()) {
            request = request.set("If-None-Match", etag);
        }

        match request.call() {
            Ok(response) => {
                let etag = response.header("ETag").map(|s| s.to_string());
                let content = Self::read_body(response)?;
                // Validate before overwriting the cache, like Ludusavi.
                let parsed = Self::parse(&content)?;
                if parsed.0.is_empty() {
                    return Err(tr("scan.download_err_keep"));
                }
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                let tmp = path.with_extension("yaml.tmp");
                std::fs::write(&tmp, &content).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
                Ok(ManifestUpdate {
                    etag,
                    modified: true,
                })
            }
            Err(ureq::Error::Status(304, _)) => Ok(ManifestUpdate {
                etag: old_etag.map(|s| s.to_string()),
                modified: false,
            }),
            Err(e) => Err(format!("Failed to download manifest: {e}")),
        }
    }

    /// Follow `alias` entries to the canonical game name,
    /// like Ludusavi's alias handling.
    pub fn resolve_alias(&self, name: &str) -> String {
        let mut current = name.to_string();
        for _ in 0..5 {
            match self.0.get(&current).and_then(|g| g.alias.clone()) {
                Some(target) if self.0.contains_key(&target) => current = target,
                _ => break,
            }
        }
        current
    }

    /// Build an index from Steam app ID to manifest game name.
    pub fn steam_index(&self) -> BTreeMap<u32, String> {
        let mut index = BTreeMap::new();
        for (name, game) in &self.0 {
            if let Some(id) = game.steam.id {
                index.entry(id).or_insert_with(|| name.clone());
            }
            for id in &game.id.steam_extra {
                index.entry(*id).or_insert_with(|| name.clone());
            }
        }
        index
    }

    /// Build an index from install directory name to manifest game name.
    #[allow(dead_code)]
    pub fn install_dir_index(&self) -> BTreeMap<String, String> {
        let mut index = BTreeMap::new();
        for (name, game) in &self.0 {
            for dir in game.install_dir.keys() {
                index.entry(dir.clone()).or_insert_with(|| name.clone());
            }
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corrupt/empty cached manifest must not produce a "successful"
    /// zero-game scan — loading falls through to the shared Ludusavi
    /// cache (or a download if that's absent too).
    #[test]
    fn load_falls_through_corrupt_cache() {
        let dir = std::env::temp_dir().join("gsc-tests").join("manifest-load");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 0-byte file — the "Scan complete: 0 games" scenario.
        std::fs::write(dir.join(MANIFEST_FILE_NAME), "").unwrap();

        let mut log = vec![];
        let manifest = Manifest::load(&dir, &mut |s: String| log.push(s)).unwrap();
        assert!(!manifest.0.is_empty());
        // The fallback produced a visible status trail (never silent).
        // Text is localized now — just assert the user saw messages.
        assert!(!log.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
