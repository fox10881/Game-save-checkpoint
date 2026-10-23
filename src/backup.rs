//! Backup layout and execution.
//!
//! Adapted from Ludusavi's `src/scan/layout.rs`. The on-disk format is
//! kept compatible: each game gets a folder containing `mapping.yaml`
//! plus `backup-<timestamp>/drive-C/...` file trees, so backups made
//! here can also be restored by Ludusavi itself.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::manifest::Os;
use crate::paths::{normalize, split_drive};
use crate::scanner::{ScanChange, ScanInfo, ScannedFile};

/// From Ludusavi's `prelude.rs`.
pub const INVALID_FILE_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|', '\0'];

const SAFE: &str = "_";

/// Copied from Ludusavi's `scan/layout.rs`.
pub fn escape_folder_name(name: &str) -> String {
    let mut escaped = String::from(name);

    // Leading dots would hide folders, and Windows Explorer has a bug
    // with trailing dots. This also covers the `.` and `..` cases.
    if escaped.starts_with('.') {
        escaped.replace_range(..1, SAFE);
    }
    if escaped.ends_with('.') {
        escaped.replace_range(escaped.len() - 1.., SAFE);
    }

    escaped.replace(INVALID_FILE_CHARS, SAFE)
}

/// Mirrors Ludusavi's `IndividualMappingFile`.
#[derive(Clone, Default, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IndividualMappingFile {
    pub hash: String,
    pub size: u64,
}

/// Mirrors Ludusavi's `FullBackup`. `children` holds the differential
/// backups nested under this full backup; `comment`, `locked`,
/// `semantics` and `registry` are preserved verbatim via `rest`.
/// (`Eq` is impossible while `rest` holds `serde_yaml::Value`.)
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FullBackup {
    pub name: String,
    pub when: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<Os>,
    pub files: BTreeMap<String, IndividualMappingFile>,
    /// Differential backups chained on this full backup.
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pub children: VecDeque<DifferentialBackup>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, serde_yaml::Value>,
}

/// Mirrors Ludusavi's `DifferentialBackup`. A `files` value of `None`
/// means the file was excluded in that snapshot.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DifferentialBackup {
    pub name: String,
    pub when: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<Os>,
    pub files: BTreeMap<String, Option<IndividualMappingFile>>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, serde_yaml::Value>,
}

/// Mirrors Ludusavi's `IndividualMapping` (`mapping.yaml`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IndividualMapping {
    pub name: String,
    pub drives: BTreeMap<String, String>,
    pub backups: VecDeque<FullBackup>,
}

impl IndividualMapping {
    pub fn new(name: String) -> Self {
        Self {
            name,
            ..Default::default()
        }
    }

    fn reversed_drives(&self) -> BTreeMap<String, String> {
        self.drives.iter().map(|(k, v)| (v.to_owned(), k.to_owned())).collect()
    }

    /// From Ludusavi: folder name for a drive, e.g. `C:` -> `drive-C`.
    pub fn drive_folder_name(&mut self, drive: &str) -> String {
        if let Some(mapped) = self.reversed_drives().get(drive) {
            return mapped.clone();
        }
        let key = if drive.is_empty() {
            "drive-0".to_string()
        } else {
            format!("drive-{}", escape_folder_name(&drive.replace(':', "")))
        };
        self.drives.insert(key.clone(), drive.to_string());
        key
    }

    pub fn latest_backup(&self) -> Option<&FullBackup> {
        self.backups.back()
    }

    pub fn load(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        serde_yaml::from_str(&content).ok()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let content = serde_yaml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(path, content).map_err(|e| e.to_string())
    }
}

/// Result of backing up one game.
#[derive(Clone, Debug)]
pub struct BackupReport {
    pub game_name: String,
    pub backup_name: Option<String>,
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub skipped_reason: Option<String>,
    pub errors: Vec<String>,
    /// How many older backups were pruned by retention limits.
    pub pruned: usize,
}

/// Result of restoring one game from its latest backup.
#[derive(Clone, Debug)]
pub struct RestoreReport {
    pub game_name: String,
    /// Which `backup-<timestamp>` was restored.
    pub backup_name: Option<String>,
    pub files_restored: usize,
    pub bytes_restored: u64,
    pub skipped_reason: Option<String>,
    pub errors: Vec<String>,
}

/// One row of a game's on-disk backup history.
#[derive(Clone, Debug)]
pub struct HistoryRow {
    /// `true` = differential snapshot nested under a full backup.
    pub differential: bool,
    /// Folder name (`backup-<ts>`) — the restore target identifier.
    pub name: String,
    pub when: chrono::DateTime<chrono::Utc>,
    /// Files recorded in this snapshot (differentials exclude `None`
    /// tombstones from the count).
    pub files: usize,
    /// Logical size of the snapshot — sum of recorded file sizes.
    pub size: u64,
}

/// Simplified version of Ludusavi's `BackupLayout`.
pub struct BackupLayout {
    pub base: PathBuf,
}

impl BackupLayout {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn game_folder(&self, game_name: &str) -> PathBuf {
        self.base.join(escape_folder_name(game_name))
    }

    fn mapping_file(game_folder: &Path) -> PathBuf {
        game_folder.join("mapping.yaml")
    }

    pub fn load_mapping(&self, game_name: &str) -> Option<IndividualMapping> {
        IndividualMapping::load(&Self::mapping_file(&self.game_folder(game_name)))
    }

    /// Latest backup timestamp for a game, if any.
    #[allow(dead_code)]
    pub fn latest_backup_time(&self, game_name: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        self.load_mapping(game_name).and_then(|m| m.latest_backup().map(|b| b.when))
    }

    /// Every snapshot for a game, newest first: the full backups in
    /// `mapping.yaml` plus the differentials nested under each.
    pub fn backup_history(&self, game_name: &str) -> Vec<HistoryRow> {
        let Some(mapping) = self.load_mapping(game_name) else {
            return vec![];
        };
        let mut rows = Vec::new();
        for b in &mapping.backups {
            rows.push(HistoryRow {
                differential: false,
                name: b.name.clone(),
                when: b.when,
                files: b.files.len(),
                size: b.files.values().map(|f| f.size).sum(),
            });
            for d in &b.children {
                rows.push(HistoryRow {
                    differential: true,
                    name: d.name.clone(),
                    when: d.when,
                    files: d.files.values().flatten().count(),
                    size: d.files.values().flatten().map(|f| f.size).sum(),
                });
            }
        }
        rows.sort_by(|a, b| b.when.cmp(&a.when));
        rows
    }

    /// Scan the backup directory for games that already have backups,
    /// like Ludusavi's `BackupLayout::load`.
    pub fn restorable_games(&self) -> Vec<(String, chrono::DateTime<chrono::Utc>)> {
        let mut games = vec![];
        let Ok(entries) = std::fs::read_dir(&self.base) else {
            return games;
        };
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let mapping_file = Self::mapping_file(&entry.path());
            if let Some(mapping) = IndividualMapping::load(&mapping_file)
                && let Some(latest) = mapping.latest_backup()
            {
                games.push((mapping.name.clone(), latest.when));
            }
        }
        games
    }

    /// Perform a full backup of one game's scanned files.
    /// Adapted from Ludusavi's `GameLayout::back_up` (full backups only,
    /// loose files, no zip/differential).
    pub fn back_up_game(&self, scan: &ScanInfo) -> BackupReport {
        let mut report = BackupReport {
            game_name: scan.game_name.clone(),
            backup_name: None,
            files_copied: 0,
            bytes_copied: 0,
            skipped_reason: None,
            errors: vec![],
            pruned: 0,
        };

        if scan.found_files.is_empty() {
            report.skipped_reason = Some("no save files found".to_string());
            return report;
        }

        let game_folder = self.game_folder(&scan.game_name);
        let mut mapping = self
            .load_mapping(&scan.game_name)
            .unwrap_or_else(|| IndividualMapping::new(scan.game_name.clone()));

        // Like Ludusavi's constructive backups: skip if nothing changed.
        if mapping.latest_backup().is_some() && scan.all_same() {
            report.skipped_reason = Some("no changes since last backup".to_string());
            return report;
        }

        let now = chrono::Utc::now();
        let backup_name = format!("backup-{}", now.format("%Y%m%dT%H%M%SZ"));

        let mut backed_up_files: BTreeMap<String, IndividualMappingFile> = BTreeMap::new();

        for (original_path, file) in &scan.found_files {
            if file.change == ScanChange::Unknown {
                continue;
            }
            let (drive, plain_path) = split_drive(original_path);
            let drive_folder = mapping.drive_folder_name(&drive);
            let target = game_folder.join(&backup_name).join(&drive_folder).join(&plain_path);

            if let Some(parent) = target.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                report.errors.push(format!("{original_path}: {e}"));
                continue;
            }
            match std::fs::copy(Path::new(&normalize(original_path)), &target) {
                Ok(bytes) => {
                    report.files_copied += 1;
                    report.bytes_copied += bytes;
                    backed_up_files.insert(
                        original_path.clone(),
                        IndividualMappingFile {
                            hash: file.hash.clone(),
                            size: file.size,
                        },
                    );
                }
                Err(e) => report.errors.push(format!("{original_path}: {e}")),
            }
        }

        if backed_up_files.is_empty() {
            report.skipped_reason = Some("no files could be copied".to_string());
            return report;
        }

        mapping.backups.push_back(FullBackup {
            name: backup_name.clone(),
            when: now,
            os: Some(Os::HOST),
            files: backed_up_files,
            children: VecDeque::new(),
            rest: BTreeMap::new(),
        });

        if let Err(e) = mapping.save(&Self::mapping_file(&game_folder)) {
            report.errors.push(format!("failed to save mapping.yaml: {e}"));
        }

        report.backup_name = Some(backup_name);
        report
    }

    /// Enforce the retention limit for one game: keep the newest `full`
    /// backups and delete the rest, both from `mapping.yaml` and disk.
    /// Mirrors Ludusavi's `GameLayout::back_up` retention pruning.
    pub fn enforce_retention(&self, game_name: &str, full: u8) -> usize {
        let game_folder = self.game_folder(game_name);
        let Some(mut mapping) = self.load_mapping(game_name) else {
            return 0;
        };
        let keep = full.max(1) as usize;
        if mapping.backups.len() <= keep {
            return 0;
        }

        let mut removed = 0;
        while mapping.backups.len() > keep {
            let Some(old) = mapping.backups.pop_front() else {
                break;
            };
            let dir = game_folder.join(&old.name);
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            removed += 1;
        }
        let _ = mapping.save(&Self::mapping_file(&game_folder));
        removed
    }

    /// Compute the effective restore plan for snapshot `target`:
    /// `file -> (info, source folder)`. A full backup contributes all
    /// its files from its own folder; a differential replays its parent
    /// full plus every intervening differential — `Some` entries
    /// override, `None` tombstones delete — so the result is the state
    /// at that point in time. Returns `None` when `target` is unknown.
    fn restore_plan(
        mapping: &IndividualMapping,
        target: &str,
    ) -> Option<(String, BTreeMap<String, (IndividualMappingFile, String)>)> {
        for full in &mapping.backups {
            if full.name == target {
                return Some((
                    full.name.clone(),
                    full.files
                        .iter()
                        .map(|(k, v)| (k.clone(), (v.clone(), full.name.clone())))
                        .collect(),
                ));
            }
            let mut effective: BTreeMap<String, (IndividualMappingFile, String)> = full
                .files
                .iter()
                .map(|(k, v)| (k.clone(), (v.clone(), full.name.clone())))
                .collect();
            for child in &full.children {
                for (k, v) in &child.files {
                    match v {
                        Some(f) => {
                            effective.insert(k.clone(), (f.clone(), child.name.clone()));
                        }
                        None => {
                            effective.remove(k);
                        }
                    }
                }
                if child.name == target {
                    return Some((child.name.clone(), effective));
                }
            }
        }
        None
    }

    /// The newest snapshot overall — the last child of the last full,
    /// or the last full itself.
    fn newest_name(mapping: &IndividualMapping) -> Option<&str> {
        let last = mapping.backups.back()?;
        Some(&last.children.back().map(|c| &c.name).unwrap_or(&last.name))
    }

    /// Restore one game from a SPECIFIC backup name (`backup-<ts>`);
    /// `None` = newest snapshot. Reads `mapping.yaml` and copies each
    /// file from `<game>/<snapshot>/drive-<X>/<path>` back to its
    /// original absolute path, mirroring Ludusavi's restore.
    pub fn restore_game_version(&self, game_name: &str, backup_name: Option<&str>) -> RestoreReport {
        let mut report = RestoreReport {
            game_name: game_name.to_string(),
            backup_name: None,
            files_restored: 0,
            bytes_restored: 0,
            skipped_reason: None,
            errors: vec![],
        };

        let game_folder = self.game_folder(game_name);
        let Some(mapping) = self.load_mapping(game_name) else {
            report.skipped_reason = Some("no backup found".to_string());
            return report;
        };
        let Some(target) = backup_name
            .map(str::to_string)
            .or_else(|| Self::newest_name(&mapping).map(str::to_string))
        else {
            report.skipped_reason = Some("backup has no snapshots".to_string());
            return report;
        };
        let Some((resolved_name, effective)) = Self::restore_plan(&mapping, &target) else {
            report.skipped_reason = Some(format!("backup {target:?} not found"));
            return report;
        };
        report.backup_name = Some(resolved_name);

        // mapping.drives is folder-name -> drive letter ("drive-C" -> "C:");
        // invert it so we can locate each file's archived copy.
        let folder_for: BTreeMap<String, String> = mapping
            .drives
            .iter()
            .map(|(folder, drive)| (drive.clone(), folder.clone()))
            .collect();

        for (original_path, (_info, source_folder)) in &effective {
            let (drive, plain) = split_drive(original_path);
            let Some(drive_folder) = folder_for.get(&drive) else {
                report.errors.push(format!("{original_path}: unknown drive mapping {drive:?}"));
                continue;
            };
            let source = game_folder
                .join(source_folder)
                .join(drive_folder)
                .join(&plain);
            let target = Path::new(&normalize(original_path)).to_path_buf();

            if !source.is_file() {
                report.errors.push(format!("{original_path}: missing from backup"));
                continue;
            }
            if let Some(parent) = target.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                report.errors.push(format!("{original_path}: {e}"));
                continue;
            }
            match std::fs::copy(&source, &target) {
                Ok(bytes) => {
                    report.files_restored += 1;
                    report.bytes_restored += bytes;
                }
                Err(e) => report.errors.push(format!("{original_path}: {e}")),
            }
        }
        report
    }

    /// Whether the local scan is content-identical to ANY recorded
    /// snapshot: every file in some effective restore plan exists
    /// locally with the same hash+size, and the scan holds no extra
    /// files. `std::fs::copy` preserves the source mtime, so a
    /// just-restored file keeps the backup-era date — timestamps alone
    /// can't tell "old data" from "in sync", but hashes can. Checking
    /// every snapshot (not just the newest) means a deliberate older
    /// restore still counts as synchronized.
    pub fn is_synced(&self, game_name: &str, found: &BTreeMap<String, ScannedFile>) -> bool {
        let Some(mapping) = self.load_mapping(game_name) else {
            return false;
        };
        if found.is_empty() {
            return false;
        }
        let matches = |effective: &BTreeMap<String, (IndividualMappingFile, String)>| {
            effective.len() == found.len()
                && effective.iter().all(|(path, (info, _))| {
                    found
                        .get(path)
                        .is_some_and(|f| f.hash == info.hash && f.size == info.size)
                })
        };
        for full in &mapping.backups {
            let mut effective: BTreeMap<String, (IndividualMappingFile, String)> = full
                .files
                .iter()
                .map(|(k, v)| (k.clone(), (v.clone(), full.name.clone())))
                .collect();
            if matches(&effective) {
                return true;
            }
            for child in &full.children {
                for (k, v) in &child.files {
                    match v {
                        Some(f) => {
                            effective.insert(k.clone(), (f.clone(), child.name.clone()));
                        }
                        None => {
                            effective.remove(k);
                        }
                    }
                }
                if matches(&effective) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::ScannedFile;

    fn fake_scan(game: &str, files: &[&Path]) -> ScanInfo {
        let mut found = BTreeMap::new();
        for p in files {
            let key = normalize(&p.to_string_lossy());
            found.insert(
                key,
                ScannedFile {
                    size: std::fs::metadata(p).unwrap().len(),
                    hash: "x".to_string(),
                    change: ScanChange::New,
                    mtime: None,
                },
            );
        }
        ScanInfo {
            game_name: game.to_string(),
            found_files: found,
        }
    }

    #[test]
    fn restore_puts_files_back_at_original_paths() {
        let base = std::env::temp_dir().join("gsc-tests").join("restore");
        let _ = std::fs::remove_dir_all(&base);
        let saves = base.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let file_a = saves.join("slot1.sav");
        let file_b = saves.join("profile.dat");
        std::fs::write(&file_a, b"original-a").unwrap();
        std::fs::write(&file_b, b"original-b").unwrap();

        let layout = BackupLayout::new(base.join("backups"));
        std::fs::create_dir_all(&layout.base).unwrap();
        let report = layout.back_up_game(&fake_scan("Restore Game", &[&file_a, &file_b]));
        assert_eq!(report.files_copied, 2);

        // Destroy the originals.
        std::fs::write(&file_a, b"corrupted").unwrap();
        std::fs::remove_file(&file_b).unwrap();

        let restored = layout.restore_game_version("Restore Game", None);
        assert!(restored.backup_name.is_some());
        assert_eq!(restored.files_restored, 2);
        assert!(restored.errors.is_empty());
        assert_eq!(std::fs::read(&file_a).unwrap(), b"original-a");
        assert_eq!(std::fs::read(&file_b).unwrap(), b"original-b");

        // A game with no backup is skipped cleanly.
        let none = layout.restore_game_version("Never Backed Up", None);
        assert!(none.backup_name.is_none());
        assert_eq!(none.skipped_reason.as_deref(), Some("no backup found"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn backup_history_lists_full_and_differential() {
        let base = std::env::temp_dir().join("gsc-tests").join("history");
        let _ = std::fs::remove_dir_all(&base);
        let game_dir = base.join("SomeGame");
        std::fs::create_dir_all(&game_dir).unwrap();
        // Ludusavi schema: differentials nest under their full backup,
        // and a null file entry is an exclusion tombstone.
        std::fs::write(
            game_dir.join("mapping.yaml"),
            r#"---
name: SomeGame
drives: {}
backups:
  - name: backup-20240101T000000Z
    when: 2024-01-01T00:00:00Z
    os: windows
    files:
      "C:/Saves/a.sav": { hash: h1, size: 100 }
      "C:/Saves/b.sav": { hash: h2, size: 200 }
    children:
      - name: backup-20240102T000000Z
        when: 2024-01-02T00:00:00Z
        os: windows
        files:
          "C:/Saves/a.sav": { hash: h3, size: 150 }
          "C:/Saves/b.sav": ~
  - name: backup-20240103T000000Z
    when: 2024-01-03T00:00:00Z
    os: windows
    files:
      "C:/Saves/a.sav": { hash: h4, size: 120 }
"#,
        )
        .unwrap();

        let rows = BackupLayout::new(base.clone()).backup_history("SomeGame");
        assert_eq!(rows.len(), 3);
        // Newest first.
        assert_eq!(rows[0].when.to_rfc3339(), "2024-01-03T00:00:00+00:00");
        assert!(!rows[0].differential);
        assert_eq!((rows[0].files, rows[0].size), (1, 120));
        // The differential child: tombstones don't count as files.
        assert!(rows[1].differential);
        assert_eq!(rows[1].when.to_rfc3339(), "2024-01-02T00:00:00+00:00");
        assert_eq!((rows[1].files, rows[1].size), (1, 150));
        // The original full, with its children preserved.
        assert!(!rows[2].differential);
        assert_eq!((rows[2].files, rows[2].size), (2, 300));

        // Round-trip: children + unknown fields must survive
        // deserialize -> serialize, not be dropped silently.
        let mapping = BackupLayout::new(base.clone()).load_mapping("SomeGame").unwrap();
        let yaml = serde_yaml::to_string(&mapping).unwrap();
        let reparsed: IndividualMapping = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(reparsed.backups[0].children.len(), 1);
        assert_eq!(reparsed.backups[0].children[0].files["C:/Saves/b.sav"], None);

        assert!(BackupLayout::new(base.clone()).backup_history("NoGame").is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_specific_version_replays_chain() {
        let base = std::env::temp_dir().join("gsc-tests").join("versions");
        let _ = std::fs::remove_dir_all(&base);
        let saves = base.join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let save_dir = normalize(&saves.to_string_lossy());
        let game_dir = base.join("VerGame");
        // Backed-up files live at `drive-<X>/<path-after-drive>` — the
        // full path mirrors under the drive folder.
        let plain_dir = split_drive(&save_dir).1;
        // Filesystem: full holds a(v1)+b; diff holds a(v2)+c, and a
        // tombstone on b.
        let full_dir = game_dir.join("backup-20240101T000000Z");
        let diff_dir = game_dir.join("backup-20240102T000000Z");
        std::fs::create_dir_all(full_dir.join(format!("drive-C/{plain_dir}"))).unwrap();
        std::fs::create_dir_all(diff_dir.join(format!("drive-C/{plain_dir}"))).unwrap();
        std::fs::write(full_dir.join(format!("drive-C/{plain_dir}/a.sav")), b"a-v1").unwrap();
        std::fs::write(full_dir.join(format!("drive-C/{plain_dir}/b.sav")), b"b-v1").unwrap();
        std::fs::write(diff_dir.join(format!("drive-C/{plain_dir}/a.sav")), b"a-v2").unwrap();
        std::fs::write(diff_dir.join(format!("drive-C/{plain_dir}/c.sav")), b"c-v2").unwrap();
        std::fs::write(
            game_dir.join("mapping.yaml"),
            format!(
                r#"---
name: VerGame
drives:
  drive-C: "C:"
backups:
  - name: backup-20240101T000000Z
    when: 2024-01-01T00:00:00Z
    files:
      "{save_dir}/a.sav": {{ hash: h1, size: 4 }}
      "{save_dir}/b.sav": {{ hash: h2, size: 4 }}
    children:
      - name: backup-20240102T000000Z
        when: 2024-01-02T00:00:00Z
        files:
          "{save_dir}/a.sav": {{ hash: h3, size: 4 }}
          "{save_dir}/b.sav": ~
          "{save_dir}/c.sav": {{ hash: h4, size: 4 }}
"#
            ),
        )
        .unwrap();

        let layout = BackupLayout::new(base.clone());

        // Newest overall = the differential child.
        let r = layout.restore_game_version("VerGame", None);
        assert_eq!(r.backup_name.as_deref(), Some("backup-20240102T000000Z"));
        assert_eq!(std::fs::read(saves.join("a.sav")).unwrap(), b"a-v2");
        assert_eq!(std::fs::read(saves.join("c.sav")).unwrap(), b"c-v2");
        // Tombstoned file is not part of the diff snapshot — untouched.
        assert!(!saves.join("b.sav").exists());

        // Choosing the FULL backup restores the older state: a-v1 + b.
        std::fs::remove_file(saves.join("a.sav")).unwrap();
        std::fs::remove_file(saves.join("c.sav")).unwrap();
        let r = layout.restore_game_version("VerGame", Some("backup-20240101T000000Z"));
        assert_eq!(r.backup_name.as_deref(), Some("backup-20240101T000000Z"));
        assert_eq!(std::fs::read(saves.join("a.sav")).unwrap(), b"a-v1");
        assert_eq!(std::fs::read(saves.join("b.sav")).unwrap(), b"b-v1");

        // Unknown version name → clean skip, no panic, no writes.
        let r = layout.restore_game_version("VerGame", Some("backup-999"));
        assert!(r.backup_name.is_none());
        assert_eq!(r.skipped_reason.as_deref(), Some("backup \"backup-999\" not found"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `is_synced` compares the local scan against every snapshot's
    /// effective plan — hashes, not mtimes. Restored files keep their
    /// original timestamps, so a correct restore still shows an old
    /// "local save" date; this is the check that catches that.
    #[test]
    fn is_synced_compares_hashes_against_newest_snapshot() {
        let base = std::env::temp_dir().join("gsc-tests").join("synced");
        let _ = std::fs::remove_dir_all(&base);
        let game_dir = base.join("SyncGame");
        std::fs::create_dir_all(&game_dir).unwrap();
        std::fs::write(
            game_dir.join("mapping.yaml"),
            r#"---
name: SyncGame
drives: {}
backups:
  - name: backup-20240101T000000Z
    when: 2024-01-01T00:00:00Z
    files:
      "C:/Saves/a.sav": { hash: h1, size: 100 }
      "C:/Saves/b.sav": { hash: h2, size: 200 }
    children:
      - name: backup-20240102T000000Z
        when: 2024-01-02T00:00:00Z
        files:
          "C:/Saves/a.sav": { hash: h3, size: 150 }
          "C:/Saves/b.sav": ~
"#,
        )
        .unwrap();
        let layout = BackupLayout::new(base.clone());
        let file = |hash: &str, size: u64| ScannedFile {
            size,
            hash: hash.to_string(),
            change: ScanChange::Same,
            mtime: None,
        };
        let scan = |files: &[(&str, &str, u64)]| {
            files
                .iter()
                .map(|(p, h, s)| (p.to_string(), file(h, *s)))
                .collect::<BTreeMap<_, _>>()
        };

        // Newest = the differential: a@h3 only (b tombstoned).
        assert!(layout.is_synced("SyncGame", &scan(&[("C:/Saves/a.sav", "h3", 150)])));
        // The tombstoned file still being there → not synced.
        assert!(!layout.is_synced(
            "SyncGame",
            &scan(&[("C:/Saves/a.sav", "h3", 150), ("C:/Saves/b.sav", "h2", 200)])
        ));
        // The full backup's state IS a recorded snapshot — restoring an
        // older version still counts as synchronized.
        assert!(layout.is_synced(
            "SyncGame",
            &scan(&[("C:/Saves/a.sav", "h1", 100), ("C:/Saves/b.sav", "h2", 200)])
        ));
        // Changed hash, missing file, extra file, empty scan → false.
        assert!(!layout.is_synced("SyncGame", &scan(&[("C:/Saves/a.sav", "hx", 150)])));
        assert!(!layout.is_synced("SyncGame", &scan(&[])));
        // No backup at all → false.
        assert!(!layout.is_synced("NoGame", &scan(&[("C:/a", "h", 1)])));

        let _ = std::fs::remove_dir_all(&base);
    }
}
