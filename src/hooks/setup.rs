use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::config::Config;

/// Ensure the agentick hook handler script is installed, and if Claude Code
/// is present, inject hook configuration into its settings.
/// Idempotent — safe to call on every startup.
pub fn ensure_hooks_installed() {
    let _ = install_handler_script();
    // Only inject Claude hooks if the Claude CLI is actually installed.
    if crate::session::instance::Tool::Claude.is_available() {
        let _ = inject_claude_hooks();
    }
}

// ---------------------------------------------------------------------------
// Hook handler script
// ---------------------------------------------------------------------------

fn handler_script_path() -> PathBuf {
    Config::data_dir().join("bin").join("hook-handler.sh")
}

fn hooks_dir() -> PathBuf {
    Config::data_dir().join("hooks")
}

/// Install the hook handler shell script to `~/.agentick/bin/hook-handler.sh`.
fn install_handler_script() -> Result<(), Box<dyn std::error::Error>> {
    let script_path = handler_script_path();

    // Create parent directories.
    if let Some(parent) = script_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(hooks_dir())?;

    let script = r#"#!/bin/bash
# agentick hook handler — receives Claude Code hook events on stdin,
# writes status JSON to ~/.agentick/hooks/<tmux_session>.json
set -euo pipefail

INPUT=$(cat)
EVENT=$(echo "$INPUT" | jq -r '.hook_event_name // empty')
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // empty')

[ -z "$EVENT" ] && exit 0
[ -z "$SESSION_ID" ] && exit 0

# Determine status from event.
case "$EVENT" in
  SessionStart)           STATUS="active" ;;
  PreToolUse)             STATUS="active" ;;
  PostToolUse)            STATUS="active" ;;
  Stop)                   STATUS="done" ;;
  Notification)
    NTYPE=$(echo "$INPUT" | jq -r '.notification_type // empty')
    case "$NTYPE" in
      permission_prompt|elicitation_dialog) STATUS="waiting" ;;
      idle_prompt)                          STATUS="done" ;;
      *)                                   STATUS="active" ;;
    esac
    ;;
  SessionEnd)             STATUS="stopped" ;;
  *)                      exit 0 ;;
esac

# Find the tmux session name for this Claude session.
# The session_id from Claude hooks is the Claude session UUID.
# We write it keyed by session_id; the reader maps it.
HOOKS_DIR="$HOME/.agentick/hooks"
mkdir -p "$HOOKS_DIR"
STATUS_FILE="${HOOKS_DIR}/${SESSION_ID}.json"
TMP_FILE="${STATUS_FILE}.tmp"

printf '{"status":"%s","event":"%s","session_id":"%s","ts":%d}\n' \
  "$STATUS" "$EVENT" "$SESSION_ID" "$(date +%s)" > "$TMP_FILE" \
  && mv "$TMP_FILE" "$STATUS_FILE"

exit 0
"#;

    // Only write if content changed (avoid unnecessary mtime bump).
    let current = fs::read_to_string(&script_path).unwrap_or_default();
    if current != script {
        fs::write(&script_path, script)?;
    }

    // Ensure executable.
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Claude Code settings injection
// ---------------------------------------------------------------------------

const AGENTICK_MARKER: &str = "agentick-hook-handler";

/// Inject agentick hook entries into `~/.claude/settings.json`.
/// Preserves existing hooks. Idempotent via marker check.
fn inject_claude_hooks() -> Result<(), Box<dyn std::error::Error>> {
    let claude_dir = dirs::home_dir()
        .ok_or("no home dir")?
        .join(".claude");
    let settings_path = claude_dir.join("settings.json");

    // Read existing settings or start fresh.
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        fs::create_dir_all(&claude_dir)?;
        serde_json::json!({})
    };

    // Check if already installed.
    let settings_str = serde_json::to_string(&settings)?;
    if settings_str.contains(AGENTICK_MARKER) {
        return Ok(());
    }

    let handler = handler_script_path()
        .to_string_lossy()
        .to_string();

    // Ensure "hooks" object exists.
    let hooks = settings
        .as_object_mut()
        .ok_or("settings is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));

    let hooks_obj = hooks
        .as_object_mut()
        .ok_or("hooks is not an object")?;

    // Hook events to register.
    let hook_configs: Vec<(&str, bool)> = vec![
        ("SessionStart", false),
        ("PreToolUse", true),   // async — don't slow down tool use
        ("PostToolUse", true),  // async
        ("Stop", false),
        ("Notification", false),
        ("SessionEnd", false),
    ];

    for (event, is_async) in hook_configs {
        let mut hook_entry = serde_json::json!({
            "type": "command",
            "command": format!("{} # {}", handler, AGENTICK_MARKER),
        });

        if is_async {
            hook_entry["async"] = serde_json::json!(true);
        }

        let event_array = hooks_obj
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));

        if let Some(arr) = event_array.as_array_mut() {
            arr.push(serde_json::json!({
                "hooks": [hook_entry]
            }));
        }
    }

    // Write back atomically.
    let output = serde_json::to_string_pretty(&settings)?;
    let tmp = settings_path.with_extension("json.tmp");
    fs::write(&tmp, &output)?;
    fs::rename(&tmp, &settings_path)?;

    Ok(())
}
