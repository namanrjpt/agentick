use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub default_tool: Option<String>,
    pub theme: Option<String>,
    /// When set, selecting a directory in inline-new mode auto-creates a
    /// session with this tool (e.g. "claude") instead of showing the tool
    /// picker.  Set to `null` or omit to always show the picker.
    pub auto_create_tool: Option<String>,

    // ── UI ──
    /// Main tick interval in milliseconds (default: 500).
    pub refresh_rate_ms: Option<u64>,
    /// Show context window usage bars in the session list (default: true).
    pub show_token_usage: Option<bool>,
    /// Max lines in preview pane. 0 = use full pane height (default: 0).
    pub preview_lines: Option<usize>,

    // ── Tmux ──
    /// Custom tmux binary path (default: "tmux").
    pub tmux_path: Option<String>,

    // ── Status Detection ──
    /// Seconds of inactivity before done→idle (default: 60).
    pub idle_timeout_secs: Option<u64>,
    /// How long hook status files stay valid in seconds (default: 120).
    pub hook_freshness_secs: Option<u64>,

    // ── Updates ──
    /// Check GitHub for new versions on startup (default: true).
    pub check_for_updates: Option<bool>,

    // ── Quick Create Keys ──
    /// Custom key→tool mappings for quick-create mode.
    /// Keys are single characters, values are tool command names.
    /// Example: `{ c = "claude", x = "codex" }`
    /// Overrides defaults; omitted tools keep their default key.
    pub quick_create_keys: Option<HashMap<String, String>>,
}

impl Config {
    pub fn data_dir() -> PathBuf {
        let home = dirs::home_dir().expect("Could not find home directory");
        home.join(".agentick")
    }

    pub fn config_path() -> PathBuf {
        Self::data_dir().join("config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            toml::from_str(&content).unwrap_or_default()
        } else {
            // Generate a default config file so users can discover all options.
            let _ = std::fs::create_dir_all(Self::data_dir());
            let _ = std::fs::write(&path, Self::default_toml());
            Self::default()
        }
    }

    /// A well-commented default config showing all available options.
    fn default_toml() -> &'static str {
        r#"# ── General ──
# default_tool = "claude"          # default tool when creating new sessions
# theme = "dark"                   # color theme
# auto_create_tool = "claude"      # auto-create with this tool when picking a directory

# ── UI ──
# refresh_rate_ms = 500            # main tick interval in milliseconds
# show_token_usage = true          # show context window usage bars in session list
# preview_lines = 0                # max lines in preview pane (0 = full pane height)

# ── Tmux ──
# tmux_path = "tmux"               # custom tmux binary path

# ── Status Detection ──
# idle_timeout_secs = 60           # seconds of inactivity before done -> idle
# hook_freshness_secs = 120        # how long hook status files stay valid (seconds)

# ── Updates ──
# check_for_updates = true         # check GitHub for new versions on startup

# ── Quick Create Keys ──
# Key-to-tool mappings for the quick-create sheet.
# Each key is a single character, value is the tool command name.
# Only uncomment the ones you want to change; omitted tools keep defaults.
#
# [quick_create_keys]
# c = "claude"
# x = "codex"
# g = "gemini"
# r = "cursor"
# v = "vibe"
# a = "aider"
# s = "shell"
# o = "opencode"
"#
    }

    /// Load config from a specific TOML string (for testing).
    pub fn from_toml(content: &str) -> Self {
        toml::from_str(content).unwrap_or_default()
    }

    // -- Accessor helpers with defaults --

    pub fn refresh_rate_ms(&self) -> u64 {
        self.refresh_rate_ms.unwrap_or(500)
    }

    pub fn show_token_usage(&self) -> bool {
        self.show_token_usage.unwrap_or(true)
    }

    pub fn preview_lines(&self) -> usize {
        self.preview_lines.unwrap_or(0)
    }

    pub fn tmux_path(&self) -> &str {
        self.tmux_path.as_deref().unwrap_or("tmux")
    }

    pub fn idle_timeout_secs(&self) -> u64 {
        self.idle_timeout_secs.unwrap_or(60)
    }

    pub fn hook_freshness_secs(&self) -> u64 {
        self.hook_freshness_secs.unwrap_or(120)
    }

    pub fn check_for_updates(&self) -> bool {
        self.check_for_updates.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_none_fields() {
        let cfg = Config::default();
        assert!(cfg.default_tool.is_none());
        assert!(cfg.theme.is_none());
        assert!(cfg.auto_create_tool.is_none());
        assert!(cfg.refresh_rate_ms.is_none());
        assert!(cfg.show_token_usage.is_none());
        assert!(cfg.preview_lines.is_none());
        assert!(cfg.tmux_path.is_none());
        assert!(cfg.idle_timeout_secs.is_none());
        assert!(cfg.hook_freshness_secs.is_none());
        assert!(cfg.check_for_updates.is_none());
        assert!(cfg.quick_create_keys.is_none());
    }

    #[test]
    fn default_accessors_return_expected_values() {
        let cfg = Config::default();
        assert_eq!(cfg.refresh_rate_ms(), 500);
        assert!(cfg.show_token_usage());
        assert_eq!(cfg.preview_lines(), 0);
        assert_eq!(cfg.tmux_path(), "tmux");
        assert_eq!(cfg.idle_timeout_secs(), 60);
        assert_eq!(cfg.hook_freshness_secs(), 120);
        assert!(cfg.check_for_updates());
    }

    #[test]
    fn from_toml_valid() {
        let cfg = Config::from_toml(r#"
default_tool = "claude"
theme = "dark"
auto_create_tool = "gemini"
"#);
        assert_eq!(cfg.default_tool.as_deref(), Some("claude"));
        assert_eq!(cfg.theme.as_deref(), Some("dark"));
        assert_eq!(cfg.auto_create_tool.as_deref(), Some("gemini"));
    }

    #[test]
    fn from_toml_new_fields() {
        let cfg = Config::from_toml(r#"
refresh_rate_ms = 250
show_token_usage = false
preview_lines = 30
tmux_path = "/usr/local/bin/tmux"
idle_timeout_secs = 90
hook_freshness_secs = 60
check_for_updates = false
"#);
        assert_eq!(cfg.refresh_rate_ms(), 250);
        assert!(!cfg.show_token_usage());
        assert_eq!(cfg.preview_lines(), 30);
        assert_eq!(cfg.tmux_path(), "/usr/local/bin/tmux");
        assert_eq!(cfg.idle_timeout_secs(), 90);
        assert_eq!(cfg.hook_freshness_secs(), 60);
        assert!(!cfg.check_for_updates());
    }

    #[test]
    fn from_toml_partial() {
        let cfg = Config::from_toml(r#"default_tool = "codex""#);
        assert_eq!(cfg.default_tool.as_deref(), Some("codex"));
        assert!(cfg.theme.is_none());
        assert!(cfg.auto_create_tool.is_none());
        // New fields should fall back to defaults.
        assert_eq!(cfg.refresh_rate_ms(), 500);
        assert!(cfg.show_token_usage());
    }

    #[test]
    fn from_toml_empty() {
        let cfg = Config::from_toml("");
        assert!(cfg.default_tool.is_none());
    }

    #[test]
    fn from_toml_malformed_returns_default() {
        let cfg = Config::from_toml("this is not valid toml [[[");
        assert!(cfg.default_tool.is_none());
    }

    #[test]
    fn data_dir_ends_with_agentick() {
        let dir = Config::data_dir();
        assert!(dir.ends_with(".agentick"));
    }

    #[test]
    fn config_path_is_toml() {
        let path = Config::config_path();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("toml"));
    }

    #[test]
    fn from_toml_quick_create_keys() {
        let cfg = Config::from_toml(r#"
[quick_create_keys]
m = "claude"
z = "codex"
"#);
        let keys = cfg.quick_create_keys.unwrap();
        assert_eq!(keys.get("m").map(|s| s.as_str()), Some("claude"));
        assert_eq!(keys.get("z").map(|s| s.as_str()), Some("codex"));
    }

    #[test]
    fn from_toml_quick_create_keys_empty_table() {
        let cfg = Config::from_toml(r#"
[quick_create_keys]
"#);
        assert!(cfg.quick_create_keys.unwrap().is_empty());
    }
}
