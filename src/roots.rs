//! Store root detection and installed-game discovery.
//!
//! Replaces Ludusavi's `steamlocate`-based launcher detection
//! (`src/launchers/steam.rs`) with a native implementation:
//! we probe every drive letter for Steam libraries, follow
//! `libraryfolders.vdf` for additional libraries, read any
//! existing Ludusavi config roots, and parse
//! `appmanifest_*.acf` files for installed games.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::manifest::Store;
use crate::paths::normalize;

/// A save root: a base path associated with a store,
/// mirroring Ludusavi's `Root` concept.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Root {
    pub path: String,
    pub store: Store,
}

/// An installed Steam game discovered from an appmanifest ACF file.
#[derive(Clone, Debug)]
pub struct SteamGame {
    pub app_id: u32,
    pub name: String,
    /// Folder name under `steamapps/common`.
    pub install_dir: String,
    /// The Steam library root this game belongs to.
    pub library: String,
}

/// Detect store roots on this system (Windows-focused subset of
/// Ludusavi's root detection, plus scanning all drive letters).
pub fn detect_roots() -> Vec<Root> {
    let mut roots: Vec<Root> = vec![];
    let mut seen: HashSet<String> = HashSet::new();

    let push = |roots: &mut Vec<Root>, seen: &mut HashSet<String>, path: String, store: Store| {
        let path = normalize(&path);
        let key = path.to_lowercase();
        if Path::new(&path).is_dir() && seen.insert(key) {
            roots.push(Root { path, store });
        }
    };

    for steam_root in steam_roots() {
        push(&mut roots, &mut seen, steam_root, Store::Steam);
    }

    // Common store roots on every present drive letter.
    for drive in drive_letters() {
        for (sub, store) in [
            ("GOG Games", Store::Gog),
            ("GOG Galaxy/Games", Store::GogGalaxy),
            ("Program Files (x86)/GOG Galaxy/Games", Store::GogGalaxy),
            ("Epic Games", Store::Epic),
            ("Program Files/Epic Games", Store::Epic),
            ("Program Files (x86)/Epic Games", Store::Epic),
            ("Program Files/EA Games", Store::Ea),
            ("Program Files (x86)/EA Games", Store::Ea),
            ("Program Files (x86)/Origin Games", Store::Origin),
            ("Origin Games", Store::Origin),
            (
                "Program Files (x86)/Ubisoft/Ubisoft Game Launcher/games",
                Store::Uplay,
            ),
            ("Ubisoft Game Launcher/games", Store::Uplay),
            ("Program Files/WindowsApps", Store::Microsoft),
            ("XboxGames", Store::Microsoft),
        ] {
            push(&mut roots, &mut seen, format!("{drive}:/{sub}"), store);
        }
    }

    // Any roots configured in the user's own Ludusavi installation.
    for root in ludusavi_config_roots() {
        push(&mut roots, &mut seen, root.path, root.store);
    }

    roots
}

/// Drive letters that exist on this machine (`C` ..= `Z`).
fn drive_letters() -> Vec<char> {
    ('A'..='Z')
        .filter(|d| Path::new(&format!("{d}:/")).is_dir())
        .collect()
}

/// Find every Steam root: the primary install plus all library folders.
/// Probes `X:/Steam`, `X:/SteamLibrary`, `X:/Games/Steam`, etc. on every
/// drive, then follows `libraryfolders.vdf` to discover more.
fn steam_roots() -> Vec<String> {
    let mut found: Vec<String> = vec![];
    let mut queue: Vec<String> = vec![];

    for drive in drive_letters() {
        for sub in [
            "Steam",
            "SteamLibrary",
            "Games/Steam",
            "Games/SteamLibrary",
            "Program Files/Steam",
            "Program Files (x86)/Steam",
        ] {
            let path = format!("{drive}:/{sub}");
            if Path::new(&path).join("steamapps").is_dir() {
                queue.push(normalize(&path));
            }
        }
    }

    while let Some(root) = queue.first().cloned() {
        queue.remove(0);
        let key = root.to_lowercase();
        if found.iter().any(|x: &String| x.to_lowercase() == key) {
            continue;
        }
        found.push(root.clone());

        for vdf in [
            PathBuf::from(&root).join("config").join("libraryfolders.vdf"),
            PathBuf::from(&root).join("steamapps").join("libraryfolders.vdf"),
        ] {
            let Ok(content) = std::fs::read_to_string(&vdf) else {
                continue;
            };
            for library in parse_vdf_values(&content, "path") {
                let library = normalize(&library);
                if Path::new(&library).join("steamapps").is_dir()
                    && !queue.iter().any(|x| x.to_lowercase() == library.to_lowercase())
                    && !found.iter().any(|x| x.to_lowercase() == library.to_lowercase())
                {
                    queue.push(library);
                }
            }
        }
    }

    found
}

/// Read `roots` from the user's own Ludusavi config, if they have one
/// (`%APPDATA%/ludusavi/config.yaml`). This matches the store/path pairs
/// real Ludusavi would scan.
fn ludusavi_config_roots() -> Vec<Root> {
    #[derive(serde::Deserialize)]
    struct ConfigRoots {
        #[serde(default)]
        roots: Vec<ConfigRoot>,
    }
    #[derive(serde::Deserialize)]
    struct ConfigRoot {
        store: Store,
        path: String,
    }

    // `roots:` come from the app's own (portable) config.yaml — same
    // schema as Ludusavi's, seeded from it on first run.
    let config_file = crate::config::Config::file_path();
    let Ok(content) = std::fs::read_to_string(&config_file) else {
        return vec![];
    };
    let Ok(parsed) = serde_yaml::from_str::<ConfigRoots>(&content) else {
        return vec![];
    };
    parsed
        .roots
        .into_iter()
        .map(|r| Root {
            path: normalize(&r.path),
            store: r.store,
        })
        .collect()
}

/// List installed Steam games across the given Steam roots by parsing
/// `steamapps/appmanifest_*.acf`.
pub fn steam_games(steam_roots: &[&Root]) -> Vec<SteamGame> {
    let mut games = vec![];

    for root in steam_roots {
        let steamapps = Path::new(&root.path).join("steamapps");
        let Ok(entries) = std::fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let app_id = parse_vdf_values(&content, "appid")
                .first()
                .and_then(|x| x.parse::<u32>().ok());
            let name = parse_vdf_values(&content, "name").first().cloned();
            let install_dir = parse_vdf_values(&content, "installdir").first().cloned();
            if let (Some(app_id), Some(name), Some(install_dir)) = (app_id, name, install_dir) {
                games.push(SteamGame {
                    app_id,
                    name,
                    install_dir,
                    library: root.path.clone(),
                });
            }
        }
    }

    games
}

/// Minimal Valve KeyValues (VDF/ACF) line parser: collects every value
/// whose key matches `key`, e.g. `"path"  "D:\\SteamLibrary"`.
fn parse_vdf_values(content: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{}\"", key.to_lowercase());
    let mut values = vec![];
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.to_lowercase().starts_with(&needle) {
            continue;
        }
        let rest = &trimmed[needle.len()..];
        let mut chunks = rest.split('"');
        chunks.next(); // between key and value
        if let Some(value) = chunks.next() {
            values.push(value.replace("\\\\", "\\"));
        }
    }
    values
}
