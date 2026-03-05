use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub default_tool: Option<String>,
    pub theme: Option<String>,
    /// When set, selecting a directory in inline-new mode auto-creates a
    /// session with this tool (e.g. "claude") instead of showing the tool
    /// picker.  Set to `null` or omit to always show the picker.
    pub auto_create_tool: Option<String>,
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
            Self::default()
        }
    }

    /// Load config from a specific TOML string (for testing).
    pub fn from_toml(content: &str) -> Self {
        toml::from_str(content).unwrap_or_default()
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
    fn from_toml_partial() {
        let cfg = Config::from_toml(r#"default_tool = "codex""#);
        assert_eq!(cfg.default_tool.as_deref(), Some("codex"));
        assert!(cfg.theme.is_none());
        assert!(cfg.auto_create_tool.is_none());
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
}
