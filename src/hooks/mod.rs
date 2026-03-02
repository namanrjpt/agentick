pub mod setup;

use std::collections::HashMap;
use std::fs;
use std::time::SystemTime;

use crate::config::Config;
use crate::tmux::detector::HookStatus;

/// Expected JSON shape of a hook status file.
#[derive(serde::Deserialize)]
struct HookFile {
    status: String,
}

/// Maximum age (in seconds) for a hook file to be considered fresh.
const HOOK_FRESHNESS_SECS: u64 = 120;

/// Read all fresh hook status files from `~/.agentick/hooks/`.
///
/// Returns a map of `filename_stem -> HookStatus`.
/// Stale files (older than 2 minutes) are ignored.
pub fn read_hook_statuses() -> HashMap<String, HookStatus> {
    let hooks_dir = Config::data_dir().join("hooks");
    let mut map = HashMap::new();

    let entries = match fs::read_dir(&hooks_dir) {
        Ok(e) => e,
        Err(_) => return map,
    };

    let now = SystemTime::now();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        // Check file freshness by modification time.
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let age = now.duration_since(mtime).unwrap_or_default();
        if age.as_secs() > HOOK_FRESHNESS_SECS {
            continue;
        }

        // Parse the file.
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let hook_file: HookFile = match serde_json::from_str(&content) {
            Ok(h) => h,
            Err(_) => continue,
        };

        let status = match hook_file.status.as_str() {
            "active" | "working" => HookStatus::Active,
            "waiting" | "waiting_for_permission" => HookStatus::Waiting,
            "done" | "idle" | "stopped" => HookStatus::Done,
            _ => continue,
        };

        // Use filename stem as the key (e.g. session name or id).
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            map.insert(stem.to_string(), status);
        }
    }

    map
}
