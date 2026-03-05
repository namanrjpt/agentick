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

/// Internal: parse a hook status string to HookStatus (for testing).
fn parse_hook_status(s: &str) -> Option<HookStatus> {
    match s {
        "active" | "working" => Some(HookStatus::Active),
        "waiting" | "waiting_for_permission" => Some(HookStatus::Waiting),
        "done" | "idle" | "stopped" => Some(HookStatus::Done),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_hook_status_maps_active() {
        assert_eq!(parse_hook_status("active"), Some(HookStatus::Active));
        assert_eq!(parse_hook_status("working"), Some(HookStatus::Active));
    }

    #[test]
    fn parse_hook_status_maps_waiting() {
        assert_eq!(parse_hook_status("waiting"), Some(HookStatus::Waiting));
        assert_eq!(parse_hook_status("waiting_for_permission"), Some(HookStatus::Waiting));
    }

    #[test]
    fn parse_hook_status_maps_done() {
        assert_eq!(parse_hook_status("done"), Some(HookStatus::Done));
        assert_eq!(parse_hook_status("idle"), Some(HookStatus::Done));
        assert_eq!(parse_hook_status("stopped"), Some(HookStatus::Done));
    }

    #[test]
    fn parse_hook_status_unknown_returns_none() {
        assert_eq!(parse_hook_status("exploding"), None);
        assert_eq!(parse_hook_status(""), None);
    }

    #[test]
    fn hook_file_deserializes_valid_json() {
        let json = r#"{"status":"active","event":"PreToolUse","session_id":"abc","ts":1234}"#;
        let hf: HookFile = serde_json::from_str(json).unwrap();
        assert_eq!(hf.status, "active");
    }

    #[test]
    fn hook_file_ignores_extra_fields() {
        let json = r#"{"status":"waiting","unknown_field":42}"#;
        let hf: HookFile = serde_json::from_str(json).unwrap();
        assert_eq!(hf.status, "waiting");
    }

    #[test]
    fn hook_file_rejects_missing_status() {
        let json = r#"{"event":"Stop"}"#;
        assert!(serde_json::from_str::<HookFile>(json).is_err());
    }

    #[test]
    fn read_hooks_from_fresh_dir() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join("hooks");
        fs::create_dir_all(&hooks_dir).unwrap();

        // Write a fresh hook file.
        let hook_path = hooks_dir.join("session123.json");
        fs::write(&hook_path, r#"{"status":"active"}"#).unwrap();

        // Write a non-json file — should be ignored.
        fs::write(hooks_dir.join("readme.txt"), "ignore me").unwrap();

        // We can't call read_hook_statuses() directly because it hardcodes
        // ~/.agentick/hooks, but we can verify the JSON parsing logic.
        let content = fs::read_to_string(&hook_path).unwrap();
        let hf: HookFile = serde_json::from_str(&content).unwrap();
        let status = parse_hook_status(&hf.status);
        assert_eq!(status, Some(HookStatus::Active));
    }

    #[test]
    fn stale_hook_freshness_constant() {
        // Verify the freshness window is 2 minutes.
        assert_eq!(HOOK_FRESHNESS_SECS, 120);
    }
}
