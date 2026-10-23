//! Cloud sync via Rclone — a port of Ludusavi's `src/cloud.rs`.
//!
//! The invocation mirrors Ludusavi exactly: the configured global
//! `apps.rclone.arguments` are shlex-split and prepended, then
//!
//! ```text
//! rclone sync -v --use-json-log --stats=100ms --progress=false \
//!     <local backup dir> <remote id>:<cloud.path>
//! ```
//!
//! On Windows the process is spawned with `CREATE_NO_WINDOW` so no
//! console flashes. Exit code 0 is success; stderr's JSON log lines
//! (`{"level":"error","msg":"…"}`) are surfaced as the error message.

use std::path::Path;

use crate::config::Config;
use crate::paths::normalize;

/// `CREATE_NO_WINDOW` — avoids a console window flash on Windows.
/// (Raw constant; no `windows` crate needed.)
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Equivalent of Ludusavi's `Rclone { app, remote }`.
pub struct Rclone {
    /// `apps.rclone.path` (falls back to bare `rclone` for PATH lookup).
    exe: String,
    /// `apps.rclone.arguments` — global flags for every invocation.
    arguments: String,
    /// The remote's `id` (from `cloud.remote`).
    remote_id: String,
}

impl Rclone {
    /// Build from config. Fails when no remote is configured —
    /// Ludusavi's `Error::CloudNotConfigured` equivalent.
    pub fn from_config(config: &Config) -> Result<Self, String> {
        let remote_id = config
            .cloud_remote_id()
            .ok_or_else(|| "cloud remote not configured".to_string())?;
        let exe = if config.apps.rclone.path.trim().is_empty() {
            "rclone".to_string()
        } else {
            config.apps.rclone.path.trim().to_string()
        };
        Ok(Self {
            exe,
            arguments: config.apps.rclone.arguments.clone(),
            remote_id,
        })
    }

    /// `remote:path` — rclone disallows backslashes in remote paths.
    /// https://rclone.org/docs/#syntax-of-remote-paths
    fn remote_path(&self, path: &str) -> String {
        format!("{}:{}", self.remote_id, path.replace('\\', "/"))
    }

    /// Global arguments first (shlex-split), then the command args —
    /// same order as Ludusavi's `Rclone::args`.
    fn full_args(&self, args: &[String]) -> Vec<String> {
        let mut collected = vec![];
        if !self.arguments.trim().is_empty()
            && let Some(parts) = shlex::split(&self.arguments)
        {
            collected.extend(parts);
        }
        collected.extend(args.iter().cloned());
        collected
    }

    /// `rclone sync` upload: local backup dir → `<remote>:<cloud.path>`.
    /// Blocking — call from a worker thread. Mirrors Ludusavi's
    /// `Rclone::sync` with `SyncDirection::Upload`, `Finality::Final`,
    /// and no per-game include filters (the whole backup dir syncs).
    pub fn sync_upload(&self, local: &Path, cloud_path: &str) -> Result<(), String> {
        if !local.exists() {
            // Port of Ludusavi's guard: rclone exits 3 when the local
            // folder doesn't exist, so create it first.
            let _ = std::fs::create_dir_all(local);
        }
        let sync_args = vec![
            "sync".to_string(),
            "-v".to_string(),
            "--use-json-log".to_string(),
            "--stats=100ms".to_string(),
            // Terminal progress output is incompatible with the JSON
            // log; a flag also overrides RCLONE_PROGRESS.
            "--progress=false".to_string(),
            normalize(&local.to_string_lossy()),
            self.remote_path(cloud_path),
        ];
        let args = self.full_args(&sync_args);

        let mut cmd = std::process::Command::new(&self.exe);
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd
            .output()
            .map_err(|e| format!("cannot launch rclone: {e}"))?;

        if output.status.success() {
            return Ok(());
        }
        Err(extract_error(&output.stderr, output.status.code()))
    }
}

/// Rclone logs to stderr as JSON lines (`{"level":"error","msg":"…"}`)
/// under `--use-json-log`. Pull the `msg` fields; fall back to raw
/// lines for anything that isn't JSON.
fn extract_error(stderr: &[u8], code: Option<i32>) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut msgs: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            if !line.starts_with('{') {
                return Some(line.to_string());
            }
            // Crude `"msg":"…"` extraction — enough for error surfacing
            // without pulling in a JSON parser.
            let key = "\"msg\":\"";
            let start = line.find(key)? + key.len();
            let end = line[start..].find('"')? + start;
            Some(line[start..end].to_string())
        })
        .collect();
    msgs.dedup();
    match msgs.last() {
        Some(m) => m.clone(),
        None => format!("rclone exited with code {}", code.unwrap_or(-1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(remote_yaml: &str, args: &str) -> Config {
        let mut c: Config = serde_yaml::from_str(remote_yaml).unwrap();
        c.apps.rclone.arguments = args.to_string();
        c
    }

    #[test]
    fn sync_args_match_ludusavi() {
        let c = cfg(
            "---\ncloud:\n  remote: {Custom: {id: myremote}}\n  path: ludusavi-backup\n",
            "--fast-list --ignore-checksum",
        );
        let r = Rclone::from_config(&c).unwrap();
        // Global args first, then the sync invocation.
        let args = r.full_args(&[
            "sync".to_string(),
            "-v".to_string(),
            "--use-json-log".to_string(),
            "--stats=100ms".to_string(),
            "--progress=false".to_string(),
            "C:/backups".to_string(),
            r.remote_path(&c.cloud.path),
        ]);
        assert_eq!(
            args,
            vec![
                "--fast-list",
                "--ignore-checksum",
                "sync",
                "-v",
                "--use-json-log",
                "--stats=100ms",
                "--progress=false",
                "C:/backups",
                "myremote:ludusavi-backup",
            ]
        );
    }

    #[test]
    fn remote_path_switches_backslashes() {
        let c = cfg("---\ncloud:\n  remote: {Custom: {id: r}}\n", "");
        let r = Rclone::from_config(&c).unwrap();
        assert_eq!(r.remote_path("a\\b/c"), "r:a/b/c");
    }

    #[test]
    fn from_config_requires_a_remote() {
        let c = cfg("---\ncloud:\n  remote: ~\n", "");
        assert!(Rclone::from_config(&c).is_err());
    }

    #[test]
    fn extract_error_reads_json_msgs() {
        let stderr = b"{\"level\":\"warning\",\"msg\":\"skipped\"}\n{\"level\":\"error\",\"msg\":\"dir not found\"}\n";
        assert_eq!(extract_error(stderr, Some(1)), "dir not found");
        // Non-JSON lines pass through; empty stderr falls back to code.
        assert_eq!(extract_error(b"plain failure\n", Some(3)), "plain failure");
        assert_eq!(extract_error(b"", Some(3)), "rclone exited with code 3");
    }
}
