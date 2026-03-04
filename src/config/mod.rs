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
}
