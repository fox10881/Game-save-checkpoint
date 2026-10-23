//! Save scanning engine.
//!
//! Adapted from Ludusavi's `src/scan.rs`: `parse_paths` (manifest path
//! placeholder expansion into glob candidates) and `scan_game_for_backup`
//! (globbing + directory walking + hashing + change detection).
//! Windows-focused: the Wine/Proton/Flatpak branches are omitted.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::time::SystemTime;

use sha1::{Digest, Sha1};

use crate::backup::IndividualMappingFile;
use crate::manifest::{placeholder as p, Game, Os, Store, SKIP};
use crate::paths::{escape_glob, normalize, CommonPath, OS_USERNAME};
use crate::roots::{Root, SteamGame};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanChange {
    New,
    Different,
    Same,
    #[default]
    Unknown,
}

impl ScanChange {
    /// Like Ludusavi's `ScanChange::evaluate_backup`.
    fn evaluate_backup(current_hash: &str, previous: Option<&IndividualMappingFile>) -> Self {
        match previous {
            None => Self::New,
            Some(prev) if prev.hash == current_hash => Self::Same,
            Some(_) => Self::Different,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScannedFile {
    pub size: u64,
    pub hash: String,
    pub change: ScanChange,
    pub mtime: Option<SystemTime>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanInfo {
    pub game_name: String,
    /// Normalized full path -> file info.
    pub found_files: BTreeMap<String, ScannedFile>,
}

impl ScanInfo {
    pub fn total_size(&self) -> u64 {
        self.found_files.values().map(|f| f.size).sum()
    }

    pub fn all_same(&self) -> bool {
        !self.found_files.is_empty() && self.found_files.values().all(|f| f.change == ScanChange::Same)
    }
}

fn check_windows_path(path: Option<&str>) -> &str {
    path.unwrap_or(SKIP)
}

/// Expand one manifest file path into concrete glob candidates.
/// Adapted from Ludusavi's `parse_paths` (Windows branches).
#[allow(clippy::too_many_arguments)]
fn parse_paths(
    path: &str,
    root: &Root,
    install_dir: Option<&str>,
    full_install_dir: Option<&str>,
    steam_ids: &[u32],
    gog_ids: &[u64],
) -> HashSet<String> {
    let mut paths = HashSet::new();

    // Since STORE_USER_ID becomes `*`, avoid ending up with an invalid `**`.
    let path = path
        .replace(&format!("*{}", p::STORE_USER_ID), p::STORE_USER_ID)
        .replace(&format!("{}*", p::STORE_USER_ID), p::STORE_USER_ID);

    let install_dir = install_dir.map(escape_glob).unwrap_or_else(|| SKIP.to_string());
    let full_install_dir = full_install_dir.map(escape_glob).unwrap_or_else(|| SKIP.to_string());
    let root_globbable = escape_glob(&normalize(&root.path));

    let data_dir = check_windows_path(CommonPath::Data.get());
    let data_local_dir = check_windows_path(CommonPath::DataLocal.get());
    let data_local_low_dir = check_windows_path(CommonPath::DataLocalLow.get());
    let home = check_windows_path(CommonPath::Home.get());
    let document_dir = check_windows_path(CommonPath::Document.get());
    let public_dir = check_windows_path(CommonPath::Public.get());
    let program_data = check_windows_path(CommonPath::WindowsProgramData.get());
    let win_dir = check_windows_path(CommonPath::WindowsSystemRoot.get());

    paths.insert(
        path.replace(p::ROOT, &root_globbable)
            .replace(p::GAME, &install_dir)
            .replace(p::BASE, &full_install_dir)
            .replace(p::HOME, home)
            .replace(p::STORE_USER_ID, "*")
            .replace(p::OS_USER_NAME, &OS_USERNAME)
            .replace(p::WIN_APP_DATA, data_dir)
            .replace(p::WIN_LOCAL_APP_DATA, data_local_dir)
            .replace(p::WIN_LOCAL_APP_DATA_LOW, data_local_low_dir)
            .replace(p::WIN_DOCUMENTS, document_dir)
            .replace(p::WIN_PUBLIC, public_dir)
            .replace(p::WIN_PROGRAM_DATA, program_data)
            .replace(p::WIN_DIR, win_dir)
            // XDG placeholders are Linux-only; leave them unresolved so
            // the candidate is filtered out below (same as Ludusavi's SKIP).
            .replace(p::XDG_DATA, SKIP)
            .replace(p::XDG_CONFIG, SKIP),
    );

    // `<home>/Saved Games` is a distinct known folder on Windows.
    if let Some(saved_games) = CommonPath::SavedGames.get() {
        paths.insert(
            path.replace(p::GAME, &install_dir)
                .replace(p::BASE, &full_install_dir)
                .replace(p::ROOT, &root_globbable)
                .replace(p::STORE_USER_ID, "*")
                .replace(p::OS_USER_NAME, &OS_USERNAME)
                .replace("<home>/Saved Games/", &format!("{saved_games}/"))
                .replace("<home>\\Saved Games\\", &format!("{saved_games}/"))
                .replace(p::HOME, home)
                .replace(p::WIN_APP_DATA, data_dir)
                .replace(p::WIN_LOCAL_APP_DATA, data_local_dir)
                .replace(p::WIN_LOCAL_APP_DATA_LOW, data_local_low_dir)
                .replace(p::WIN_DOCUMENTS, document_dir)
                .replace(p::WIN_PUBLIC, public_dir)
                .replace(p::WIN_PROGRAM_DATA, program_data)
                .replace(p::WIN_DIR, win_dir),
        );
    }

    // Known-folder redirection (e.g. OneDrive) can move Documents away from
    // %USERPROFILE%\Documents, but many games still write to the legacy
    // location — check both when they differ.
    let legacy_documents = format!("{home}/Documents");
    if normalize(&legacy_documents) != normalize(document_dir) {
        paths.insert(
            path.replace(p::WIN_DOCUMENTS, &legacy_documents)
                .replace(p::ROOT, &root_globbable)
                .replace(p::GAME, &install_dir)
                .replace(p::BASE, &full_install_dir)
                .replace(p::HOME, home)
                .replace(p::STORE_USER_ID, "*")
                .replace(p::OS_USER_NAME, &OS_USERNAME)
                .replace(p::WIN_APP_DATA, data_dir)
                .replace(p::WIN_LOCAL_APP_DATA, data_local_dir)
                .replace(p::WIN_LOCAL_APP_DATA_LOW, data_local_low_dir)
                .replace(p::WIN_PUBLIC, public_dir)
                .replace(p::WIN_PROGRAM_DATA, program_data)
                .replace(p::WIN_DIR, win_dir)
                .replace(p::XDG_DATA, SKIP)
                .replace(p::XDG_CONFIG, SKIP),
        );
    }

    // UAC VirtualStore virtualization, like Ludusavi's VIRTUALIZED regex:
    // C:/(Program Files|Program Files (x86)|Windows|ProgramData)/...
    let virtualized: Vec<String> = paths
        .iter()
        .filter_map(|candidate| {
            let normalized = candidate.replace('\\', "/");
            for prefix in ["Program Files (x86)", "Program Files", "Windows", "ProgramData"] {
                let full = format!("C:/{prefix}/");
                if normalized.starts_with(&full) {
                    return Some(format!(
                        "{data_local_dir}/VirtualStore/{prefix}/{}",
                        &normalized[full.len()..]
                    ));
                }
            }
            None
        })
        .collect();
    paths.extend(virtualized);

    // Expand `<storeGameId>` for the relevant store.
    let paths = if path.contains(p::STORE_GAME_ID) {
        let mut expanded = HashSet::new();
        for candidate in paths {
            match root.store {
                Store::Gog | Store::GogGalaxy => {
                    for id in gog_ids {
                        expanded.insert(candidate.replace(p::STORE_GAME_ID, &id.to_string()));
                    }
                }
                Store::Steam => {
                    for id in steam_ids {
                        expanded.insert(candidate.replace(p::STORE_GAME_ID, &id.to_string()));
                    }
                }
                _ => continue,
            }
        }
        expanded
    } else {
        paths
    };

    // This excludes `SKIP` and any other unmatched placeholders.
    paths
        .into_iter()
        .filter(|candidate| !candidate.contains('<'))
        .map(|candidate| normalize(&candidate))
        .collect()
}

/// Scan one game for save files. Adapted from Ludusavi's
/// `scan_game_for_backup` (minus registry/Wine/cloud concerns).
pub fn scan_game_for_backup(
    name: &str,
    game: &Game,
    roots: &[Root],
    steam_games: &[SteamGame],
    previous_files: Option<&BTreeMap<String, IndividualMappingFile>>,
) -> ScanInfo {
    let mut found_files = BTreeMap::new();
    let mut paths_to_check: HashSet<String> = HashSet::new();

    let steam_ids = game.steam_ids(None);
    let gog_ids = game.gog_ids();

    // The games from Steam ACFs that correspond to this manifest entry.
    let matching_steam: Vec<&SteamGame> = steam_games
        .iter()
        .filter(|sg| steam_ids.contains(&sg.app_id))
        .collect();

    // Add a dummy root for checking paths without `<root>`.
    let mut roots_to_check: Vec<Root> = vec![Root {
        path: SKIP.to_string(),
        store: Store::Other,
    }];
    roots_to_check.extend(roots.iter().cloned());

    for root in &roots_to_check {
        for (raw_path, path_data) in &game.files {
            if raw_path.trim().is_empty() {
                continue;
            }
            // Skip entries that can only apply on another OS: every `when`
            // alternative pins a non-host OS (mirrors the effective result
            // of Ludusavi's placeholder resolution, which yields nothing
            // for these paths on Windows).
            if !path_data.when.is_empty()
                && path_data
                    .when
                    .iter()
                    .all(|c| c.os.is_some_and(|o| o != Os::HOST))
            {
                continue;
            }

            // Determine install dir candidates, like Ludusavi: launcher data
            // when available, otherwise the game name + manifest installDir.
            if root.store == Store::Steam && !matching_steam.is_empty() {
                for sg in &matching_steam {
                    if normalize(&sg.library) != normalize(&root.path) {
                        continue;
                    }
                    let full_install_dir = format!("{}/steamapps/common/{}", normalize(&root.path), sg.install_dir);
                    paths_to_check.extend(parse_paths(
                        raw_path,
                        root,
                        Some(&sg.install_dir),
                        Some(&full_install_dir),
                        &steam_ids,
                        &gog_ids,
                    ));
                }
            } else {
                let install_dirs: Vec<&str> = std::iter::once(name)
                    .chain(game.install_dir.keys().map(|k| k.as_str()))
                    .collect();
                for install_dir in install_dirs {
                    // Without launcher data, `<base>` becomes `<root>/**/<game>`
                    // only for game-directory style stores (GOG/Epic/...).
                    // A `**` walk over Steam libraries or WindowsApps would
                    // be too costly; Ludusavi likewise skips `<base>` when
                    // the launcher doesn't know the game.
                    let full_install_dir = format!(
                        "{}/**/{}",
                        normalize(&root.path),
                        escape_glob(install_dir)
                    );
                    let full = matches!(
                        root.store,
                        Store::Gog
                            | Store::GogGalaxy
                            | Store::Epic
                            | Store::Legendary
                            | Store::Uplay
                            | Store::Origin
                            | Store::Ea
                            | Store::Heroic
                            | Store::Prime
                    )
                    .then_some(full_install_dir);
                    paths_to_check.extend(parse_paths(
                        raw_path,
                        root,
                        Some(install_dir),
                        full.as_deref(),
                        &steam_ids,
                        &gog_ids,
                    ));
                }
            }
        }

        // Steam Cloud copies of saves, like Ludusavi.
        if root.store == Store::Steam {
            for id in &steam_ids {
                paths_to_check.insert(format!("{}/userdata/*/{}/remote", normalize(&root.path), id));
            }
        }
    }

    let match_options = glob::MatchOptions {
        case_sensitive: false,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };

    for pattern in paths_to_check {
        let Ok(globbed) = glob::glob_with(&pattern, match_options) else {
            continue;
        };
        for entry in globbed.flatten() {
            if entry.is_file() {
                record_file(&mut found_files, &entry, previous_files);
            } else if entry.is_dir() {
                for child in walkdir::WalkDir::new(&entry)
                    .max_depth(100)
                    .follow_links(true)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if child.file_type().is_file() {
                        record_file(&mut found_files, child.path(), previous_files);
                    }
                }
            }
        }
    }

    ScanInfo {
        game_name: name.to_string(),
        found_files,
    }
}

fn record_file(
    found: &mut BTreeMap<String, ScannedFile>,
    path: &std::path::Path,
    previous_files: Option<&BTreeMap<String, IndividualMappingFile>>,
) {
    let scan_key = normalize(&path.to_string_lossy());
    if found.contains_key(&scan_key) {
        return;
    }
    let Ok(metadata) = path.metadata() else {
        return;
    };
    let size = metadata.len();
    let mtime = metadata.modified().ok();
    let hash = sha1_file(path).unwrap_or_default();
    let change = ScanChange::evaluate_backup(&hash, previous_files.and_then(|prev| prev.get(&scan_key)));
    found.insert(scan_key, ScannedFile { size, hash, change, mtime });
}

/// Stream a file through SHA-1, like Ludusavi's `StrictPath::sha1`.
pub fn sha1_file(path: &std::path::Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::BackupLayout;
    use crate::manifest::Manifest;
    use crate::paths::normalize;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("gsc-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_backup_rescan_roundtrip() {
        let base = temp_dir("roundtrip");
        let saves = base.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("slot1.sav"), b"hello").unwrap();
        std::fs::write(saves.join("profile.dat"), b"data").unwrap();

        // Manifest entry whose path points at our temp "saves" dir.
        let yaml = format!(
            "Test Game:\n  files:\n    \"{}/saves\":\n      tags:\n        - save\n",
            normalize(&base.to_string_lossy()),
        );
        let manifest = Manifest::parse(&yaml).unwrap();
        let game = &manifest.0["Test Game"];

        // Scan: both files should be found as new.
        let scan = scan_game_for_backup("Test Game", game, &[], &[], None);
        assert_eq!(scan.found_files.len(), 2);
        assert!(scan.found_files.values().all(|f| f.change == ScanChange::New));

        // Backup: copies files and writes mapping.yaml.
        let layout = BackupLayout::new(base.join("backups"));
        std::fs::create_dir_all(&layout.base).unwrap();
        let report = layout.back_up_game(&scan);
        assert_eq!(report.files_copied, 2);
        let backup_name = report.backup_name.expect("backup should be created");
        let game_folder = layout.game_folder("Test Game");
        assert!(game_folder.join("mapping.yaml").is_file());
        let drive_dir = game_folder.join(&backup_name).join("drive-C");
        assert!(drive_dir.is_dir());

        // Rescan: everything unchanged -> Same, and a re-backup is skipped.
        let mapping = layout.load_mapping("Test Game").unwrap();
        let previous = mapping.latest_backup().map(|b| b.files.clone());
        let rescan = scan_game_for_backup("Test Game", game, &[], &[], previous.as_ref());
        assert!(rescan.all_same());
        let report2 = layout.back_up_game(&rescan);
        assert!(report2.backup_name.is_none());
        assert_eq!(report2.skipped_reason.as_deref(), Some("no changes since last backup"));

        // Modify one file -> Different -> new backup is created.
        std::fs::write(saves.join("slot1.sav"), b"changed!").unwrap();
        let rescan2 = scan_game_for_backup("Test Game", game, &[], &[], previous.as_ref());
        let changed = rescan2
            .found_files
            .iter()
            .find(|(k, _)| k.ends_with("slot1.sav"))
            .unwrap();
        assert_eq!(changed.1.change, ScanChange::Different);
        let report3 = layout.back_up_game(&rescan2);
        assert!(report3.backup_name.is_some());

        // Backed-up game is discoverable.
        let restorable = layout.restorable_games();
        assert_eq!(restorable.len(), 1);
        assert_eq!(restorable[0].0, "Test Game");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parse_paths_expands_placeholders() {
        use crate::manifest::Store;
        use crate::roots::Root;

        let root = Root {
            path: "C:/Games/SomeStore".to_string(),
            store: Store::Gog,
        };
        let candidates = parse_paths(
            "<root>/<game>/saves/<storeUserId>",
            &root,
            Some("My Game"),
            Some("C:/Games/SomeStore/My Game"),
            &[],
            &[],
        );
        assert!(candidates.contains("C:/Games/SomeStore/My Game/saves/*"));

        // Unresolvable placeholders are filtered out.
        let candidates = parse_paths("<xdgConfig>/game", &root, None, None, &[], &[]);
        assert!(candidates.is_empty());

        // <winAppData> resolves to the real roaming AppData.
        let candidates = parse_paths("<winAppData>/Studio/Game", &root, None, None, &[], &[]);
        assert!(candidates.iter().any(|c| c.contains("AppData/Roaming/Studio/Game")));
    }
}
