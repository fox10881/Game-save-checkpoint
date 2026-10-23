mod backup;
mod cache;
mod cloud;
mod config;
mod i18n;
mod icons;
mod manifest;
mod paths;
mod roots;
mod scanner;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui::{
    self, Align, Color32, Frame, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Vec2,
};

use crate::backup::{BackupLayout, BackupReport};
use crate::config::Config;
use crate::i18n::{is_rtl, tr, trf};
use crate::manifest::{Game, Manifest};
use crate::paths::{human_bytes, normalize};
use crate::scanner::{ScanChange, ScanInfo};

// ---------- palette ----------
/// Theme-dependent colors. `colors()` returns the active palette — the
/// atomic flag is refreshed by `apply_theme` whenever `config.theme`
/// changes, so every widget below stays a plain `colors().field` read.
#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    panel_bg: Color32,
    card_bg: Color32,
    hover_bg: Color32,
    select_bg: Color32,
    border: Color32,
    accent: Color32,
    text_weak: Color32,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(15, 17, 21),
    panel_bg: Color32::from_rgb(21, 24, 30),
    card_bg: Color32::from_rgb(29, 33, 42),
    hover_bg: Color32::from_rgb(36, 41, 53),
    select_bg: Color32::from_rgb(39, 52, 73),
    border: Color32::from_rgb(46, 52, 66),
    accent: Color32::from_rgb(76, 139, 245),
    text_weak: Color32::from_rgb(139, 147, 161),
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(240, 242, 247),
    panel_bg: Color32::from_rgb(233, 236, 242),
    card_bg: Color32::from_rgb(255, 255, 255),
    hover_bg: Color32::from_rgb(220, 226, 236),
    select_bg: Color32::from_rgb(197, 216, 245),
    border: Color32::from_rgb(196, 203, 214),
    accent: Color32::from_rgb(52, 112, 205),
    text_weak: Color32::from_rgb(96, 104, 118),
};

/// `true` while the Dark palette is active.
static DARK_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn colors() -> Palette {
    if DARK_MODE.load(std::sync::atomic::Ordering::Relaxed) {
        DARK
    } else {
        LIGHT
    }
}

/// Apply `config.theme` globally: egui's base `Visuals` plus our
/// palette overrides, and the `DARK_MODE` flag `colors()` reads.
fn apply_theme(ctx: &egui::Context, theme: config::Theme) {
    let dark = theme == config::Theme::Dark;
    let p = if dark { DARK } else { LIGHT };
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = p.panel_bg;
    visuals.window_fill = p.panel_bg;
    visuals.extreme_bg_color = p.bg;
    visuals.faint_bg_color = p.card_bg;
    visuals.hyperlink_color = p.accent;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    ctx.set_visuals(visuals);
    DARK_MODE.store(dark, std::sync::atomic::Ordering::Relaxed);
}

const C_RED: Color32 = Color32::from_rgb(239, 83, 80);
const C_YELLOW: Color32 = Color32::from_rgb(255, 202, 40);
const C_GREEN: Color32 = Color32::from_rgb(102, 187, 106);
const C_PURPLE: Color32 = Color32::from_rgb(171, 130, 224);
const C_ORANGE: Color32 = Color32::from_rgb(255, 152, 0);

/// Days before a backup is considered "old".
const RECENT_BACKUP_DAYS: i64 = 7;

/// Max image-fetch jobs dispatched per frame — trickles work to the
/// background worker instead of queueing a whole library at once.
const IMAGE_REQUESTS_PER_FRAME: usize = 6;

// ---------- model ----------
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Backup,
    Restore,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    NoBackup,
    OldBackup,
    RecentBackup,
    NotInstalled,
    /// Installed (or with a backup) but the scan found zero save files —
    /// the "config only / missing save data" heuristic warning.
    ConfigOnly,
}

impl Status {
    fn color(self) -> Color32 {
        match self {
            Status::NoBackup => C_RED,
            Status::OldBackup => C_YELLOW,
            Status::RecentBackup => C_GREEN,
            Status::NotInstalled => C_PURPLE,
            Status::ConfigOnly => C_ORANGE,
        }
    }

    fn tr_key(self) -> &'static str {
        match self {
            Status::NoBackup => "status.no_backup",
            Status::OldBackup => "status.old_backup",
            Status::RecentBackup => "status.recent_backup",
            Status::NotInstalled => "status.not_installed",
            Status::ConfigOnly => "status.config_only",
        }
    }

    fn label(self) -> String {
        tr(self.tr_key())
    }
}

struct GameRow {
    name: String,
    path_hint: String,
    /// Primary Steam appid from the manifest (`steam.id`) — the CDN
    /// header image key.
    steam_id: Option<u32>,
    installed: bool,
    checked: bool,
    scan: Option<ScanInfo>,
    /// Newest backup in `backup.path` (drive for status dots).
    latest_backup: Option<chrono::DateTime<chrono::Utc>>,
    /// Newest backup in `restore.path` (visibility in Restore Mode).
    restorable_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Files in the newest backup snapshot: (original path, size).
    /// Used to display meaningful data for not-installed games.
    backup_files: Vec<(String, u64)>,
}

impl GameRow {
    /// `verified` = the game is in `config.verified_saves` — the user
    /// confirmed its save locations, so the "config only" heuristic is
    /// bypassed and the row shows its normal backup-status color.
    fn status(&self, verified: bool) -> Status {
        // The "config only" warning outranks not-installed: a game that
        // only ever held config files is worth flagging even when it
        // isn't installed here.
        if !verified && self.is_config_only() {
            Status::ConfigOnly
        } else if !self.installed {
            Status::NotInstalled
        } else {
            match self.latest_backup {
                None => Status::NoBackup,
                Some(when) => {
                    if chrono::Utc::now() - when <= chrono::Duration::days(RECENT_BACKUP_DAYS) {
                        Status::RecentBackup
                    } else {
                        Status::OldBackup
                    }
                }
            }
        }
    }

    /// The "config only / missing save data" heuristic:
    /// - installed + scanned + zero files → nothing to back up;
    /// - local scan found files but EVERY one is a config-type file
    ///   (ini/cfg/json/xml/…) → config, not save data;
    /// - not installed → judged by backup contents instead: a snapshot
    ///   holding only config files warns the same way.
    fn is_config_only(&self) -> bool {
        if let Some(scan) = &self.scan
            && !scan.found_files.is_empty()
        {
            return scan.found_files.keys().all(|p| is_config_file(p));
        }
        if self.installed {
            return self.scan.is_some();
        }
        !self.backup_files.is_empty()
            && self.backup_files.iter().all(|(p, _)| is_config_file(p))
    }
}

/// Whether `path` looks like a config file rather than save data —
/// used by the "config only" heuristic. No-extension files count as
/// save data (binary blobs), so only a known config extension flags.
fn is_config_file(path: &str) -> bool {
    const CONFIG_EXTS: &[&str] = &[
        "ini", "cfg", "conf", "config", "json", "xml", "yaml", "yml", "toml",
        "properties", "prefs", "opt", "settings",
    ];
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase());
    ext.is_some_and(|e| CONFIG_EXTS.contains(&e.as_str()))
}

/// Messages sent from worker threads back to the UI.
enum Msg {
    Status(String),
    GameScanned {
        name: String,
        path_hint: String,
        steam_id: Option<u32>,
        installed: bool,
        scan: ScanInfo,
        latest_backup: Option<chrono::DateTime<chrono::Utc>>,
        restorable_at: Option<chrono::DateTime<chrono::Utc>>,
        backup_files: Vec<(String, u64)>,
    },
    ScanFinished {
        games: usize,
        error: Option<String>,
    },
    BackupDone(BackupReport),
    BackupFinished,
    RestoreDone(backup::RestoreReport),
    RestoreFinished,
    /// Worker-thread result of an rclone cloud sync.
    CloudSyncDone(Result<(), String>),
    ManifestUpdateDone {
        checked: chrono::DateTime<chrono::Utc>,
        updated: Option<chrono::DateTime<chrono::Utc>>,
        modified: bool,
        error: Option<String>,
    },
}

struct CheckpointApp {
    mode: Mode,
    selected: Option<String>,
    games: Vec<GameRow>,
    status_line: String,
    scanning: bool,
    backing_up: bool,
    restoring: bool,
    rx: Option<Receiver<Msg>>,
    cache_dir: PathBuf,
    backup_dir: PathBuf,
    /// Ludusavi-compatible settings (shared `config.yaml`).
    config: Config,
    show_settings: bool,
    draft_backup_dir: String,
    draft_restore_dir: String,
    draft_full_retention: u8,
    draft_diff_retention: u8,
    settings_error: Option<String>,
    /// Manifest update state (shared `cache.yaml` timestamps).
    manifest_updating: bool,
    manifest_checked: Option<chrono::DateTime<chrono::Utc>>,
    manifest_updated: Option<chrono::DateTime<chrono::Utc>>,
    /// Reports from the finished backup run, shown in the summary modal.
    backup_results: Vec<BackupReport>,
    show_backup_summary: bool,
    /// Games queued for restore while the confirmation modal is open.
    pending_restore: Vec<String>,
    /// Selected backup version (`backup-<ts>` folder name) per game in
    /// the restore modal; defaults to each game's newest snapshot.
    restore_choice: HashMap<String, String>,
    /// Game awaiting local-save deletion while the modal is open.
    pending_delete: Option<String>,
    /// Game being edited in the "Set Custom Path" modal + the draft path.
    editing_custom_path: Option<String>,
    draft_custom_path: String,
    custom_path_error: Option<String>,
    /// Language tag being edited in Settings (e.g. `en-US`, `ar-SA`).
    draft_language: String,
    /// Theme being edited in Settings + the last theme applied to egui.
    draft_theme: config::Theme,
    theme_applied: config::Theme,
    /// Settings toggle for game images (`showGameIcons`).
    draft_show_icons: bool,
    /// A cloud sync (rclone) is running on a worker thread.
    cloud_syncing: bool,
    /// Settings drafts for the cloud-sync section (`cloud.*`,
    /// `apps.rclone.*`).
    draft_remote_name: String,
    draft_cloud_path: String,
    draft_rclone_path: String,
    draft_rclone_args: String,
    /// Ready card images (Steam header or exe icon) keyed by game name.
    image_cache: HashMap<String, egui::TextureHandle>,
    /// Fetches currently running on the image worker thread.
    image_pending: HashSet<String>,
    /// Games with no usable image — letter placeholder, never retried.
    image_failed: HashSet<String>,
    /// Channel to the background image-fetch worker.
    image_tx: std::sync::mpsc::Sender<ImageJob>,
    image_rx: std::sync::mpsc::Receiver<ImageResult>,
}

/// One fetch request for the background image worker.
struct ImageJob {
    name: String,
    steam_id: Option<u32>,
    hint_dir: String,
}

/// `(game name, decoded image or None)`.
type ImageResult = (String, Option<egui::ColorImage>);

/// Background image fetcher: consumes requests one at a time so the UI
/// thread never blocks on network or PE parsing. Exits when the app
/// drops its sender.
fn image_worker(
    rx: std::sync::mpsc::Receiver<ImageJob>,
    tx: std::sync::mpsc::Sender<ImageResult>,
    cache_dir: PathBuf,
) {
    while let Ok(job) = rx.recv() {
        let img =
            icons::fetch_game_image(&job.name, job.steam_id, &job.hint_dir, &cache_dir);
        if tx.send((job.name, img)).is_err() {
            break;
        }
    }
}

impl CheckpointApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        // Global theme from the shared config (`theme: light|dark`).
        apply_theme(&cc.egui_ctx, config.theme);
        for t in [egui::Theme::Dark, egui::Theme::Light] {
            cc.egui_ctx.style_mut_of(t, |style| {
                style.spacing.item_spacing = Vec2::new(8.0, 8.0);
                style.spacing.button_padding = Vec2::new(10.0, 5.0);
            });
        }
        // UI language from the shared config (en-US, ar-SA, ...).
        i18n::init(&config.language);
        // Arabic-capable fallback font for RTL glyphs.
        install_fallback_fonts(&cc.egui_ctx);
        let backup_dir = PathBuf::from(&config.backup.path);
        let (manifest_checked, manifest_updated) = cache::Cache::load().manifest_info();
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Background image-fetch worker (CDN download / exe parsing).
        let (image_tx, job_rx) = std::sync::mpsc::channel::<ImageJob>();
        let (res_tx, image_rx) = std::sync::mpsc::channel::<ImageResult>();
        let worker_cache = base.join("cache");
        std::thread::spawn(move || image_worker(job_rx, res_tx, worker_cache));
        let mut app = Self {
            mode: Mode::Backup,
            selected: None,
            games: vec![],
            status_line: trf("status.ready", &[("path", &backup_dir.display())]),
            scanning: false,
            backing_up: false,
            restoring: false,
            rx: None,
            cache_dir: base.join("cache"),
            backup_dir,
            theme_applied: config.theme,
            config,
            show_settings: false,
            draft_backup_dir: String::new(),
            draft_restore_dir: String::new(),
            draft_full_retention: 1,
            draft_diff_retention: 0,
            settings_error: None,
            manifest_updating: false,
            manifest_checked,
            manifest_updated,
            backup_results: vec![],
            show_backup_summary: false,
            pending_restore: vec![],
            restore_choice: HashMap::new(),
            pending_delete: None,
            editing_custom_path: None,
            draft_custom_path: String::new(),
            custom_path_error: None,
            draft_language: i18n::LANGUAGE_ENGLISH.to_string(),
            draft_theme: config::Theme::Dark,
            draft_show_icons: false,
            cloud_syncing: false,
            draft_remote_name: String::new(),
            draft_cloud_path: String::new(),
            draft_rclone_path: String::new(),
            draft_rclone_args: String::new(),
            image_cache: HashMap::new(),
            image_pending: HashSet::new(),
            image_failed: HashSet::new(),
            image_tx,
            image_rx,
        };
        // Auto-scan on startup: the list populates as soon as the
        // manifest and roots are available.
        app.start_scan(&cc.egui_ctx);
        app
    }

    fn busy(&self) -> bool {
        self.scanning
            || self.backing_up
            || self.restoring
            || self.manifest_updating
            || self.cloud_syncing
    }

    fn selected_row(&self) -> Option<&GameRow> {
        self.selected
            .as_ref()
            .and_then(|name| self.games.iter().find(|g| &g.name == name))
    }

    // ---------- worker threads ----------
    fn start_scan(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        self.rx = Some(rx);
        self.scanning = true;
        self.games.clear();
        self.selected = None;

        let ctx = ctx.clone();
        let cache_dir = self.cache_dir.clone();
        let backup_dir = self.backup_dir.clone();
        let restore_dir = PathBuf::from(&self.config.restore.path);
        let custom_games = self.config.custom_games.clone();

        std::thread::spawn(move || {
            let send = |msg: Msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };
            // If the scan ever panics, still report ScanFinished so the
            // UI releases the busy state instead of wedging forever.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_scan(cache_dir, backup_dir, restore_dir, custom_games, &send);
            }));
            if result.is_err() {
                send(Msg::ScanFinished {
                    games: 0,
                    error: Some("scan worker panicked".to_string()),
                });
            }
        });
    }

    fn start_backup(&mut self, ctx: &egui::Context) {
        let targets: Vec<ScanInfo> = self
            .games
            .iter()
            .filter(|g| {
                let any_checked = self.games.iter().any(|x| x.checked);
                g.checked || (!any_checked && self.selected.as_deref() == Some(g.name.as_str()))
            })
            .filter_map(|g| g.scan.clone())
            .collect();

        if targets.is_empty() {
            self.status_line = tr("backup.nothing");
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        self.rx = Some(rx);
        self.backing_up = true;
        self.backup_results.clear();

        let ctx = ctx.clone();
        let backup_dir = self.backup_dir.clone();
        let retention_full = self.config.backup.retention.full;

        std::thread::spawn(move || {
            let send = |msg: Msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };

            let layout = BackupLayout::new(backup_dir);
            if let Err(e) = std::fs::create_dir_all(&layout.base) {
                send(Msg::Status(trf("backup.no_dir", &[("err", &e)])));
                send(Msg::BackupFinished);
                return;
            }

            for scan in &targets {
                send(Msg::Status(trf("backup.progress", &[("name", &scan.game_name)])));
                let mut report = layout.back_up_game(scan);
                if report.backup_name.is_some() {
                    report.pruned = layout.enforce_retention(&scan.game_name, retention_full);
                }
                send(Msg::BackupDone(report));
            }
            send(Msg::BackupFinished);
        });
    }

    /// Compute the restore target set (checked games, or the selected
    /// one) and open the confirmation modal instead of restoring
    /// immediately.
    fn prepare_restore(&mut self) {
        let any_checked = self.games.iter().any(|g| g.checked);
        let targets: Vec<String> = self
            .games
            .iter()
            .filter(|g| g.restorable_at.is_some())
            .filter(|g| {
                g.checked || (!any_checked && self.selected.as_deref() == Some(g.name.as_str()))
            })
            .map(|g| g.name.clone())
            .collect();

        if targets.is_empty() {
            self.status_line = tr("restore.nothing");
            return;
        }
        // Everything in the pending set starts checked so the modal
        // checkboxes reflect exactly what will be restored.
        for g in self.games.iter_mut() {
            if targets.contains(&g.name) {
                g.checked = true;
            }
        }
        // Version pickers default to each game's newest snapshot
        // (first row of its sorted history). Clear first so a stale
        // choice from a previous modal session can't survive.
        let restore_layout = BackupLayout::new(PathBuf::from(&self.config.restore.path));
        self.restore_choice.clear();
        for name in &targets {
            if let Some(newest) = restore_layout.backup_history(name).first() {
                self.restore_choice
                    .insert(name.clone(), newest.name.clone());
            }
        }
        self.pending_restore = targets;
    }

    /// Restore the given games from the restore source directory —
    /// copies each file back to its original path. Each target is
    /// `(game name, chosen backup name or None for newest)`.
    fn start_restore(&mut self, ctx: &egui::Context, targets: Vec<(String, Option<String>)>) {
        if targets.is_empty() {
            self.status_line = tr("restore.nothing");
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        self.rx = Some(rx);
        self.restoring = true;

        let ctx = ctx.clone();
        let restore_dir = PathBuf::from(&self.config.restore.path);

        std::thread::spawn(move || {
            let send = |msg: Msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };

            let layout = BackupLayout::new(restore_dir);
            if !layout.base.is_dir() {
                send(Msg::Status(trf(
                    "restore.no_source",
                    &[("path", &layout.base.display())],
                )));
                send(Msg::RestoreFinished);
                return;
            }

            for (name, version) in &targets {
                send(Msg::Status(trf("restore.progress", &[("name", name)])));
                send(Msg::RestoreDone(
                    layout.restore_game_version(name, version.as_deref()),
                ));
            }
            send(Msg::RestoreFinished);
        });
    }

    /// Upload the backup directory to the configured rclone remote on a
    /// worker thread (same invocation Ludusavi uses after a backup).
    fn start_cloud_sync(&mut self, ctx: &egui::Context) {
        if self.cloud_syncing {
            return;
        }
        // Validate first so misconfiguration reports immediately.
        let rclone = match cloud::Rclone::from_config(&self.config) {
            Ok(r) => r,
            Err(_) => {
                self.status_line = tr("cloud.no_remote");
                return;
            }
        };
        if self.config.cloud.path.trim().is_empty() {
            self.status_line = tr("cloud.no_remote");
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        self.rx = Some(rx);
        self.cloud_syncing = true;

        let ctx = ctx.clone();
        let backup_dir = self.backup_dir.clone();
        let cloud_path = self.config.cloud.path.clone();

        std::thread::spawn(move || {
            let _ = tx.send(Msg::Status(tr("cloud.syncing")));
            ctx.request_repaint();
            let result = rclone.sync_upload(&backup_dir, &cloud_path);
            let _ = tx.send(Msg::CloudSyncDone(result));
            ctx.request_repaint();
        });
    }

    /// Download/revalidate the primary manifest on a worker thread,
    /// mirroring Ludusavi's manifest update (ETag + `cache.yaml`).
    fn start_manifest_update(&mut self, ctx: &egui::Context) {
        if self.manifest_updating {
            return;
        }
        self.manifest_updating = true;
        self.status_line = tr("manifest.checking");

        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        self.rx = Some(rx);

        let ctx = ctx.clone();
        let local_cache = self.cache_dir.join(manifest::MANIFEST_FILE_NAME);
        let old_etag = cache::Cache::load()
            .manifests
            .get(manifest::MANIFEST_URL)
            .and_then(|m| m.etag.clone());

        std::thread::spawn(move || {
            let send = |msg: Msg| {
                let _ = tx.send(msg);
                ctx.request_repaint();
            };

            let Some(ludusavi_cache) = Manifest::ludusavi_manifest_path() else {
                send(Msg::ManifestUpdateDone {
                    checked: chrono::Utc::now(),
                    updated: None,
                    modified: false,
                    error: Some(tr("manifest.no_dir")),
                });
                return;
            };

            match Manifest::update(&ludusavi_cache, old_etag.as_deref()) {
                Ok(update) => {
                    let now = chrono::Utc::now();
                    // Keep our own cache copy in sync so scans see fresh data.
                    if update.modified {
                        let _ = std::fs::copy(&ludusavi_cache, &local_cache);
                    }
                    let mut cache = cache::Cache::load();
                    cache.update_manifest(update.etag, now, update.modified);
                    if let Err(e) = cache.save() {
                        send(Msg::Status(trf("manifest.cache_err", &[("err", &e)])));
                    }
                    send(Msg::ManifestUpdateDone {
                        checked: now,
                        updated: update.modified.then_some(now),
                        modified: update.modified,
                        error: None,
                    });
                }
                Err(e) => send(Msg::ManifestUpdateDone {
                    checked: chrono::Utc::now(),
                    updated: None,
                    modified: false,
                    error: Some(e),
                }),
            }
        });
    }

    fn poll_messages(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = false;
        loop {
            let msg = match rx.try_recv() {
                Ok(msg) => msg,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The worker thread died without sending its *Finished
                // message (panic or silent early exit). Without this the
                // busy flags would stay set forever and permanently
                // disable the Scan/Backup/Restore buttons.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if self.busy() {
                        self.scanning = false;
                        self.backing_up = false;
                        self.restoring = false;
                        self.manifest_updating = false;
                        self.status_line = tr("scan.worker_died");
                    }
                    done = true;
                    break;
                }
            };
            match msg {
                Msg::Status(text) => self.status_line = text,
                Msg::GameScanned {
                    name,
                    path_hint,
                    steam_id,
                    installed,
                    scan,
                    latest_backup,
                    restorable_at,
                    backup_files,
                } => {
                    let checked = match self.mode {
                        Mode::Backup => installed && !scan.found_files.is_empty(),
                        Mode::Restore => restorable_at.is_some(),
                    };
                    let row = GameRow {
                        name: name.clone(),
                        path_hint,
                        steam_id,
                        installed,
                        checked,
                        scan: Some(scan),
                        latest_backup,
                        restorable_at,
                        backup_files,
                    };
                    if let Some(existing) = self.games.iter_mut().find(|g| g.name == name) {
                        *existing = row;
                    } else {
                        self.games.push(row);
                        self.games.sort_by(|a, b| a.name.cmp(&b.name));
                    }
                    if self.selected.is_none() {
                        self.selected = Some(name);
                    }
                }
                Msg::ScanFinished { games, error } => {
                    self.scanning = false;
                    done = true;
                    if std::env::var_os("GSC_DEBUG").is_some() {
                        eprintln!("[ui] scan finished: {} scanned, {} rows", games, self.games.len());
                    }
                    self.status_line = match error {
                        Some(e) => trf("scan.failed", &[("err", &e)]),
                        None => trf(
                            "scan.done",
                            &[("games", &games), ("found", &self.games.len())],
                        ),
                    };
                }
                Msg::BackupDone(report) => {
                    if let Some(row) = self.games.iter_mut().find(|g| g.name == report.game_name) {
                        if report.backup_name.is_some() {
                            row.latest_backup = Some(chrono::Utc::now());
                            // When backup dir == restore source, a fresh backup
                            // makes the game restorable too.
                            if normalize(&self.config.restore.path)
                                == normalize(&self.config.backup.path)
                            {
                                row.restorable_at = row.latest_backup;
                            }
                            // Refresh change state: everything we just copied is now "same".
                            if let Some(scan) = &mut row.scan {
                                for file in scan.found_files.values_mut() {
                                    file.change = ScanChange::Same;
                                }
                            }
                        }
                    }
                    self.status_line = match (&report.backup_name, &report.skipped_reason) {
                        (Some(name), _) => {
                            let mut text = trf(
                                "backup.done",
                                &[
                                    ("name", &report.game_name),
                                    ("files", &report.files_copied),
                                    ("size", &human_bytes(report.bytes_copied)),
                                    ("backup", name),
                                ],
                            );
                            if report.pruned > 0 {
                                text.push_str(&trf("backup.pruned", &[("count", &report.pruned)]));
                            }
                            text
                        }
                        (None, Some(reason)) => trf(
                            "backup.skipped",
                            &[("name", &report.game_name), ("reason", &translate_reason(reason))],
                        ),
                        (None, None) => trf(
                            "backup.failed",
                            &[("name", &report.game_name), ("errs", &format!("{:?}", report.errors))],
                        ),
                    };
                    self.backup_results.push(report);
                }
                Msg::BackupFinished => {
                    self.backing_up = false;
                    self.show_backup_summary = !self.backup_results.is_empty();
                    done = true;
                }
                Msg::RestoreDone(report) => {
                    self.status_line = match (&report.backup_name, &report.skipped_reason) {
                        (Some(name), _) if report.errors.is_empty() => trf(
                            "restore.done",
                            &[
                                ("name", &report.game_name),
                                ("files", &report.files_restored),
                                ("size", &human_bytes(report.bytes_restored)),
                                ("backup", name),
                            ],
                        ),
                        (Some(name), _) => trf(
                            "restore.done_errors",
                            &[
                                ("name", &report.game_name),
                                ("errors", &report.errors.len()),
                                ("files", &report.files_restored),
                                ("backup", name),
                            ],
                        ),
                        (None, Some(reason)) => trf(
                            "backup.skipped",
                            &[("name", &report.game_name), ("reason", &translate_reason(reason))],
                        ),
                        (None, None) => trf("restore.failed", &[("name", &report.game_name)]),
                    };
                }
                Msg::RestoreFinished => {
                    self.restoring = false;
                    done = true;
                }
                Msg::ManifestUpdateDone {
                    checked,
                    updated,
                    modified,
                    error,
                } => {
                    self.manifest_updating = false;
                    done = true;
                    self.manifest_checked = Some(checked);
                    if let Some(u) = updated {
                        self.manifest_updated = Some(u);
                    }
                    self.status_line = match (&error, modified) {
                        (Some(e), _) => trf("manifest.failed", &[("err", e)]),
                        (None, true) => tr("manifest.updated"),
                        (None, false) => tr("manifest.current"),
                    };
                }
                Msg::CloudSyncDone(result) => {
                    self.cloud_syncing = false;
                    done = true;
                    self.status_line = match result {
                        Ok(()) => tr("cloud.sync_done"),
                        Err(e) => trf("cloud.sync_failed", &[("err", &e)]),
                    };
                }
            }
        }
        if done {
            self.rx = None;
        }
    }

    /// Drain the image worker's results: upload decoded images as
    /// textures, mark failures so they aren't re-requested, and ask for
    /// a repaint so cards update as soon as an image lands.
    fn poll_images(&mut self, ctx: &egui::Context) {
        let mut any = false;
        while let Ok((name, img)) = self.image_rx.try_recv() {
            self.image_pending.remove(&name);
            match img {
                Some(img) => {
                    self.image_cache.insert(
                        name.clone(),
                        ctx.load_texture(
                            format!("img:{name}"),
                            img,
                            egui::TextureOptions::LINEAR,
                        ),
                    );
                }
                None => {
                    self.image_failed.insert(name);
                }
            }
            any = true;
        }
        if any {
            ctx.request_repaint();
        }
    }

    // ---------- settings ----------
    fn open_settings(&mut self) {
        self.draft_backup_dir = self.config.backup.path.clone();
        self.draft_restore_dir = self.config.restore.path.clone();
        self.draft_full_retention = self.config.backup.retention.full.max(1);
        self.draft_diff_retention = self.config.backup.retention.differential;
        self.draft_language = if self.config.language.is_empty() {
            i18n::LANGUAGE_ENGLISH.to_string()
        } else {
            self.config.language.clone()
        };
        self.draft_theme = self.config.theme;
        self.draft_show_icons = self.config.show_game_icons;
        self.draft_remote_name = self.config.cloud_remote_id().unwrap_or_default();
        self.draft_cloud_path = self.config.cloud.path.clone();
        self.draft_rclone_path = self.config.apps.rclone.path.clone();
        self.draft_rclone_args = self.config.apps.rclone.arguments.clone();
        self.settings_error = None;
        // Re-read shared cache.yaml so timestamps reflect updates made
        // by Ludusavi itself since the app started.
        let (checked, updated) = cache::Cache::load().manifest_info();
        self.manifest_checked = checked;
        self.manifest_updated = updated;
        self.show_settings = true;
    }

    fn save_settings(&mut self) {
        self.config.backup.path = normalize(&self.draft_backup_dir);
        self.config.restore.path = normalize(&self.draft_restore_dir);
        self.config.backup.retention.full = self.draft_full_retention.max(1);
        self.config.backup.retention.differential = self.draft_diff_retention;
        let language_changed = self.config.language != self.draft_language;
        self.config.language = self.draft_language.clone();
        self.config.theme = self.draft_theme;
        self.config.show_game_icons = self.draft_show_icons;
        // Cloud block: renaming to the same id preserves whatever
        // Remote variant Ludusavi configured (credentials included);
        // a different name writes Remote::Custom.
        self.config.set_cloud_remote(&self.draft_remote_name);
        self.config.cloud.path = self.draft_cloud_path.trim().to_string();
        self.config.apps.rclone.path = self.draft_rclone_path.trim().to_string();
        self.config.apps.rclone.arguments = self.draft_rclone_args.clone();
        // Re-run migration on what was typed: an emptied restore path
        // collapses back to the backup dir (same as a fresh load),
        // instead of staying "" and silently scanning nothing.
        self.config.fix();
        match self.config.save() {
            Ok(()) => {
                self.backup_dir = PathBuf::from(&self.config.backup.path);
                if language_changed {
                    i18n::set_language(&self.config.language);
                }
                self.status_line = trf(
                    "settings.saved",
                    &[("path", &Config::file_path().display())],
                );
                self.show_settings = false;
            }
            Err(e) => self.settings_error = Some(e),
        }
    }

    /// Settings modal: backup directory + retention, backed by the
    /// shared Ludusavi `config.yaml`.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut save = false;
        let mut cancel = false;
        let mut update_manifest = false;
        egui::Modal::new(egui::Id::unique("settings_modal")).show(ctx, |ui| {
            ui.set_min_width(470.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);

            ui.label(RichText::new(tr("settings.title")).size(17.0).strong());
            rtl_label(
                ui,
                RichText::new(tr("settings.shared_hint"))
                    .size(11.0)
                    .color(colors().text_weak),
            );
            // Height cap: screen minus a margin (with a sane absolute
            // cap) so the scroll region engages instead of letting the
            // modal overflow past the window's bottom edge.
            let max_h = (ctx.content_rect().height() - 60.0).min(640.0);
            ui.set_max_height(max_h);

            // Fixed bottom bar — the path label + Save/Cancel stay at
            // the modal's bottom edge, outside the scroll region.
            egui::Panel::bottom("settings_bottom").show(ui, |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(Config::file_path().display().to_string())
                            .size(10.0)
                            .color(colors().text_weak),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.draft_backup_dir.trim().is_empty(),
                                egui::Button::new(RichText::new(tr("common.save")).strong())
                                    .fill(colors().accent)
                                    .min_size(Vec2::new(80.0, 26.0)),
                            )
                            .clicked()
                        {
                            save = true;
                        }
                        if ui.button(tr("common.cancel")).clicked() {
                            cancel = true;
                        }
                    });
                });
            });

            // Scrollable body — all settings fields in normal top-down
            // order; the CentralPanel takes the space left by the
            // bottom bar.
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if self.settings_fields(ui) {
                            update_manifest = true;
                        }
                    });
            });
        });

        if save {
            self.save_settings();
        } else if cancel {
            self.show_settings = false;
        }
        if update_manifest {
            self.start_manifest_update(ctx);
        }
    }

    /// The scrollable body of the Settings modal — every field between
    /// the fixed title and the pinned Save/Cancel strip. Returns `true`
    /// when the "update manifest" button was clicked.
    fn settings_fields(&mut self, ui: &mut egui::Ui) -> bool {
        let mut update_manifest = false;

            // Language picker — bound to the shared `language` field.
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("settings.language")).strong());
                egui::ComboBox::from_id_salt("language_picker")
                    .selected_text(match self.draft_language.as_str() {
                        i18n::LANGUAGE_ARABIC => i18n::display("العربية"),
                        _ => "English".to_string(),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.draft_language,
                            i18n::LANGUAGE_ENGLISH.to_string(),
                            "English",
                        );
                        ui.selectable_value(
                            &mut self.draft_language,
                            i18n::LANGUAGE_ARABIC.to_string(),
                            i18n::display("العربية"),
                        );
                    });
            });

            // Theme picker — bound to the shared `theme` field
            // (light|dark), same key Ludusavi uses.
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("settings.theme")).strong());
                ui.radio_value(
                    &mut self.draft_theme,
                    config::Theme::Dark,
                    tr("theme.dark"),
                );
                ui.radio_value(
                    &mut self.draft_theme,
                    config::Theme::Light,
                    tr("theme.light"),
                );
            });

            // Icon extraction toggle — bound to `showGameIcons`.
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.draft_show_icons, RichText::new(tr("settings.show_icons")).strong());
            });
            rtl_label(
                ui,
                RichText::new(tr("settings.show_icons_hint"))
                    .size(10.5)
                    .color(colors().text_weak),
            );

            ui.add_space(6.0);
            ui.label(RichText::new(tr("settings.backup_dir")).strong());
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_backup_dir)
                        .desired_width(360.0),
                );
                if ui.button(tr("common.browse")).clicked()
                    && let Some(path) = browse_folder(&self.draft_backup_dir)
                {
                    self.draft_backup_dir = path;
                }
            });
            rtl_label(
                ui,
                RichText::new(trf(
                    "settings.backup_dir_hint",
                    &[("path", &self.config.backup.path)],
                ))
                .size(10.5)
                .color(colors().text_weak),
            );

            ui.add_space(4.0);
            ui.label(RichText::new(tr("settings.restore_dir")).strong());
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_restore_dir)
                        .desired_width(360.0),
                );
                if ui.button(tr("common.browse")).clicked()
                    && let Some(path) = browse_folder(&self.draft_restore_dir)
                {
                    self.draft_restore_dir = path;
                }
            });
            rtl_label(
                ui,
                RichText::new(tr("settings.restore_dir_hint"))
                    .size(10.5)
                    .color(colors().text_weak),
            );

            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new(tr("settings.retention")).strong());
            ui.horizontal(|ui| {
                ui.label(tr("settings.retention_full"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.draft_full_retention)
                            .range(1..=255)
                            .speed(0.05),
                    );
                });
            });
            ui.horizontal(|ui| {
                ui.label(tr("settings.retention_diff"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.draft_diff_retention)
                            .range(0..=255)
                            .speed(0.05),
                    );
                });
            });
            rtl_label(
                ui,
                RichText::new(tr("settings.retention_hint"))
                    .size(10.5)
                    .color(colors().text_weak),
            );

            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new(tr("settings.manifest")).strong());
            ui.horizontal(|ui| {
                let updating = self.manifest_updating;
                if ui
                    .add_enabled(
                        !self.busy(),
                        egui::Button::new(RichText::new(tr("settings.update_manifest")).strong())
                            .fill(colors().accent)
                            .min_size(Vec2::new(130.0, 26.0)),
                    )
                    .clicked()
                {
                    update_manifest = true;
                }
                if updating {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new(tr("settings.checking")).size(11.0).color(colors().text_weak));
                }
            });
            let fmt_ts = |ts: Option<chrono::DateTime<chrono::Utc>>| {
                ts.map(|t| {
                    let local: chrono::DateTime<chrono::Local> = t.into();
                    local.format("%Y-%m-%d %H:%M").to_string()
                })
                .unwrap_or_else(|| tr("common.never"))
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("settings.last_checked")).size(11.5).color(colors().text_weak));
                ui.label(RichText::new(fmt_ts(self.manifest_checked)).size(11.5));
                ui.add_space(16.0);
                ui.label(RichText::new(tr("settings.last_updated")).size(11.5).color(colors().text_weak));
                ui.label(RichText::new(fmt_ts(self.manifest_updated)).size(11.5));
            });
            rtl_label(
                ui,
                RichText::new(tr("settings.manifest_hint"))
                    .size(10.5)
                    .color(colors().text_weak),
            );

            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(RichText::new(tr("cloud.section")).strong());
            rtl_label(
                ui,
                RichText::new(tr("cloud.hint"))
                    .size(10.5)
                    .color(colors().text_weak),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("cloud.remote_name")).size(11.5));
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_remote_name)
                        .desired_width(160.0),
                );
                ui.label(RichText::new(tr("cloud.path")).size(11.5));
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_cloud_path)
                        .desired_width(130.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("cloud.rclone_path")).size(11.5));
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_rclone_path)
                        .desired_width(160.0),
                );
                ui.label(RichText::new(tr("cloud.args")).size(11.5));
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_rclone_args)
                        .desired_width(130.0),
                );
            });

            if self.draft_backup_dir.trim().is_empty() {
                ui.add_space(4.0);
                rtl_label(ui, RichText::new(tr("settings.empty_dir")).size(11.5).color(C_RED));
            }
            if let Some(err) = &self.settings_error {
                ui.add_space(4.0);
                rtl_label(
                    ui,
                    RichText::new(trf("settings.save_err", &[("err", err)]))
                        .size(11.5)
                        .color(C_RED),
                );
            }

            update_manifest
    }

    // ---------- backup summary ----------
    /// Shown when a backup run finishes: how many games were backed up
    /// plus the names/errors of any that hit problems.
    fn backup_summary_window(&mut self, ctx: &egui::Context) {
        if !self.show_backup_summary {
            return;
        }
        let mut close = false;
        egui::Modal::new(egui::Id::unique("backup_summary_modal")).show(ctx, |ui| {
            ui.set_min_width(460.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);

            ui.label(RichText::new(tr("summary.title")).size(17.0).strong());
            ui.add_space(4.0);

            let succeeded = self
                .backup_results
                .iter()
                .filter(|r| r.backup_name.is_some())
                .count();
            let skipped = self
                .backup_results
                .iter()
                .filter(|r| r.skipped_reason.is_some())
                .count();
            let with_errors: Vec<&BackupReport> = self
                .backup_results
                .iter()
                .filter(|r| !r.errors.is_empty())
                .collect();

            let files: usize = self.backup_results.iter().map(|r| r.files_copied).sum();
            let bytes: u64 = self.backup_results.iter().map(|r| r.bytes_copied).sum();
            rtl_label(
                ui,
                RichText::new(trf(
                    "summary.count",
                    &[
                        ("succeeded", &succeeded),
                        ("files", &files),
                        ("bytes", &human_bytes(bytes)),
                    ],
                )),
            );
            if skipped > 0 {
                rtl_label(
                    ui,
                    RichText::new(trf("summary.skipped", &[("skipped", &skipped)]))
                        .size(11.5)
                        .color(colors().text_weak),
                );
            }

            if !with_errors.is_empty() {
                ui.add_space(4.0);
                ui.separator();
                rtl_label(
                    ui,
                    RichText::new(trf("summary.errors", &[("count", &with_errors.len())]))
                        .color(C_RED)
                        .strong(),
                );
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for report in &with_errors {
                            ui.label(RichText::new(i18n::display(&report.game_name)).strong().size(12.5));
                            for err in &report.errors {
                                ui.label(
                                    RichText::new(format!("  {err}"))
                                        .size(11.0)
                                        .monospace()
                                        .color(colors().text_weak),
                                );
                            }
                        }
                    });
            } else {
                rtl_label(
                    ui,
                    RichText::new(tr("summary.no_errors")).size(11.5).color(colors().text_weak),
                );
            }

            ui.add_space(8.0);
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new(tr("common.ok")).strong())
                            .fill(colors().accent)
                            .min_size(Vec2::new(80.0, 26.0)),
                    )
                    .clicked()
                {
                    close = true;
                }
            });
        });
        if close {
            self.show_backup_summary = false;
            self.backup_results.clear();
        }
    }

    // ---------- restore confirmation ----------
    /// Open while `pending_restore` is non-empty. Shows each game with
    /// its backup timestamp vs. local save timestamp and a per-game
    /// checkbox; Confirm runs the actual restore.
    fn restore_confirm_window(&mut self, ctx: &egui::Context) {
        if self.pending_restore.is_empty() {
            return;
        }
        let mut confirm = false;
        let mut cancel = false;
        // Snapshot histories from the restore source once per frame —
        // each row needs the version count and the radio list.
        let restore_layout = BackupLayout::new(PathBuf::from(&self.config.restore.path));
        let histories: HashMap<String, Vec<backup::HistoryRow>> = self
            .pending_restore
            .iter()
            .map(|n| (n.clone(), restore_layout.backup_history(n)))
            .collect();
        egui::Modal::new(egui::Id::unique("restore_confirm_modal")).show(ctx, |ui| {
            ui.set_min_width(560.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);

            ui.label(RichText::new(tr("restore.title")).size(17.0).strong());
            rtl_label(
                ui,
                RichText::new(tr("restore.hint"))
                    .size(11.0)
                    .color(colors().text_weak),
            );
            ui.add_space(4.0);
            ui.separator();

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for name in &self.pending_restore {
                        let Some(g) = self.games.iter_mut().find(|g| &g.name == name) else {
                            continue;
                        };
                        let versions: &[backup::HistoryRow] =
                            histories.get(name).map(Vec::as_slice).unwrap_or(&[]);
                        // Default selection = newest snapshot (first row);
                        // self-heal if a stored choice no longer exists
                        // in this game's history.
                        if let Some(first) = versions.first() {
                            let entry = self
                                .restore_choice
                                .entry(name.clone())
                                .or_insert_with(|| first.name.clone());
                            if !versions.iter().any(|v| &v.name == entry) {
                                *entry = first.name.clone();
                            }
                        }
                        let choice = self.restore_choice.get(name).cloned();
                        // The ts line reflects the CHOSEN version.
                        let backup_ts = versions
                            .iter()
                            .find(|v| Some(&v.name) == choice.as_ref())
                            .map(|v| fmt_dt(v.when))
                            .or_else(|| g.restorable_at.or(g.latest_backup).map(fmt_dt))
                            .unwrap_or_else(|| tr("common.unknown"));
                        let local_mtime = g
                            .scan
                            .as_ref()
                            .and_then(|s| s.found_files.values().filter_map(|f| f.mtime).max());
                        // Red = restoring will overwrite existing local
                        // save data; green = nothing to lose.
                        let (local_text, local_color) = match local_mtime {
                            Some(t) => (
                                trf(
                                    "restore.local_ts",
                                    &[("ts", &fmt_dt(chrono::DateTime::<chrono::Utc>::from(t)))],
                                ),
                                C_RED,
                            ),
                            None => (tr("restore.no_local"), C_GREEN),
                        };
                        let name_text = if local_mtime.is_some() {
                            RichText::new(i18n::display(&g.name)).strong().color(C_RED)
                        } else {
                            RichText::new(i18n::display(&g.name)).strong()
                        };

                        let header = |ui: &mut egui::Ui| {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut g.checked, name_text);
                                if !versions.is_empty() {
                                    ui.label(
                                        RichText::new(format!(
                                            "({})",
                                            trf(
                                                "restore.versions",
                                                &[("count", &versions.len())]
                                            )
                                        ))
                                        .size(10.5)
                                        .color(colors().text_weak),
                                    );
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(trf("restore.backup_ts", &[("ts", &backup_ts)]))
                                        .size(11.0)
                                        .color(colors().text_weak),
                                );
                                ui.add_space(16.0);
                                ui.label(
                                    RichText::new(local_text)
                                        .size(11.0)
                                        .color(local_color),
                                );
                            });
                        };

                        Frame::new()
                            .fill(colors().card_bg)
                            .stroke(Stroke::new(1.0, colors().border))
                            .corner_radius(6)
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                if !versions.is_empty() {
                                    // The row is a drawer of
                                    // radio-pickable snapshots.
                                    egui::collapsing_header::CollapsingState::load_with_default_open(
                                        ui.ctx(),
                                        ui.scope_id().with(("restore_versions", name)),
                                        false,
                                    )
                                    .show_header(ui, header)
                                    .body(|ui| {
                                        for h in versions {
                                            let kind = if h.differential {
                                                tr("history.diff")
                                            } else {
                                                tr("history.full")
                                            };
                                            let local: chrono::DateTime<chrono::Local> =
                                                h.when.into();
                                            let label = trf(
                                                "history.row",
                                                &[
                                                    ("type", &kind),
                                                    ("date", &local.format("%Y-%m-%d %H:%M")),
                                                    ("files", &h.files),
                                                    ("size", &human_bytes(h.size)),
                                                ],
                                            );
                                            if let Some(choice) =
                                                self.restore_choice.get_mut(name)
                                            {
                                                ui.radio_value(
                                                    choice,
                                                    h.name.clone(),
                                                    RichText::new(label).size(11.5),
                                                );
                                            }
                                        }
                                    });
                                } else {
                                    header(ui);
                                }
                            });
                        ui.add_space(4.0);
                    }
                });

            let selected_count = self
                .pending_restore
                .iter()
                .filter(|n| self.games.iter().any(|g| &g.name == *n && g.checked))
                .count();
            if selected_count == 0 {
                rtl_label(
                    ui,
                    RichText::new(tr("restore.need_one"))
                        .size(11.5)
                        .color(C_YELLOW),
                );
            }

            ui.add_space(4.0);
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        selected_count > 0,
                        egui::Button::new(
                            RichText::new(trf("restore.confirm", &[("count", &selected_count)]))
                                .strong(),
                        )
                        .fill(colors().accent)
                        .min_size(Vec2::new(160.0, 26.0)),
                    )
                    .clicked()
                {
                    confirm = true;
                }
                if ui.button(tr("common.cancel")).clicked() {
                    cancel = true;
                }
            });
        });

        if confirm {
            let names: Vec<(String, Option<String>)> = self
                .pending_restore
                .iter()
                .filter(|n| {
                    self.games
                        .iter()
                        .any(|g| &g.name == *n && g.checked && g.restorable_at.is_some())
                })
                .map(|n| {
                    (
                        n.clone(),
                        self.restore_choice.get(n).filter(|s| !s.is_empty()).cloned(),
                    )
                })
                .collect();
            self.pending_restore.clear();
            self.start_restore(ctx, names);
        } else if cancel {
            self.pending_restore.clear();
        }
    }

    // ---------- local-save deletion ----------
    /// "Confirm Deletion" modal — permanently removes the selected
    /// game's local save files, then rescan updates its status.
    fn delete_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(name) = self.pending_delete.clone() else {
            return;
        };
        let (file_count, total_size) = self
            .games
            .iter()
            .find(|g| g.name == name)
            .and_then(|g| g.scan.as_ref())
            .map(|s| (s.found_files.len(), s.total_size()))
            .unwrap_or((0, 0));

        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("delete_confirm_modal")).show(ctx, |ui| {
            ui.set_min_width(460.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);

            ui.label(RichText::new(tr("delete.title")).size(17.0).strong());
            ui.add_space(4.0);
            rtl_label(ui, RichText::new(trf("delete.warn", &[("name", &name)])));
            rtl_label(
                ui,
                RichText::new(trf(
                    "delete.count",
                    &[("count", &file_count), ("size", &human_bytes(total_size))],
                ))
                .color(C_RED)
                .strong(),
            );
            rtl_label(
                ui,
                RichText::new(tr("delete.backups_safe"))
                    .size(11.0)
                    .color(colors().text_weak),
            );

            ui.add_space(8.0);
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new(tr("common.delete")).strong())
                            .fill(C_RED)
                            .min_size(Vec2::new(90.0, 26.0)),
                    )
                    .clicked()
                {
                    confirm = true;
                }
                if ui.button(tr("common.cancel")).clicked() {
                    cancel = true;
                }
            });
        });

        if confirm {
            self.pending_delete = None;
            self.delete_local_saves(ctx, &name);
        } else if cancel {
            self.pending_delete = None;
        }
    }

    /// Physically delete the game's local save files, then rescan so
    /// statuses refresh (e.g. becomes Not Installed / purple).
    /// Empty leftover directories are removed too — `remove_dir` only
    /// removes empty dirs, so unrelated content (Documents, AppData)
    /// can never be touched.
    fn delete_local_saves(&mut self, ctx: &egui::Context, name: &str) {
        let files: Vec<String> = self
            .games
            .iter()
            .find(|g| g.name == name)
            .and_then(|g| g.scan.as_ref())
            .map(|s| s.found_files.keys().cloned().collect())
            .unwrap_or_default();
        if files.is_empty() {
            self.status_line = trf("delete.none", &[("name", &name)]);
            return;
        }

        let mut deleted = 0usize;
        let mut errors: Vec<String> = vec![];
        let mut parents = std::collections::BTreeSet::new();
        for path in &files {
            let p = PathBuf::from(path);
            match std::fs::remove_file(&p) {
                Ok(()) => deleted += 1,
                // Already gone (e.g. a sibling glob removed it first).
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => errors.push(format!("{path}: {e}")),
            }
            if let Some(parent) = p.parent() {
                parents.insert(parent.to_path_buf());
            }
        }

        // Deepest-first; `remove_dir` fails on non-empty dirs, which
        // bounds cleanup to directories the deletion actually emptied.
        let mut parents: Vec<PathBuf> = parents.into_iter().collect();
        parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for parent in parents {
            let mut dir = Some(parent.as_path());
            while let Some(d) = dir {
                if std::fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }

        self.status_line = if errors.is_empty() {
            trf("delete.done", &[("deleted", &deleted), ("name", &name)])
        } else {
            trf(
                "delete.done_errors",
                &[
                    ("deleted", &deleted),
                    ("total", &files.len()),
                    ("name", &name),
                    ("errors", &errors.len()),
                    ("errs", &errors.join("; ")),
                ],
            )
        };
        self.start_scan(ctx);
    }

    // ---------- custom save path ----------
    /// "Set Custom Path" modal — writes a `customGames` entry into the
    /// shared `config.yaml` (same as Ludusavi's UI), then rescans.
    fn custom_path_window(&mut self, ctx: &egui::Context) {
        let Some(name) = self.editing_custom_path.clone() else {
            return;
        };
        let has_existing = self.config.custom_game_path(&name).is_some();

        let mut save = false;
        let mut clear = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("custom_path_modal")).show(ctx, |ui| {
            ui.set_min_width(500.0);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);

            ui.label(RichText::new(tr("custom.title")).size(17.0).strong());
            ui.add_space(4.0);
            ui.label(RichText::new(trf("custom.dir_label", &[("name", &name)])).strong());
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft_custom_path)
                        .desired_width(360.0)
                        .hint_text("D:/Games/My Saves"),
                );
                if ui.button(tr("common.browse")).clicked()
                    && let Some(path) = browse_folder(&self.draft_custom_path)
                {
                    self.draft_custom_path = path;
                }
            });
            rtl_label(
                ui,
                RichText::new(tr("custom.hint"))
                    .size(10.5)
                    .color(colors().text_weak),
            );
            if let Some(err) = &self.custom_path_error {
                rtl_label(
                    ui,
                    RichText::new(trf("custom.save_err", &[("err", err)]))
                        .size(11.5)
                        .color(C_RED),
                );
            }

            ui.add_space(8.0);
            ui.separator();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.draft_custom_path.trim().is_empty(),
                        egui::Button::new(RichText::new(tr("common.save")).strong())
                            .fill(colors().accent)
                            .min_size(Vec2::new(80.0, 26.0)),
                    )
                    .clicked()
                {
                    save = true;
                }
                if has_existing && ui.button(tr("custom.remove")).clicked() {
                    clear = true;
                }
                if ui.button(tr("common.cancel")).clicked() {
                    cancel = true;
                }
            });
        });

        if save {
            let path = normalize(&self.draft_custom_path);
            self.config.set_custom_game_path(&name, &path);
            match self.config.save() {
                Ok(()) => {
                    self.editing_custom_path = None;
                    self.status_line = trf("custom.set", &[("name", &name), ("path", &path)]);
                    self.start_scan(ctx);
                }
                Err(e) => self.custom_path_error = Some(e),
            }
        } else if clear {
            self.config.remove_custom_game(&name);
            match self.config.save() {
                Ok(()) => {
                    self.editing_custom_path = None;
                    self.status_line = trf("custom.removed", &[("name", &name)]);
                    self.start_scan(ctx);
                }
                Err(e) => self.custom_path_error = Some(e),
            }
        } else if cancel {
            self.editing_custom_path = None;
        }
    }

    // ---------- top header ----------
    fn header(&self, ui: &mut egui::Ui) {
        egui::Panel::top("header")
            .resizable(false)
            .exact_size(56.0)
            .frame(
                Frame::new()
                    .fill(colors().panel_bg)
                    .inner_margin(Margin::symmetric(16, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("Game Save Checkpoint").size(19.0).strong());
                    ui.label(
                        RichText::new(tr("app.tagline"))
                            .size(11.0)
                            .color(colors().text_weak),
                    );
                    // TEMPORARY: Arabic pipeline visual check — must
                    // render as connected RTL text (تجربة), not
                    // isolated letters. Remove once verified.
                    #[cfg(debug_assertions)]
                    ui.label(
                        RichText::new(i18n::display("تجربة"))
                            .size(13.0)
                            .color(C_GREEN),
                    );
                    // Compact status counters (moved up from the old
                    // bottom stat-card row).
                    let count = |s: Status| {
                        self.games
                            .iter()
                            .filter(|g| {
                                g.status(self.config.verified_saves.contains(&g.name)) == s
                            })
                            .count()
                    };
                    let stats: [(usize, String, Color32); 6] = [
                        (self.games.len(), tr("stats.total"), colors().accent),
                        (count(Status::NoBackup), Status::NoBackup.label(), C_RED),
                        (count(Status::OldBackup), Status::OldBackup.label(), C_YELLOW),
                        (count(Status::RecentBackup), Status::RecentBackup.label(), C_GREEN),
                        (count(Status::NotInstalled), Status::NotInstalled.label(), C_PURPLE),
                        (count(Status::ConfigOnly), Status::ConfigOnly.label(), C_ORANGE),
                    ];
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        for (value, label, color) in stats.iter().rev() {
                            ui.label(RichText::new(label).size(10.0).color(colors().text_weak));
                            ui.label(
                                RichText::new(value.to_string())
                                    .size(13.0)
                                    .strong()
                                    .color(*color),
                            );
                            ui.add_space(10.0);
                        }
                    });
                });
            });
    }

    // ---------- mode switcher + actions ----------
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut do_scan = false;
        let mut do_backup = false;
        let mut do_restore = false;
        let mut do_settings = false;
        let mut do_sync = false;

        egui::Panel::top("toolbar")
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(colors().panel_bg)
                    .inner_margin(Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    Frame::new()
                        .fill(colors().bg)
                        .stroke(Stroke::new(1.0, colors().border))
                        .corner_radius(6)
                        .inner_margin(Margin::same(3))
                        .show(ui, |ui| {
                            let backup = self.mode == Mode::Backup;
                            if ui
                                .add(
                                    egui::Button::new(tr("toolbar.backup_mode"))
                                        .selected(backup)
                                        .min_size(Vec2::new(110.0, 26.0)),
                                )
                                .clicked()
                            {
                                self.mode = Mode::Backup;
                                self.selected = None;
                                for g in self.games.iter_mut() {
                                    g.checked = g.installed
                                        && g.scan.as_ref().is_some_and(|s| !s.found_files.is_empty());
                                }
                            }
                            if ui
                                .add(
                                    egui::Button::new(tr("toolbar.restore_mode"))
                                        .selected(!backup)
                                        .min_size(Vec2::new(110.0, 26.0)),
                                )
                                .clicked()
                            {
                                self.mode = Mode::Restore;
                                self.selected = None;
                                for g in self.games.iter_mut() {
                                    g.checked = g.restorable_at.is_some();
                                }
                            }
                        });

                    if self.busy() {
                        ui.add(egui::Spinner::new());
                        ui.label(
                            RichText::new(if self.scanning {
                                tr("busy.scanning")
                            } else if self.restoring {
                                tr("busy.restoring")
                            } else if self.manifest_updating {
                                tr("busy.manifest")
                            } else if self.cloud_syncing {
                                tr("busy.syncing")
                            } else {
                                tr("busy.backing_up")
                            })
                            .color(colors().text_weak),
                        );
                    }
                    // Status line (was the bottom bar) — truncated to fit.
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.status_line).size(11.0).color(colors().text_weak),
                        )
                        .truncate(),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let backup_mode = self.mode == Mode::Backup;
                        let checked = self
                            .games
                            .iter()
                            .filter(|g| g.checked && (backup_mode || g.restorable_at.is_some()))
                            .count();
                        let (action_label, can_act) = if backup_mode {
                            let label = if checked > 0 {
                                trf("toolbar.start_backup_count", &[("checked", &checked)])
                            } else {
                                tr("toolbar.start_backup")
                            };
                            let can = checked > 0
                                || self.selected_row().is_some_and(|g| g.scan.is_some());
                            (label, can)
                        } else {
                            let label = if checked > 0 {
                                trf("toolbar.start_restore_count", &[("checked", &checked)])
                            } else {
                                tr("toolbar.start_restore")
                            };
                            let can = checked > 0
                                || self.selected_row().is_some_and(|g| g.restorable_at.is_some());
                            (label, can)
                        };
                        if ui
                            .add_enabled(
                                can_act && !self.busy(),
                                egui::Button::new(RichText::new(action_label).strong())
                                    .fill(colors().accent)
                                    .min_size(Vec2::new(130.0, 30.0)),
                            )
                            .clicked()
                        {
                            if backup_mode {
                                do_backup = true;
                            } else {
                                do_restore = true;
                            }
                        }
                        if ui
                            .add_enabled(
                                !self.busy(),
                                egui::Button::new(tr("toolbar.scan")).min_size(Vec2::new(80.0, 30.0)),
                            )
                            .clicked()
                        {
                            do_scan = true;
                        }
                        // Settings gear + backup path live here now
                        // (freed the header's right side for stats).
                        if ui
                            .add(
                                egui::Button::new(RichText::new("⚙").size(15.0))
                                    .min_size(Vec2::splat(30.0)),
                            )
                            .clicked()
                        {
                            do_settings = true;
                        }
                        // Manual rclone upload of the backup dir — runs on
                        // a worker thread; disabled while anything's busy
                        // or no remote is configured.
                        if ui
                            .add_enabled(
                                !self.busy() && self.config.cloud_remote_id().is_some(),
                                egui::Button::new(tr("cloud.sync_button"))
                                    .min_size(Vec2::new(105.0, 30.0)),
                            )
                            .clicked()
                        {
                            do_sync = true;
                        }
                        ui.label(
                            RichText::new(trf(
                                "toolbar.backups",
                                &[("path", &self.backup_dir.display())],
                            ))
                            .size(10.5)
                            .color(colors().text_weak),
                        );
                        ui.separator();
                    });
                });
            });

        let ctx = ui.ctx().clone();
        if do_scan {
            self.start_scan(&ctx);
        }
        if do_backup {
            self.start_backup(&ctx);
        }
        if do_restore {
            self.prepare_restore();
        }
        if do_sync {
            self.start_cloud_sync(&ctx);
        }
        if do_settings {
            self.open_settings();
        }
    }

    // ---------- left: game list ----------
    fn game_list(&mut self, ui: &mut egui::Ui) {
        let third = ui.max_rect().width() / 3.0;
        let mut clicked: Option<String> = None;
        // Restore Mode shows only games with a backup in the restore
        // source directory (mirrors Ludusavi's restore list).
        let restore_mode = self.mode == Mode::Restore;

        // Checked games first, then alphabetical — kept up-to-date every
        // frame so toggles and bulk actions reorder immediately.
        self.games
            .sort_by(|a, b| b.checked.cmp(&a.checked).then_with(|| a.name.cmp(&b.name)));

        let visible_count = self
            .games
            .iter()
            .filter(|g| !restore_mode || g.restorable_at.is_some())
            .count();

        egui::Panel::left("game_list")
            .resizable(true)
            .default_size(third)
            .min_size(240.0)
            .frame(
                Frame::new()
                    .fill(colors().panel_bg)
                    .inner_margin(Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| {
                ui.label(
                    RichText::new(trf("list.title", &[("count", &visible_count)]))
                        .size(15.0)
                        .strong(),
                );
                // Bulk selection by backup status; applies to the games
                // visible in the current mode.
                ui.horizontal_wrapped(|ui| {
                    for (label, color, status) in [
                        (tr("select.all"), colors().accent, None),
                        (tr("select.red"), C_RED, Some(Status::NoBackup)),
                        (tr("select.yellow"), C_YELLOW, Some(Status::OldBackup)),
                        (tr("select.green"), C_GREEN, Some(Status::RecentBackup)),
                        (tr("select.purple"), C_PURPLE, Some(Status::NotInstalled)),
                        (tr("select.orange"), C_ORANGE, Some(Status::ConfigOnly)),
                    ] {
                        if ui
                            .add_enabled(
                                visible_count > 0,
                                egui::Button::new(RichText::new(label).size(10.5).color(color)),
                            )
                            .clicked()
                        {
                            let verified = &self.config.verified_saves;
                            bulk_check(&mut self.games, restore_mode, |g| {
                                status.is_none_or(|s| g.status(verified.contains(&g.name)) == s)
                            });
                        }
                    }
                });
                ui.separator();
                if visible_count == 0 {
                    ui.add_space(8.0);
                    rtl_label(
                        ui,
                        RichText::new(if self.scanning {
                            tr("list.scanning")
                        } else if restore_mode {
                            tr("list.no_backups")
                        } else {
                            tr("list.empty")
                        })
                        .size(12.0)
                        .color(colors().text_weak),
                    );
                    return;
                }
                // Game images: serve the ready cache and dispatch a
                // bounded number of fetch requests per frame to the
                // background worker (Steam CDN -> disk cache -> exe
                // icon). Nothing here touches the network or disk on
                // the UI thread.
                let mut tex_ids: HashMap<String, egui::TextureId> = HashMap::new();
                if self.config.show_game_icons {
                    let mut budget = IMAGE_REQUESTS_PER_FRAME;
                    let cache = &self.image_cache;
                    let failed = &self.image_failed;
                    let pending = &mut self.image_pending;
                    let tx = &self.image_tx;
                    for g in self.games.iter() {
                        if restore_mode && g.restorable_at.is_none() {
                            continue;
                        }
                        if let Some(tex) = cache.get(&g.name) {
                            tex_ids.insert(g.name.clone(), tex.id());
                            continue;
                        }
                        if failed.contains(&g.name)
                            || pending.contains(&g.name)
                            || budget == 0
                        {
                            continue;
                        }
                        budget -= 1;
                        pending.insert(g.name.clone());
                        let _ = tx.send(ImageJob {
                            name: g.name.clone(),
                            steam_id: g.steam_id,
                            hint_dir: g.path_hint.clone(),
                        });
                    }
                }

                let selected = self.selected.clone();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for row in self.games.iter_mut() {
                            if restore_mode && row.restorable_at.is_none() {
                                continue;
                            }
                            let is_selected = selected.as_deref() == Some(row.name.as_str());
                            let verified = self.config.verified_saves.contains(&row.name);
                            if game_card(ui, row, is_selected, tex_ids.get(&row.name).copied(), verified) {
                                clicked = Some(row.name.clone());
                            }
                            ui.add_space(2.0);
                        }
                    });
            });

        if let Some(name) = clicked {
            self.selected = Some(name);
        }
    }

    // ---------- right: save file details tree ----------
    fn details(&mut self, ui: &mut egui::Ui) {
        // Button clicks inside the panel are deferred so the immutable
        // `row` borrow used for rendering can end first.
        let mut delete_clicked = false;
        let mut custom_clicked = false;
        let mut verify_clicked = false;
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(colors().bg)
                    .inner_margin(Margin::symmetric(14, 10)),
            )
            .show(ui, |ui| {
                let Some(row) = self.selected_row() else {
                    ui.add_space(12.0);
                    rtl_label(
                        ui,
                        RichText::new(tr("details.pick_game"))
                            .size(13.0)
                            .color(colors().text_weak),
                    );
                    return;
                };

                let verified = self.config.verified_saves.contains(&row.name);
                let status = row.status(verified);
                if self.config.show_game_icons
                    && let Some(id) =
                        self.image_cache.get(&row.name).map(|t| t.id())
                {
                    ui.add(egui::Image::new((id, Vec2::new(184.0, 86.0))));
                    ui.add_space(2.0);
                }
                ui.horizontal(|ui| {
                    status_dot(ui, status.color());
                    ui.label(RichText::new(i18n::display(&row.name)).size(17.0).strong());
                    ui.label(
                        RichText::new(trf("details.status_dash", &[("status", &status.label())]))
                            .size(12.0)
                            .color(status.color()),
                    );
                });

                // Per-game actions.
                let has_local = row
                    .scan
                    .as_ref()
                    .is_some_and(|s| !s.found_files.is_empty());
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            has_local && !self.busy(),
                            egui::Button::new(tr("details.delete_saves")),
                        )
                        .clicked()
                    {
                        delete_clicked = true;
                    }
                    if ui.button(tr("details.custom_path")).clicked() {
                        custom_clicked = true;
                    }
                    // Manual override for the orange "config only"
                    // heuristic — persists the game in config.verifiedSaves.
                    if verified {
                        rtl_label(
                            ui,
                            RichText::new(tr("details.verified"))
                                .size(11.0)
                                .color(C_GREEN),
                        );
                    } else if ui.button(tr("details.verify_saves")).clicked() {
                        verify_clicked = true;
                    }
                });
                if let Some(custom) = self.config.custom_game_path(&row.name) {
                    rtl_label(
                        ui,
                        RichText::new(trf("details.custom_path_label", &[("path", &custom)]))
                            .size(10.5)
                            .color(colors().accent),
                    );
                }

                // In Restore Mode, the relevant timestamp comes from the
                // restore source, not the backup target.
                let backup_when = match self.mode {
                    Mode::Backup => row.latest_backup,
                    Mode::Restore => row.restorable_at.or(row.latest_backup),
                };
                if let Some(when) = backup_when {
                    let local: chrono::DateTime<chrono::Local> = when.into();
                    ui.label(
                        RichText::new(trf(
                            "details.latest_backup",
                            &[("ts", &local.format("%Y-%m-%d %H:%M"))],
                        ))
                        .size(11.5)
                        .color(colors().text_weak),
                    );
                }

                // Backup history — full + differential snapshots read
                // from mapping.yaml in the mode-relevant layout, with a
                // fallback to the other layout.
                let primary_layout =
                    BackupLayout::new(match self.mode {
                        Mode::Backup => self.backup_dir.clone(),
                        Mode::Restore => PathBuf::from(&self.config.restore.path),
                    });
                let mut history = primary_layout.backup_history(&row.name);
                if history.is_empty() {
                    history = BackupLayout::new(match self.mode {
                        Mode::Backup => PathBuf::from(&self.config.restore.path),
                        Mode::Restore => self.backup_dir.clone(),
                    })
                    .backup_history(&row.name);
                }
                if !history.is_empty() {
                    ui.add_space(2.0);
                    ui.collapsing(
                        RichText::new(tr("details.history")).strong().size(13.0),
                        |ui| {
                            egui::Grid::new(ui.scope_id().with("backup_history"))
                                .striped(true)
                                .num_columns(4)
                                .spacing([28.0, 4.0])
                                .show(ui, |ui| {
                                    for label in [
                                        tr("history.type"),
                                        tr("history.date"),
                                        tr("history.files"),
                                        tr("history.size"),
                                    ] {
                                        ui.label(
                                            RichText::new(label).size(10.5).color(colors().text_weak),
                                        );
                                    }
                                    ui.end_row();
                                    for h in &history {
                                        let local: chrono::DateTime<chrono::Local> = h.when.into();
                                        let (kind, color) = if h.differential {
                                            (tr("history.diff"), C_YELLOW)
                                        } else {
                                            (tr("history.full"), C_GREEN)
                                        };
                                        ui.label(RichText::new(kind).size(11.5).color(color));
                                        ui.label(
                                            RichText::new(
                                                local.format("%Y-%m-%d %H:%M").to_string(),
                                            )
                                            .size(11.5),
                                        );
                                        ui.label(
                                            RichText::new(h.files.to_string())
                                                .size(11.5)
                                                .color(colors().text_weak),
                                        );
                                        ui.label(
                                            RichText::new(human_bytes(h.size))
                                                .size(11.5)
                                                .color(colors().text_weak),
                                        );
                                        ui.end_row();
                                    }
                                });
                        },
                    );
                }

                // Not-installed games have no local save data; show the
                // contents of their existing backup instead of "0 files".
                if status == Status::NotInstalled && !row.backup_files.is_empty() {
                    rtl_label(
                        ui,
                        RichText::new(tr("details.not_installed_banner"))
                            .size(12.0)
                            .color(C_PURPLE),
                    );
                    rtl_label(
                        ui,
                        RichText::new(trf(
                            "details.backup_files",
                            &[
                                ("files", &row.backup_files.len()),
                                (
                                    "size",
                                    &human_bytes(row.backup_files.iter().map(|(_, s)| *s).sum()),
                                ),
                            ],
                        ))
                        .size(11.5)
                        .color(colors().text_weak),
                    );
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);

                    let mut groups: BTreeMap<String, Vec<(&String, u64)>> = BTreeMap::new();
                    for (path, size) in &row.backup_files {
                        let (dir, _) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                        groups.entry(dir.to_string()).or_default().push((path, *size));
                    }
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (dir, files) in &groups {
                                let dir_size: u64 = files.iter().map(|(_, s)| *s).sum();
                                ui.collapsing(
                                    RichText::new(format!("{}/  ({})", dir, human_bytes(dir_size)))
                                        .strong()
                                        .size(12.5),
                                    |ui| {
                                        for (path, size) in files {
                                            let name =
                                                path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
                                            ui.horizontal(|ui| {
                                                ui.label(RichText::new(name).monospace().size(12.0));
                                                ui.with_layout(
                                                    Layout::right_to_left(Align::Center),
                                                    |ui| {
                                                        ui.label(
                                                            RichText::new(human_bytes(*size))
                                                                .size(10.5)
                                                                .color(colors().text_weak),
                                                        );
                                                    },
                                                );
                                            });
                                        }
                                    },
                                );
                            }
                        });
                    return;
                }

                let Some(scan) = &row.scan else {
                    rtl_label(
                        ui,
                        RichText::new(tr("details.not_scanned")).size(12.0).color(colors().text_weak),
                    );
                    return;
                };

                rtl_label(
                    ui,
                    RichText::new(trf(
                        "details.save_files",
                        &[
                            ("files", &scan.found_files.len()),
                            ("size", &human_bytes(scan.total_size())),
                        ],
                    ))
                    .size(11.5)
                    .color(colors().text_weak),
                );
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                if scan.found_files.is_empty() {
                    rtl_label(
                        ui,
                        RichText::new(tr("details.no_files"))
                            .size(12.0)
                            .color(colors().text_weak),
                    );
                    return;
                }

                // Group files by parent directory for the tree view.
                let mut groups: BTreeMap<String, Vec<(&String, &scanner::ScannedFile)>> = BTreeMap::new();
                for (path, file) in &scan.found_files {
                    let (dir, _) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                    groups.entry(dir.to_string()).or_default().push((path, file));
                }

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (dir, files) in &groups {
                            let dir_size: u64 = files.iter().map(|(_, f)| f.size).sum();
                            ui.collapsing(
                                RichText::new(format!("{}/  ({})", dir, human_bytes(dir_size)))
                                    .strong()
                                    .size(12.5),
                                |ui| {
                                    for (path, file) in files {
                                        let name = path.rsplit_once('/').map(|(_, n)| n).unwrap_or(path);
                                        file_row(ui, name, file);
                                    }
                                },
                            );
                        }
                    });
            });

        if delete_clicked {
            self.pending_delete = self.selected.clone();
        }
        if custom_clicked && let Some(name) = self.selected.clone() {
            self.draft_custom_path = self
                .config
                .custom_game_path(&name)
                .unwrap_or_default()
                .to_string();
            self.custom_path_error = None;
            self.editing_custom_path = Some(name);
        }
        if verify_clicked && let Some(name) = self.selected.clone() {
            self.config.verified_saves.insert(name.clone());
            self.status_line = match self.config.save() {
                Ok(()) => trf("details.verified_status", &[("name", &name)]),
                Err(e) => trf("settings.save_err", &[("err", &e)]),
            };
        }
    }
}

/// The full scan pipeline: manifest -> roots -> installed games ->
/// scan every manifest game for save files. Rows are emitted only for
/// games that actually have save files, an existing backup, or a
/// detected install — mirroring Ludusavi's full backup scan.
///
/// `custom_games` are the `customGames` entries from the shared
/// `config.yaml`: `integration: override` replaces a manifest game's
/// file paths; `extend` merges with them; games not in the manifest at
/// all are scanned as fully custom entries (same as Ludusavi).
fn run_scan(
    cache_dir: PathBuf,
    backup_dir: PathBuf,
    restore_dir: PathBuf,
    custom_games: Vec<config::CustomGame>,
    send: impl Fn(Msg),
) {
    let manifest = {
        let mut status = |text: String| send(Msg::Status(text));
        match Manifest::load(&cache_dir, &mut status) {
            Ok(m) => m,
            Err(e) => {
                send(Msg::ScanFinished {
                    games: 0,
                    error: Some(e),
                });
                return;
            }
        }
    };

    if manifest.0.is_empty() {
        // Defense in depth: `Manifest::load` already rejects empty
        // manifests, but a zero-entry manifest must never surface as a
        // successful "0 games scanned" result.
        send(Msg::ScanFinished {
            games: 0,
            error: Some(tr("scan.empty_manifest")),
        });
        return;
    }

    send(Msg::Status(trf(
        "scan.manifest_loaded",
        &[("count", &manifest.0.len())],
    )));

    let all_roots = roots::detect_roots();
    let steam_roots: Vec<&roots::Root> = all_roots
        .iter()
        .filter(|r| r.store == manifest::Store::Steam)
        .collect();
    let steam_games = roots::steam_games(&steam_roots);

    send(Msg::Status(trf(
        "scan.roots_found",
        &[("roots", &all_roots.len()), ("steam", &steam_games.len())],
    )));

    // Map installed Steam games to manifest entries.
    let steam_index = manifest.steam_index();
    let mut installed_names: HashSet<String> = HashSet::new();
    for sg in &steam_games {
        let manifest_name = steam_index
            .get(&sg.app_id)
            .cloned()
            .or_else(|| manifest.0.contains_key(&sg.name).then(|| sg.name.clone()));
        if let Some(name) = manifest_name {
            installed_names.insert(manifest.resolve_alias(&name));
        }
    }

    // Games that already have backups — in the backup target and in the
    // restore source (usually the same dir; kept separate so Restore
    // Mode reflects the restore source exactly).
    let layout = BackupLayout::new(backup_dir);
    let mut backup_games: BTreeMap<String, chrono::DateTime<chrono::Utc>> = BTreeMap::new();
    for (name, when) in layout.restorable_games() {
        backup_games.insert(manifest.resolve_alias(&name), when);
    }
    let mut restore_games: BTreeMap<String, chrono::DateTime<chrono::Utc>> = BTreeMap::new();
    let restore_layout = BackupLayout::new(restore_dir);
    for (name, when) in restore_layout.restorable_games() {
        restore_games.insert(manifest.resolve_alias(&name), when);
    }
    for (name, when) in &restore_games {
        if !backup_games.contains_key(name) {
            // A backup that only exists in the restore source still
            // counts as "has a backup" for status purposes.
            backup_games.insert(name.clone(), *when);
        }
    }

    // Custom paths from `customGames` (active entries only).
    let custom_index: BTreeMap<&str, &config::CustomGame> = custom_games
        .iter()
        .filter(|g| g.is_active())
        .map(|g| (g.name.as_str(), g))
        .collect();

    // Scan every manifest game that declares save paths — same semantics
    // as Ludusavi's full scan. Only games with findings are reported.
    let total = manifest.0.values().filter(|g| !g.files.is_empty()).count();
    let mut scanned = 0usize;
    for (name, game) in &manifest.0 {
        let custom = custom_index.get(name.as_str()).copied();
        if game.files.is_empty() && custom.is_none() {
            continue;
        }
        // `integration: override` replaces the manifest's file paths
        // entirely; `extend` merges the custom paths into them.
        let custom_game_storage;
        let effective: &Game = match custom {
            Some(c) => {
                let entries: BTreeMap<String, manifest::GameFileEntry> = c
                    .files
                    .iter()
                    .map(|p| (p.clone(), manifest::GameFileEntry::default()))
                    .collect();
                custom_game_storage = if c.extends() {
                    let mut g = game.clone();
                    g.files.extend(entries);
                    g
                } else {
                    Game {
                        files: entries,
                        ..Default::default()
                    }
                };
                &custom_game_storage
            }
            None => game,
        };
        if effective.files.is_empty() {
            continue;
        }
        scanned += 1;
        if scanned % 50 == 0 {
            send(Msg::Status(trf(
                "scan.progress",
                &[("scanned", &scanned), ("total", &total), ("name", name)],
            )));
        }

        scan_and_emit(
            name,
            effective,
            &all_roots,
            &steam_games,
            &installed_names,
            &backup_games,
            &restore_games,
            &layout,
            &restore_layout,
            &send,
        );
    }

    // Custom-only games (a `customGames` entry for a game that isn't in
    // the manifest at all) are scanned too, like Ludusavi does.
    for (name, c) in &custom_index {
        if manifest.0.contains_key(*name) {
            continue;
        }
        scanned += 1;
        let game = Game {
            files: c
                .files
                .iter()
                .map(|p| (p.clone(), manifest::GameFileEntry::default()))
                .collect(),
            ..Default::default()
        };
        scan_and_emit(
            name,
            &game,
            &all_roots,
            &steam_games,
            &installed_names,
            &backup_games,
            &restore_games,
            &layout,
            &restore_layout,
            &send,
        );
    }

    send(Msg::ScanFinished {
        games: scanned,
        error: None,
    });
}

/// Scan one game and emit a `GameScanned` row if it produced findings
/// (files, an existing backup, or a detected install).
#[allow(clippy::too_many_arguments)]
fn scan_and_emit(
    name: &str,
    game: &Game,
    all_roots: &[roots::Root],
    steam_games: &[roots::SteamGame],
    installed_names: &HashSet<String>,
    backup_games: &BTreeMap<String, chrono::DateTime<chrono::Utc>>,
    restore_games: &BTreeMap<String, chrono::DateTime<chrono::Utc>>,
    layout: &BackupLayout,
    restore_layout: &BackupLayout,
    send: &dyn Fn(Msg),
) {
    let previous_files = backup_games
        .contains_key(name)
        .then(|| layout.load_mapping(name))
        .flatten()
        .and_then(|m| m.latest_backup().map(|b| b.files.clone()));

    let scan =
        scanner::scan_game_for_backup(name, game, all_roots, steam_games, previous_files.as_ref());

    let has_files = !scan.found_files.is_empty();
    let installed = installed_names.contains(name) || has_files;
    let latest_backup = backup_games.get(name).copied();
    let restorable_at = restore_games.get(name).copied();

    if !has_files && latest_backup.is_none() && !installed_names.contains(name) {
        return;
    }

    // Contents of the newest snapshot, for displaying meaningful
    // data on not-installed games. Prefer the restore source's
    // mapping — that's the snapshot a restore would write back.
    let backup_files: Vec<(String, u64)> = if latest_backup.is_some() || restorable_at.is_some() {
        restore_layout
            .load_mapping(name)
            .or_else(|| layout.load_mapping(name))
            .and_then(|m| {
                m.latest_backup()
                    .map(|b| b.files.iter().map(|(p, f)| (p.clone(), f.size)).collect())
            })
            .unwrap_or_default()
    } else {
        vec![]
    };

    let path_hint = steam_games
        .iter()
        .find(|sg| game.steam_ids(None).contains(&sg.app_id))
        .map(|sg| format!("{}/steamapps/common/{}", sg.library, sg.install_dir))
        .or_else(|| {
            scan.found_files
                .keys()
                .next()
                .map(|f| f.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default())
        })
        .unwrap_or_default();

    send(Msg::GameScanned {
        name: name.to_string(),
        path_hint,
        steam_id: game.steam_ids(None).first().copied(),
        installed,
        scan,
        latest_backup,
        restorable_at,
        backup_files,
    });
}

/// Headless pipeline check: `--scan-debug` prints every message the UI
/// would receive, without opening the window.
fn debug_scan() {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = Config::load();
    let backup_dir = PathBuf::from(&config.backup.path);
    let restore_dir = PathBuf::from(&config.restore.path);
    let custom_games = config.custom_games.clone();
    run_scan(base.join("cache"), backup_dir, restore_dir, custom_games, |msg| match msg {
        Msg::Status(s) => println!("[status] {s}"),
        Msg::GameScanned {
            name,
            path_hint,
            steam_id,
            installed,
            scan,
            latest_backup,
            restorable_at,
            backup_files,
        } => {
            println!(
                "[row] {} | installed={} | files={} | size={} | backup={:?} | restorable={:?} | backup_files={}",
                name,
                installed,
                scan.found_files.len(),
                human_bytes(scan.total_size()),
                latest_backup.map(|w| w.format("%Y-%m-%d").to_string()),
                restorable_at.map(|w| w.format("%Y-%m-%d").to_string()),
                backup_files.len()
            );
            if !path_hint.is_empty() {
                println!("      hint: {path_hint}");
            }
            if let Some(id) = steam_id {
                println!("      steam: {id}");
            }
        }
        Msg::ScanFinished { games, error } => println!("[done] scanned={games} error={error:?}"),
        Msg::BackupDone(r) => println!("[backup] {} -> {:?}", r.game_name, r.backup_name),
        Msg::BackupFinished => println!("[backup] finished"),
        Msg::RestoreDone(r) => println!("[restore] {} -> {:?} ({} files)", r.game_name, r.backup_name, r.files_restored),
        Msg::RestoreFinished => println!("[restore] finished"),
        Msg::ManifestUpdateDone { error, modified, .. } => {
            println!("[manifest] modified={modified} error={error:?}")
        }
        Msg::CloudSyncDone(r) => println!("[cloud] {r:?}"),
    });
}

// ---------- small widgets ----------
/// Set `checked` for every game matching `pred`, limited to the games
/// visible in the current mode (restore mode: only restorable games).
/// Semantics are "replace selection" — the bulk action selects exactly
/// the matching subset.
fn bulk_check(games: &mut [GameRow], restore_mode: bool, pred: impl Fn(&GameRow) -> bool) {
    for g in games.iter_mut() {
        if restore_mode && g.restorable_at.is_none() {
            continue;
        }
        g.checked = pred(g);
    }
}

/// UTC timestamp -> "YYYY-MM-DD HH:MM" in local time.
fn fmt_dt(dt: chrono::DateTime<chrono::Utc>) -> String {
    let local: chrono::DateTime<chrono::Local> = dt.into();
    local.format("%Y-%m-%d %H:%M").to_string()
}

/// `backup.rs` produces English skip-reason strings; map them to locale
/// keys. Unknown reasons pass through verbatim.
fn translate_reason(reason: &str) -> String {
    let key = match reason {
        "no save files found" => "reason.no_save_files",
        "no changes since last backup" => "reason.no_changes",
        "no files could be copied" => "reason.no_files_copied",
        "no backup found" => "reason.no_backup",
        "backup has no snapshots" => "reason.no_snapshots",
        _ => return reason.to_string(),
    };
    tr(key)
}

/// `ScanChange::label()` is a backend string; map to `change.*` keys.
fn translate_change(change: ScanChange) -> String {
    tr(match change {
        ScanChange::New => "change.new",
        ScanChange::Different => "change.different",
        ScanChange::Same => "change.same",
        ScanChange::Unknown => "change.unknown",
    })
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn cover_placeholder(ui: &mut egui::Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(42.0, 56.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5, colors().card_bg);
    p.rect_stroke(rect, 5, Stroke::new(1.0, colors().border), StrokeKind::Inside);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(18.0),
        colors().text_weak,
    );
}

fn game_card(
    ui: &mut egui::Ui,
    row: &mut GameRow,
    is_selected: bool,
    image: Option<egui::TextureId>,
    verified: bool,
) -> bool {
    let status = row.status(verified);
    // Not-installed games have no local files — show what the backup
    // contains instead of a misleading "0 files · 0 B".
    let showing_backup = status == Status::NotInstalled && !row.backup_files.is_empty();
    let (files, size) = if showing_backup {
        (
            row.backup_files.len(),
            row.backup_files.iter().map(|(_, s)| *s).sum(),
        )
    } else {
        row.scan
            .as_ref()
            .map(|s| (s.found_files.len(), s.total_size()))
            .unwrap_or((0, 0))
    };

    // The card-wide `ui.interact` below is registered after the checkbox,
    // so it wins hit-testing and would swallow the checkbox's clicks.
    // We capture the checkbox's rect and toggle `checked` manually when
    // the card-click lands inside it.
    let mut checkbox_rect = egui::Rect::NOTHING;
    let inner = Frame::new()
        .fill(if is_selected { colors().select_bg } else { colors().card_bg })
        .stroke(Stroke::new(1.0, if is_selected { colors().accent } else { colors().border }))
        .corner_radius(8)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                checkbox_rect = ui.checkbox(&mut row.checked, "").rect;
                status_dot(ui, status.color());
                let initial: String = row.name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                // The image takes the cover slot — wide because the
                // Steam header is 460x215 (~2.14:1).
                match image {
                    Some(id) => {
                        ui.add(egui::Image::new((id, Vec2::new(120.0, 56.0))));
                    }
                    None => cover_placeholder(ui, &initial),
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(i18n::display(&row.name)).strong().size(13.5));
                    if !row.path_hint.is_empty() {
                        ui.label(RichText::new(&row.path_hint).size(10.5).color(colors().text_weak));
                    }
                    ui.label(
                        RichText::new(trf(
                            "card.files",
                            &[
                                ("status", &status.label()),
                                ("files", &files),
                                (
                                    "in_backup",
                                    &if showing_backup {
                                        tr("card.in_backup")
                                    } else {
                                        String::new()
                                    },
                                ),
                                ("size", &human_bytes(size)),
                            ],
                        ))
                        .size(11.0)
                        .color(status.color()),
                    );
                });
            });
        });

    // The game name (unique per row) is the stable interact id — sorting
    // changes row order, so an index-based id would drift.
    let response = ui.interact(
        inner.response.rect,
        ui.scope_id().with(("game_card", &row.name)),
        Sense::click(),
    );
    if response.hovered() && !is_selected {
        ui.painter().rect_filled(
            inner.response.rect,
            8,
            Color32::from_rgba_unmultiplied(colors().hover_bg.r(), colors().hover_bg.g(), colors().hover_bg.b(), 60),
        );
    }
    if !response.clicked() {
        return false;
    }
    if let Some(pos) = response.interact_pointer_pos()
        && checkbox_rect.contains(pos)
    {
        row.checked = !row.checked;
    }
    true
}

fn file_row(ui: &mut egui::Ui, name: &str, file: &scanner::ScannedFile) {
    ui.horizontal(|ui| {
        let change_color = match file.change {
            ScanChange::New => C_GREEN,
            ScanChange::Different => C_YELLOW,
            ScanChange::Same => colors().text_weak,
            ScanChange::Unknown => colors().text_weak,
        };
        ui.label(RichText::new(name).monospace().size(12.0));
        ui.label(RichText::new(translate_change(file.change)).size(10.5).color(change_color));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mtime = file
                .mtime
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Local> = t.into();
                    dt.format("%Y-%m-%d %H:%M").to_string()
                })
                .unwrap_or_default();
            ui.label(
                RichText::new(format!("{} — {}", human_bytes(file.size), mtime))
                    .size(10.5)
                    .color(colors().text_weak),
            );
        });
    });
}

/// Embedded Arabic font — SIL-OFL Amiri. Chosen because its cmap maps
/// Arabic presentation forms (U+FB50–FEFF) to real, distinct form
/// glyphs. Windows system fonts (Segoe UI/Tahoma/Arial) *do* contain
/// FExx mappings but collapse initial/medial forms onto the nominal
/// glyph — pre-shaped text renders disconnected there. Verified by
/// `font_coverage_probe`.
const ARABIC_FONT_NAME: &str = "Amiri";
const ARABIC_FONT: &[u8] = include_bytes!("../assets/Amiri-Regular.ttf");

/// egui ships without Arabic glyphs. Build FontDefinitions with the
/// embedded Amiri as the first Arabic-capable entry in both families —
/// egui's own fonts keep priority for Latin text; Amiri serves every
/// Arabic codepoint (nominal + presentation forms). `tradbdo` is a
/// working system-font backup; the collapsed fonts are last resort.
fn arabic_font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        ARABIC_FONT_NAME.to_string(),
        egui::FontData::from_static(ARABIC_FONT).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(family).or_default();
        list.push(ARABIC_FONT_NAME.to_string());
    }
    // Extra coverage after Amiri (missing chars in the embedded font
    // fall through to these, worst case rendering isolated forms).
    for (name, path) in [
        ("Traditional Arabic", "C:/Windows/Fonts/tradbdo.ttf"),
        ("Segoe UI", "C:/Windows/Fonts/segoeui.ttf"),
        ("Tahoma", "C:/Windows/Fonts/tahoma.ttf"),
        ("Arial", "C:/Windows/Fonts/arial.ttf"),
    ] {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert(name.to_string(), egui::FontData::from_owned(bytes).into());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push(name.to_string());
        }
    }
    fonts
}

fn install_fallback_fonts(ctx: &egui::Context) {
    ctx.set_fonts(arabic_font_definitions());
}

/// Right-aligned label for paragraph text in RTL mode — plain label in
/// LTR. Keeps Arabic hint/banner text flush-right without disturbing
/// control rows.
fn rtl_label(ui: &mut egui::Ui, text: RichText) {
    if is_rtl() {
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            ui.label(text);
        });
    } else {
        ui.label(text);
    }
}

/// Open the native folder picker — on Windows this is the modern
/// Explorer-style `IFileOpenDialog` (via `rfd`), not the legacy
/// WinForms tree view. Runs modal on the UI thread like any dialog.
fn browse_folder(initial: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    if !initial.trim().is_empty() {
        dialog = dialog.set_directory(initial);
    }
    dialog
        .pick_folder()
        .map(|p| p.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal row with explicit local-scan and backup file paths, so
    /// tests can exercise the file-type-aware "config only" heuristic.
    fn row(
        installed: bool,
        scan_paths: &[&str],
        backup_paths: &[&str],
        latest_backup: Option<chrono::DateTime<chrono::Utc>>,
    ) -> GameRow {
        let found_files = scan_paths
            .iter()
            .map(|p| {
                (
                    p.to_string(),
                    scanner::ScannedFile {
                        size: 4,
                        hash: String::new(),
                        change: ScanChange::Unknown,
                        mtime: None,
                    },
                )
            })
            .collect();
        GameRow {
            name: "T".to_string(),
            path_hint: String::new(),
            steam_id: None,
            installed,
            checked: false,
            scan: Some(scanner::ScanInfo {
                game_name: "T".to_string(),
                found_files,
            }),
            latest_backup,
            restorable_at: None,
            backup_files: backup_paths.iter().map(|p| (p.to_string(), 4u64)).collect(),
        }
    }

    /// "Config only" heuristic: only config-type files — or no files at
    /// all — warns orange even ahead of not-installed, unless the user
    /// verified the saves.
    #[test]
    fn config_only_status_and_verified_bypass() {
        let now = chrono::Utc::now();
        // Installed + empty scan + no backup -> orange warning.
        let g = row(true, &[], &[], None);
        assert!(matches!(g.status(false), Status::ConfigOnly));
        // Verified -> bypasses to the normal "no backup" red.
        assert!(matches!(g.status(true), Status::NoBackup));
        // The warning still wins over a recent backup (saves are
        // missing NOW); verified reveals the underlying green.
        let g = row(true, &[], &[], Some(now));
        assert!(matches!(g.status(false), Status::ConfigOnly));
        assert!(matches!(g.status(true), Status::RecentBackup));
        // Installed with only config files found -> orange.
        let g = row(true, &["C:/g/config.ini", "C:/g/settings.json"], &[], None);
        assert!(matches!(g.status(false), Status::ConfigOnly));
        // Config files alongside real save data -> normal status.
        let g = row(true, &["C:/g/config.ini", "C:/g/save.sav"], &[], None);
        assert!(matches!(g.status(false), Status::NoBackup));
        // Not installed but the backup holds only config files ->
        // orange outranks purple.
        let g = row(false, &[], &["config.ini", "prefs.cfg"], None);
        assert!(matches!(g.status(false), Status::ConfigOnly));
        // Verified not-installed + config-only backup -> purple.
        assert!(matches!(g.status(true), Status::NotInstalled));
        // Not installed with real save data in the backup -> purple.
        let g = row(false, &[], &["config.ini", "progress.sav"], None);
        assert!(matches!(g.status(false), Status::NotInstalled));
        let g = row(false, &[], &["progress.sav"], None);
        assert!(matches!(g.status(false), Status::NotInstalled));
        // Games that actually have save files are unaffected.
        let g = row(true, &["C:/saves/a.sav", "C:/saves/b.dat"], &[], None);
        assert!(matches!(g.status(false), Status::NoBackup));
    }

    /// End-to-end font verification: run the ACTUAL display string for
    /// "الإعدادات" through egui's text layout and prove every Arabic
    /// glyph resolves to the embedded Amiri font (not a system font
    /// that collapses presentation forms onto nominal glyphs).
    #[test]
    fn arabic_text_resolves_to_embedded_font() {
        // The exact string `tr("settings.title")` hands to egui —
        // presentation forms in LOGICAL order (per font-run): harfrust
        // reorders each Arabic run to visual order at shape time.
        let shaped = i18n::display("الإعدادات");
        assert!(!shaped.is_empty());
        // Sanity: it must be in presentation forms by now.
        assert!(shaped
            .chars()
            .any(|c| ('\u{FB50}'..='\u{FEFF}').contains(&c)));
        // Single-run word -> logical order = starts with ALEF.
        assert_eq!(shaped.chars().next(), Some('\u{FE8D}'));

        let latin = |defs: &egui::FontDefinitions| {
            let mut fonts =
                egui::epaint::Fonts::new(Default::default(), defs.clone());
            fonts
                .with_pixels_per_point(1.0)
                .layout_no_wrap(
                    "Test".to_string(),
                    egui::FontId::proportional(16.0),
                    Color32::WHITE,
                )
        };
        let arabic = |defs: &egui::FontDefinitions| {
            let mut fonts =
                egui::epaint::Fonts::new(Default::default(), defs.clone());
            fonts
                .with_pixels_per_point(1.0)
                .layout_no_wrap(
                    shaped.clone(),
                    egui::FontId::proportional(16.0),
                    Color32::WHITE,
                )
        };
        let metrics =
            |g: &egui::epaint::text::Glyph| (g.font_face_height, g.font_face_ascent);

        let defs = arabic_font_definitions();

        // With Amiri installed: every Arabic glyph is real (nonzero
        // advance, codepoint preserved in the galley).
        let galley = arabic(&defs);
        let glyphs: Vec<_> = galley.rows.iter().flat_map(|r| r.glyphs.iter()).collect();
        assert!(!glyphs.is_empty());
        for g in &glyphs {
            println!(
                "glyph U+{:04X} adv={:.1} x={:.1}",
                g.chr as u32, g.advance_width, g.pos.x
            );
        }
        // Screen position is what paints: harfrust reversed the Arabic
        // run at shape time, so from the logical-order input string the
        // painted row must put TEH (U+FE95 — last letter of الإعدادات)
        // at the leftmost x and ALEF (U+FE8D) at the rightmost —
        // i.e. visual RTL order.
        let mut painted: Vec<_> = glyphs
            .iter()
            .filter(|g| g.advance_width > 0.0)
            .collect();
        assert!(!painted.is_empty());
        painted.sort_by(|a, b| a.pos.x.total_cmp(&b.pos.x));
        assert_eq!(
            painted[0].chr,
            '\u{FE95}',
            "leftmost glyph should be TEH (visual-left), got U+{:04X}",
            painted[0].chr as u32
        );
        assert_eq!(
            painted[painted.len() - 1].chr,
            '\u{FE8D}',
            "rightmost glyph should be ALEF (visual-right), got U+{:04X}",
            painted[painted.len() - 1].chr as u32
        );
        for g in &glyphs {
            assert_ne!(g.chr, '\u{FFFD}', "replacement char for U+{:04X}", g.chr as u32);
        }

        // Arabic glyphs come from a DIFFERENT font face than Latin —
        // i.e. the fallback chain engaged (egui's defaults lack Arabic).
        let latin_metrics = {
            let g = &latin(&defs).rows[0].glyphs[0];
            metrics(g)
        };
        for g in &glyphs {
            assert_ne!(
                metrics(g),
                latin_metrics,
                "Arabic glyph rendered by the Latin font — fallback broken"
            );
        }

        // Prove the serving face is AMIRI, not the collapsing system
        // fonts: rebuild definitions WITHOUT it and compare metrics —
        // a different face must take over, producing different metrics.
        let mut no_amiri = egui::FontDefinitions::default();
        for (name, path) in [
            ("Segoe UI", "C:/Windows/Fonts/segoeui.ttf"),
            ("Tahoma", "C:/Windows/Fonts/tahoma.ttf"),
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                no_amiri
                    .font_data
                    .insert(name.to_string(), egui::FontData::from_owned(bytes).into());
                for family in
                    [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
                {
                    no_amiri
                        .families
                        .entry(family)
                        .or_default()
                        .push(name.to_string());
                }
            }
        }
        if no_amiri.font_data.len() > egui::FontDefinitions::default().font_data.len() {
            let without_amiri = arabic(&no_amiri);
            let g_wo = &without_amiri.rows[0].glyphs[0];
            let g_with = &glyphs[0];
            assert_ne!(
                metrics(g_with),
                metrics(g_wo),
                "same font face with and without Amiri — Amiri is not being used"
            );
        }
    }

    /// A cleared/invalid `restore.path` must not hang, panic, or
    /// silently kill the scan — ScanFinished must still arrive.
    #[test]
    fn run_scan_survives_bad_restore_path() {
        let base = std::env::temp_dir().join("gsc-tests").join("scan-bad-restore");
        let _ = std::fs::remove_dir_all(&base);
        let cache = base.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        // Tiny local manifest so no network/cache dependency.
        std::fs::write(
            cache.join("manifest.yaml"),
            "Test Game:\n  files:\n    \"C:/definitely-not-here-gsc-test/saves\":\n      tags:\n        - save\n",
        )
        .unwrap();

        // Both an empty path (cleared field, pre-fix) and a nonexistent
        // nested path must behave identically: scan completes.
        for bogus in [
            PathBuf::new(),
            base.join("nonexistent").join("deeply").join("nested"),
        ] {
            let (tx, rx) = std::sync::mpsc::channel::<Msg>();
            run_scan(
                cache.clone(),
                base.join("backups"),
                bogus.clone(),
                vec![],
                move |m| {
                    tx.send(m).unwrap();
                },
            );
            // Iterating ends only when the senders drop — proves no hang.
            let msgs: Vec<Msg> = rx.iter().collect();
            assert!(
                msgs.iter()
                    .any(|m| matches!(m, Msg::ScanFinished { error: None, .. })),
                "restore dir {bogus:?} broke the scan"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}

// ---------- entry ----------
/// Decode the embedded `assets/app-icon.png` for the window/taskbar
/// icon. Returns `None` if the bytes ever fail to decode — the window
/// just keeps the OS default in that case.
fn app_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../assets/app-icon.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--scan-debug") {
        debug_scan();
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: {
            let mut v = egui::ViewportBuilder::default()
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([980.0, 640.0]);
            if let Some(icon) = app_icon() {
                v = v.with_icon(std::sync::Arc::new(icon));
            }
            v
        },
        ..Default::default()
    };
    eframe::run_native(
        "Game Save Checkpoint",
        options,
        Box::new(|cc| Ok(Box::new(CheckpointApp::new(cc)))),
    )
}

impl eframe::App for CheckpointApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Theme from the shared config — re-applies visuals + palette
        // when the user flips it in Settings (persisted on Save).
        if self.config.theme != self.theme_applied {
            apply_theme(ui.ctx(), self.config.theme);
            self.theme_applied = self.config.theme;
        }
        self.poll_messages();
        self.poll_images(ui.ctx());
        if self.busy() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
        }

        self.header(ui);
        self.toolbar(ui);
        self.game_list(ui);
        self.details(ui);
        self.settings_window(ui.ctx());
        self.backup_summary_window(ui.ctx());
        self.restore_confirm_window(ui.ctx());
        self.delete_confirm_window(ui.ctx());
        self.custom_path_window(ui.ctx());
    }
}
