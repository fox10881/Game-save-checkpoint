//! Common system paths and glob helpers.
//!
//! Adapted from Ludusavi's `src/path.rs` (`CommonPath` and the
//! "globbable" path handling in `StrictPath`).

use std::sync::LazyLock;

/// Well-known system folders used to expand manifest placeholders.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum CommonPath {
    Config,
    Data,
    DataLocal,
    DataLocalLow,
    Document,
    Home,
    Public,
    SavedGames,
    WindowsProgramData,
    WindowsSystemRoot,
}

fn check_dir(path: Option<std::path::PathBuf>) -> Option<String> {
    Some(normalize(&path?.to_string_lossy()))
}

static CONFIG: LazyLock<Option<String>> = LazyLock::new(|| check_dir(dirs::config_dir()));
static DATA: LazyLock<Option<String>> = LazyLock::new(|| check_dir(dirs::data_dir()));
static DATA_LOCAL: LazyLock<Option<String>> = LazyLock::new(|| check_dir(dirs::data_local_dir()));
static DATA_LOCAL_LOW: LazyLock<Option<String>> =
    LazyLock::new(|| check_dir(dirs::home_dir().map(|x| x.join("AppData").join("LocalLow"))));
static DOCUMENT: LazyLock<Option<String>> = LazyLock::new(|| check_dir(dirs::document_dir()));
static HOME: LazyLock<Option<String>> = LazyLock::new(|| check_dir(dirs::home_dir()));
static PUBLIC: LazyLock<Option<String>> = LazyLock::new(|| check_dir(dirs::public_dir()));
static SAVED_GAMES: LazyLock<Option<String>> =
    LazyLock::new(|| check_dir(dirs::home_dir().map(|x| x.join("Saved Games"))));
static WINDOWS_PROGRAM_DATA: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::var("ProgramData")
        .ok()
        .map(|x| normalize(&x))
        .or_else(|| Some("C:/ProgramData".to_string()))
});
static WINDOWS_SYSTEM_ROOT: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::var("windir")
        .ok()
        .map(|x| normalize(&x))
        .or_else(|| Some("C:/Windows".to_string()))
});

impl CommonPath {
    pub fn get(&self) -> Option<&'static str> {
        match self {
            Self::Config => CONFIG.as_deref(),
            Self::Data => DATA.as_deref(),
            Self::DataLocal => DATA_LOCAL.as_deref(),
            Self::DataLocalLow => DATA_LOCAL_LOW.as_deref(),
            Self::Document => DOCUMENT.as_deref(),
            Self::Home => HOME.as_deref(),
            Self::Public => PUBLIC.as_deref(),
            Self::SavedGames => SAVED_GAMES.as_deref(),
            Self::WindowsProgramData => WINDOWS_PROGRAM_DATA.as_deref(),
            Self::WindowsSystemRoot => WINDOWS_SYSTEM_ROOT.as_deref(),
        }
    }
}

/// The OS user name, used for the `<osUserName>` placeholder.
pub static OS_USERNAME: LazyLock<String> = LazyLock::new(|| {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "*".to_string())
});

/// Normalize a path: forward slashes, no trailing slash, no `\\?\` prefix.
pub fn normalize(path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    if let Some(stripped) = p.strip_prefix("//?/") {
        p = stripped.to_string();
    }
    while p.len() > 3 && p.ends_with('/') {
        p.pop();
    }
    p
}

/// Escape glob metacharacters in a literal path so it can be embedded
/// in a glob pattern (equivalent of `globset::escape` in Ludusavi).
pub fn escape_glob(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '[' => out.push_str("[[]"),
            ']' => out.push_str("[]]"),
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            _ => out.push(ch),
        }
    }
    out
}

/// Split a normalized absolute path into (drive, remainder), like
/// Ludusavi's `StrictPath::split_drive`. `C:/Users/foo` -> (`C:`, `Users/foo`).
pub fn split_drive(path: &str) -> (String, String) {
    let normalized = normalize(path);
    if let Some(rest) = normalized.strip_prefix("//") {
        // UNC path: \\server\share\...
        return (format!("\\\\{}", rest.split('/').next().unwrap_or("")), {
            let mut parts = rest.splitn(2, '/');
            parts.next();
            parts.next().unwrap_or("").to_string()
        });
    }
    let bytes = normalized.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = normalized[..2].to_uppercase();
        let rest = normalized[2..].trim_start_matches('/').to_string();
        (drive, rest)
    } else {
        (String::new(), normalized.trim_start_matches('/').to_string())
    }
}

/// Render bytes as a human-readable size.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}
