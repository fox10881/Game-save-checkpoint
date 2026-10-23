//! Game image extraction.
//!
//! Primary source: the Steam CDN store header for the game's appid
//! (`steam.id` from Ludusavi's `manifest.yaml`), downloaded once and
//! cached under `cache/images/`.
//!
//! Fallback: locate the game's executable inside its install directory
//! and pull the embedded icon out of the PE resources — `pelite`
//! reassembles the RT_GROUP_ICON into a standard `.ico` file and the
//! `ico` crate decodes it.
//!
//! Pure Rust — no Win32 calls — so it compiles and is testable
//! everywhere.

use std::io::Read;
use std::path::{Path, PathBuf};

use egui::ColorImage;

/// Stems that mark an executable as a helper, never the game itself.
const IGNORED_STEMS: &[&str] = &[
    "unins", "uninst", "setup", "install", "vcredist", "dxsetup", "crash", "report", "updat",
    "patch", "redist", "dotnet", "helper", "service", "eac", "anticheat", "dumper",
];

fn normalized(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn collect_exes(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            out.push(p);
        } else if depth > 0 && p.is_dir() {
            collect_exes(&p, depth - 1, out);
        }
    }
}

/// Pick the most likely game exe inside `dir`. Searches the top level
/// plus two levels of subdirectories (covers `Binaries/Win64`-style
/// layouts), drops known helper executables, then prefers an exe
/// named after the install dir or the game, then the biggest file.
pub fn find_exe(dir: &Path, game_name: &str) -> Option<PathBuf> {
    let mut exes = Vec::new();
    collect_exes(dir, 2, &mut exes);
    exes.retain(|p| {
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        !IGNORED_STEMS.iter().any(|bad| stem.contains(bad))
    });
    if exes.is_empty() {
        return None;
    }

    let dir_key = normalized(&dir.file_name()?.to_string_lossy());
    let game_key = normalized(game_name);
    let mut scored: Vec<_> = exes
        .into_iter()
        .map(|p| {
            let stem = normalized(&p.file_stem().unwrap_or_default().to_string_lossy());
            // Name affinity: "palworld" dir vs "Palworld-Win64-Shipping" exe.
            let name_match = !stem.is_empty()
                && ((!dir_key.is_empty() && stem.contains(&dir_key))
                    || (!game_key.is_empty() && stem.contains(&game_key)));
            let size = p.metadata().map(|m| m.len()).unwrap_or(0);
            (name_match, size, p)
        })
        .collect();
    // Best first: name match, then largest exe.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    Some(scored.remove(0).2)
}

/// Extract the largest icon embedded in `exe` as RGBA pixels.
pub fn extract_icon(exe: &Path) -> Option<ColorImage> {
    let data = std::fs::read(exe).ok()?;
    let pe = pelite::PeFile::from_bytes(&data).ok()?;
    let resources = pe.resources().ok()?;

    let mut best: Option<(u32, ColorImage)> = None;
    for (_name, group) in resources.icons().filter_map(Result::ok) {
        // Reassemble the group into a standard .ico file, then decode.
        let mut ico_buf = Vec::new();
        if group.write(&mut ico_buf).is_err() {
            continue;
        }
        let Ok(dir) = ico::IconDir::read(std::io::Cursor::new(&ico_buf)) else {
            continue;
        };
        for entry in dir.entries() {
            let Ok(img) = entry.decode() else {
                continue;
            };
            let (w, h) = (img.width(), img.height());
            let score = w.max(h);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                let rgba = img.rgba_data();
                if rgba.len() == (w as usize) * (h as usize) * 4 {
                    best = Some((
                        score,
                        ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba),
                    ));
                }
            }
        }
    }
    best.map(|(_, img)| img)
}

/// Locate + extract in one step. `hint` is the directory most likely to
/// hold the exe — the Steam install dir when known, else a save dir.
pub fn icon_for_game(game_name: &str, hint_dir: &str) -> Option<ColorImage> {
    if hint_dir.is_empty() {
        return None;
    }
    let dir = PathBuf::from(hint_dir);
    if !dir.is_dir() {
        return None;
    }
    let exe = find_exe(&dir, game_name)?;
    extract_icon(&exe)
}

/// Steam CDN store-header URL for an appid — no API key needed.
fn steam_header_url(app_id: u32) -> String {
    format!(
        "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{app_id}/header.jpg"
    )
}

/// Where a Steam header is kept between runs.
fn steam_cache_path(cache_dir: &Path, app_id: u32) -> PathBuf {
    cache_dir.join("images").join(format!("{app_id}.jpg"))
}

/// The wide Steam header image (460x215) for `app_id`: disk cache hit
/// first, otherwise one GET against Steam's CDN which is then written
/// to the cache so it is never fetched twice.
fn steam_header(app_id: u32, cache_dir: &Path) -> Option<ColorImage> {
    let disk = steam_cache_path(cache_dir, app_id);
    let bytes = if disk.is_file() {
        std::fs::read(&disk).ok()?
    } else {
        let response = ureq::get(&steam_header_url(app_id))
            .timeout(std::time::Duration::from_secs(8))
            .call()
            .ok()?;
        let mut buf = Vec::new();
        // Headers are ~50-150KB; cap the read so a bad endpoint can't
        // stream forever.
        response
            .into_reader()
            .take(2 * 1024 * 1024)
            .read_to_end(&mut buf)
            .ok()?;
        if let Some(dir) = disk.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            std::fs::write(&disk, &buf).ok();
        }
        buf
    };
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some(ColorImage::from_rgba_unmultiplied([w, h], img.as_raw()))
}

/// Full fallback chain for the card image: Steam CDN header (when the
/// game has a `steam.id`), else the embedded exe icon.
pub fn fetch_game_image(
    game_name: &str,
    steam_id: Option<u32>,
    hint_dir: &str,
    cache_dir: &Path,
) -> Option<ColorImage> {
    if let Some(id) = steam_id
        && let Some(img) = steam_header(id, cache_dir)
    {
        return Some(img);
    }
    icon_for_game(game_name, hint_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_exe_prefers_matching_name_and_ignores_helpers() {
        let dir = std::env::temp_dir().join(format!("gsc_icon_{}", std::process::id()));
        let sub = dir.join("Binaries").join("Win64");
        std::fs::create_dir_all(&sub).unwrap();
        let helper = dir.join("uninstall.exe");
        let small = dir.join("util.exe");
        let game = sub.join("coolgame-win64-shipping.exe");
        std::fs::write(&helper, b"x").unwrap();
        std::fs::write(&small, b"xx").unwrap();
        std::fs::write(&game, b"xxx").unwrap();

        // Name affinity beats size.
        let found = find_exe(&dir, "CoolGame").unwrap();
        assert!(found.ends_with("coolgame-win64-shipping.exe"));
        // Helpers are excluded: rename the game exe away, util.exe wins.
        let moved = sub.join("coolgame2.exe");
        std::fs::rename(&game, &moved).unwrap();
        let found = find_exe(&dir, "zz-no-match").unwrap();
        assert!(found.ends_with("coolgame2.exe"), "largest exe wins");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn steam_header_serves_disk_cache() {
        // Write a PNG under the .jpg cache name — decoding sniffs the
        // magic bytes, so the extension is irrelevant. This proves the
        // cache path round-trips without any network.
        let dir = std::env::temp_dir().join(format!("gsc_steam_{}", std::process::id()));
        let app_id = 4_294_000_000u32;
        let path = steam_cache_path(&dir, app_id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 2, image::Rgba([7, 8, 9, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&path, &png).unwrap();

        let img = steam_header(app_id, &dir).expect("cached image should decode");
        assert_eq!(img.size, [4, 2]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn icon_for_game_returns_none_for_missing_dirs() {
        assert!(icon_for_game("Nope", "").is_none());
        assert!(icon_for_game("Nope", "Z:/definitely/missing/dir").is_none());
        // A real dir with no exe also yields nothing.
        let dir = std::env::temp_dir().join(format!("gsc_noexe_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(icon_for_game("Nope", dir.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
