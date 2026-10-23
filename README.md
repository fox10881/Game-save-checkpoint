

# Game save CHECKPOINT

🌐 [العربية](README_ar.md) | [English](README.md)

A native, fast desktop companion for [Ludusavi](https://github.com/mtkennerly/ludusavi) —
the open-source game-save backup tool. Built in Rust with
[egui](https://github.com/emilk/egui), it reimplements Ludusavi's engine
directly (no subprocess, no dependency on the Ludusavi crate) while using
Ludusavi's exact `config.yaml` schema and backup layout, so both apps
interoperate seamlessly.

<img width="3840" height="2160" alt="Backup mode — the game list with status colors, Steam art, and live counters" src="https://github.com/user-attachments/assets/d2102940-51a9-4f35-952e-1c52a75747d9" />
<img width="3840" height="2160" alt="Details and confirmation views" src="https://github.com/user-attachments/assets/7f4f02cb-99ce-49ee-b5e1-a5d42e5968bb" />

![Platform](https://img.shields.io/badge/platform-Windows-blue)
![Rust](https://img.shields.io/badge/rust-edition%202024-orange)

## 🎨 Smart Status Colors

Every game row carries a colored status dot, a live counter in the header,
and a matching bulk-select button — so you can see *and* act on the whole
library's health at a glance. The colors are computed per game from real
signals: install detection, save-file hashes, file mtimes, and backup
contents — not just "does a backup exist".

| | Color | Meaning |
|---|---|---|
| 🔴 | **Red — No Backup** | Game detected locally, but never backed up. |
| 🟡 | **Yellow — Needs Backup / Old Backup** | Local files are newer than the newest snapshot, or file content diverged from it (strict timestamp + hash check). A (differential) backup is due. |
| 🟢 | **Green — Recent Backup** | Fully synchronized — the backup strictly matches local progress (hash-verified, so restored files with preserved old timestamps still count). |
| 🟣 | **Purple — Not Installed** | A backup exists in the vault, but the game is not currently installed on the system. |
| 🟠 | **Orange — Config Only** | Smart heuristic warning! The scan — or the backup itself — contains only config-type files (`.ini`, `.cfg`, `.json`, …), no actual save data. Dismiss per game with **Verify Saves**. |

## Features

- **Ludusavi-compatible engine** — manifest loading with mirror fallback,
  PCGamingWiki game data, placeholder path expansion (`<winDocuments>`,
  `<home>`, store-root detection), installed-game detection via Steam
  libraries, and backups written in Ludusavi's native `mapping.yaml` +
  `drive-<X>/` folder format. Backups made here are restorable by Ludusavi
  and vice-versa.
- **100% portable configuration** — `config.yaml` is created, read and
  saved in the same directory as the `.exe` (resolved via
  `std::env::current_exe()`), never AppData. On first run it is seeded
  byte-for-byte from Ludusavi's shared config when one exists, so every
  setting carries over. The schema is Ludusavi's exact YAML, so the file
  stays fully interchangeable.
- **Steam CDN cover art** — fetches each game's `header.jpg` from Steam's
  CDN (`shared.akamai.steamstatic.com`) in the background, with disk +
  in-memory caching and graceful fallback to extracted `.exe` icons
  (via `pelite`), then to a letter placeholder. Off by default; toggle in
  Settings.
- **Rclone cloud sync** — a faithful port of Ludusavi's rclone integration:
  same `cloud:` and `apps.rclone:` config blocks, same `rclone sync
  --use-json-log` invocation, `CREATE_NO_WINDOW` on Windows. Runs on a
  worker thread — the UI never freezes.
- **Version-specific restore** — the Confirm Restore modal lists every
  snapshot (Full and nested Differential, read from `mapping.yaml`) as a
  per-game radio list with timestamps, file counts and sizes. Pick exactly
  the point in time to restore; differential chains are replayed with
  tombstone semantics.
- **Restore report + targeted rescan** — restore runs behind a blocking
  spinner on a worker thread, then a per-game ✅/❌ report modal lists
  files, sizes and failure reasons. Closing it triggers a targeted rescan
  of *just* the restored games — status colors, local mtimes and sync
  flags refresh immediately, no full library scan. Successful backups
  rescan the same way.
- **Retention limits** — configurable Full and Differential retention,
  pruning older snapshots per Ludusavi's rules.
- **Light / Dark themes** — shared `theme:` key with Ludusavi, switchable
  in Settings; the whole palette follows.
- **Full Arabic (RTL) support** — complete English/Arabic localization via
  an editable `locales.yaml`, proper Arabic shaping through embedded Amiri
  + `arabic_reshaper`, and correct multi-word bidi ordering through
  harfbuzz-aware visual run ordering.
- **Custom paths** — per-game `customGames` entries (override/extend),
  plus Delete Local Saves with a confirmation modal.

## Building

```sh
cargo build --release
```

The executable lands in `target/release/game-save-checkpoint.exe` —
portable, no installer needed: copy it anywhere and run it, it keeps
`config.yaml` beside itself.

The icon is generated by `build.rs` from `assets/app-icon.ico`
(regenerate it from the PNG with `cargo run --example make_icon`).

## Usage

1. Run the app — on first launch it seeds `config.yaml` next to the exe
   from Ludusavi's shared config (if present), so your backup directory,
   restore source, language, theme and cloud settings apply immediately.
2. **Backup mode**: Scan → select games (all/red/yellow/green/orange
   bulk buttons or per-game checkboxes) → Start Backup. A summary modal
   reports per-game results, and the affected rows rescan immediately.
3. **Restore mode**: pick the restore source → games with backups are
   listed; expand a row in the confirmation modal to choose the exact
   snapshot → Confirm Restore. A blocking "Restoring…" overlay runs the
   engine, then the Restore Report shows ✅/❌ per game; closing it
   rescans just those games.
4. **Cloud sync**: configure a remote in `rclone config`, enter its name
   in Settings → Cloud Sync, then use **Sync to Cloud** to upload the
   backup directory.

`config.yaml` keys written by this app: `backup`, `restore`, `roots`,
`customGames`, `language`, `theme`, `cloud`, `apps.rclone` (all shared
with Ludusavi) plus app-specific `showGameIcons` and `verifiedSaves`,
which Ludusavi safely ignores.

## Project layout

```
src/
  main.rs     — egui app: header stats, toolbar, game list, details,
                modals, theme/language application
  scanner.rs  — save-file scanning, store roots, install detection
  backup.rs   — Ludusavi-compatible backup layout, mapping.yaml IO,
                full/differential restore chains, retention pruning
  cloud.rs    — rclone invocation (port of Ludusavi's src/cloud.rs)
  manifest.rs — manifest download, cache fallback, game schema
  config.rs   — portable config.yaml model (typed fields + verbatim rest)
  icons.rs    — exe icon extraction (pelite) + Steam CDN image pipeline
  cache.rs    — shared cache.yaml (manifest timestamps)
  paths.rs    — path normalization, placeholders, helpers
  roots.rs    — store-root discovery
  i18n.rs     — localization, Arabic shaping + bidi run ordering
locales.yaml  — English + Arabic strings (editable)
assets/       — app icon + embedded Amiri font
```

## Testing

```sh
cargo test
```

34 unit tests cover scanning, backup/restore chains, config round-trips,
i18n shaping (including glyph-level font resolution), icon extraction,
the rclone argument pipeline, status staleness rules, and content-hash
synchronization checks.

## Credits

- [Ludusavi](https://github.com/mtkennerly/ludusavi) — the engine and
  file formats this app interoperates with.
- [egui](https://github.com/emilk/egui) — the immediate-mode GUI.
- [Amiri](https://www.amirifont.org/) — SIL OFL Arabic typeface.
- [rclone](https://rclone.org/) — cloud sync backend.

> 📖 **[الوثائق الكاملة باللغة العربية متوفرة هنا — README_ar.md](README_ar.md)**

## License

Icon generated locally; Amiri font under the SIL Open Font License.
This project is a companion to Ludusavi — refer to upstream licenses
where applicable.
