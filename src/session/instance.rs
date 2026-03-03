use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Claude,
    Gemini,
    Codex,
    OpenCode,
    Cursor,
    Aider,
    Vibe,
    Shell,
    Custom(String),
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tool::Claude => write!(f, "claude"),
            Tool::Gemini => write!(f, "gemini"),
            Tool::Codex => write!(f, "codex"),
            Tool::OpenCode => write!(f, "opencode"),
            Tool::Cursor => write!(f, "cursor"),
            Tool::Aider => write!(f, "aider"),
            Tool::Vibe => write!(f, "vibe"),
            Tool::Shell => write!(f, "shell"),
            Tool::Custom(name) => write!(f, "{}", name.to_lowercase()),
        }
    }
}

impl Tool {
    /// Map a CLI command string to the corresponding `Tool` variant.
    pub fn from_command(cmd: &str) -> Tool {
        match cmd.to_lowercase().as_str() {
            "claude" => Tool::Claude,
            "gemini" => Tool::Gemini,
            "codex" => Tool::Codex,
            "opencode" => Tool::OpenCode,
            "cursor" | "cursor-agent" => Tool::Cursor,
            "aider" => Tool::Aider,
            "vibe" => Tool::Vibe,
            "bash" | "sh" | "zsh" | "fish" => Tool::Shell,
            other => Tool::Custom(other.to_string()),
        }
    }

    /// A single unicode glyph used as a visual indicator in the TUI.
    pub fn icon(&self) -> &str {
        match self {
            Tool::Claude => "\u{25A0}",   // ■
            Tool::Gemini => "\u{25C6}",   // ◆
            Tool::Codex => "\u{25C7}",    // ◇
            Tool::OpenCode => "\u{2588}", // █
            Tool::Cursor => "\u{25B2}",   // ▲
            Tool::Aider => "\u{2318}",    // ⌘
            Tool::Vibe => "\u{25C8}",     // ◈
            Tool::Shell => "\u{25CB}",    // ○
            Tool::Custom(_) => "\u{25CF}", // ●
        }
    }

    /// The default CLI command used to launch this tool.
    pub fn default_command(&self) -> &str {
        match self {
            Tool::Claude => "claude --dangerously-skip-permissions",
            Tool::Gemini => "gemini",
            Tool::Codex => "codex",
            Tool::OpenCode => "opencode",
            Tool::Cursor => "cursor-agent",
            Tool::Aider => "aider",
            Tool::Vibe => "vibe",
            Tool::Shell => "bash",
            Tool::Custom(name) => name.as_str(),
        }
    }

    /// Sensible default context window size per tool.
    fn default_context_limit(&self) -> u64 {
        match self {
            Tool::Claude => 200_000,
            Tool::Gemini => 1_000_000,
            Tool::Codex => 200_000,
            Tool::OpenCode => 200_000,
            Tool::Cursor => 200_000,
            Tool::Aider => 200_000,
            Tool::Vibe => 128_000,
            Tool::Shell => 0,
            Tool::Custom(_) => 200_000,
        }
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Active,
    Waiting,
    Done,
    Idle,
    Dead,
}

impl Default for Status {
    fn default() -> Self {
        Status::Idle
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Active => write!(f, "active"),
            Status::Waiting => write!(f, "waiting"),
            Status::Done => write!(f, "done"),
            Status::Idle => write!(f, "idle"),
            Status::Dead => write!(f, "dead"),
        }
    }
}

impl Status {
    /// A single unicode glyph used as a status indicator in the TUI.
    pub fn indicator(&self) -> &str {
        match self {
            Status::Active => "\u{25CF}",  // ● (flashing in renderer)
            Status::Waiting => "\u{25C9}", // ◉ (ring with dot)
            Status::Done => "\u{25CF}",    // ● (solid)
            Status::Idle => "\u{25CB}",    // ○
            Status::Dead => "\u{2715}",    // ✕
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub project_path: PathBuf,
    pub command: String,
    pub tool: Tool,

    #[serde(skip)]
    #[serde(default)]
    pub status: Status,

    pub tmux_name: String,
    pub created_at: DateTime<Utc>,
    pub context_used: Option<u64>,

    #[serde(default = "default_context_limit")]
    pub context_limit: u64,

    pub model: Option<String>,
    pub cost_usd: Option<f64>,

    pub last_activity: Option<i64>,

    /// If this session was forked from another, the parent agentick session ID.
    #[serde(default)]
    pub forked_from: Option<String>,

    /// Whether the user manually renamed this session. When true, auto-title
    /// (first-message placeholder and LLM summary) is skipped.
    #[serde(default)]
    pub user_renamed: bool,
}

fn default_context_limit() -> u64 {
    200_000
}

impl Session {
    /// Create a new session with sensible defaults.
    pub fn new(
        title: String,
        project_path: PathBuf,
        tool: Tool,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let command = tool.default_command().to_string();
        let tmux_name = crate::tmux::client::sanitize_session_name(&title, &id);
        let context_limit = tool.default_context_limit();

        Self {
            id,
            title,
            project_path,
            command,
            tool,
            status: Status::Dead,
            tmux_name,
            created_at: Utc::now(),
            context_used: None,
            context_limit,
            model: None,
            cost_usd: None,
            last_activity: None,
            forked_from: None,
            user_renamed: false,
        }
    }

    /// Create a new session that is a fork of `parent`.
    ///
    /// The caller is responsible for overriding `command` with the appropriate
    /// `--resume <uuid> --fork-session` invocation before spawning tmux.
    pub fn new_fork(parent: &Session) -> Self {
        let mut s = Session::new(
            format!("{} (fork)", parent.title),
            parent.project_path.clone(),
            parent.tool.clone(),
        );
        s.forked_from = Some(parent.id.clone());
        // Inherit parent's context data so the bar shows immediately.
        s.context_used = parent.context_used;
        s.context_limit = parent.context_limit;
        s.model = parent.model.clone();
        s
    }

    /// Context usage as a percentage (0.0 – 100.0), if `context_used` is known.
    pub fn context_percentage(&self) -> Option<f64> {
        let used = self.context_used?;
        if self.context_limit == 0 {
            return None;
        }
        Some((used as f64 / self.context_limit as f64) * 100.0)
    }

    /// Replace the user's home directory prefix with `~` for display.
    pub fn short_path(&self) -> String {
        if let Some(home) = dirs::home_dir() {
            if let Ok(suffix) = self.project_path.strip_prefix(&home) {
                return format!("~/{}", suffix.display());
            }
        }
        self.project_path.display().to_string()
    }

    /// Human-readable relative time string such as "30s ago" or "3d ago".
    pub fn age_display(&self) -> String {
        let reference = self
            .last_activity
            .and_then(|ts| DateTime::from_timestamp(ts, 0))
            .unwrap_or(self.created_at);

        let elapsed = Utc::now().signed_duration_since(reference);
        let secs = elapsed.num_seconds().max(0);

        if secs < 60 {
            format!("{}s ago", secs)
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else if secs < 86400 {
            format!("{}h ago", secs / 3600)
        } else {
            format!("{}d ago", secs / 86400)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn tool_from_command_maps_correctly() {
        assert_eq!(Tool::from_command("claude"), Tool::Claude);
        assert_eq!(Tool::from_command("Claude"), Tool::Claude);
        assert_eq!(Tool::from_command("gemini"), Tool::Gemini);
        assert_eq!(Tool::from_command("codex"), Tool::Codex);
        assert_eq!(Tool::from_command("opencode"), Tool::OpenCode);
        assert_eq!(Tool::from_command("cursor"), Tool::Cursor);
        assert_eq!(Tool::from_command("cursor-agent"), Tool::Cursor);
        assert_eq!(Tool::from_command("aider"), Tool::Aider);
        assert_eq!(Tool::from_command("vibe"), Tool::Vibe);
        assert_eq!(Tool::from_command("bash"), Tool::Shell);
        assert_eq!(
            Tool::from_command("sometool"),
            Tool::Custom("sometool".to_string())
        );
    }

    #[test]
    fn tool_display_is_lowercase() {
        assert_eq!(Tool::Claude.to_string(), "claude");
        assert_eq!(Tool::Custom("MyTool".into()).to_string(), "mytool");
    }

    #[test]
    fn status_indicator_returns_expected_glyphs() {
        assert_eq!(Status::Active.indicator(), "\u{25CF}");
        assert_eq!(Status::Waiting.indicator(), "\u{25C9}");
        assert_eq!(Status::Done.indicator(), "\u{25CF}");
        assert_eq!(Status::Idle.indicator(), "\u{25CB}");
        assert_eq!(Status::Dead.indicator(), "\u{2715}");
    }

    #[test]
    fn session_new_has_correct_defaults() {
        let s = Session::new(
            "test".into(),
            PathBuf::from("/tmp/project"),
            Tool::Claude,
        );
        assert_eq!(s.status, Status::Dead);
        assert_eq!(s.context_limit, 200_000);
        assert!(s.tmux_name.starts_with("agentick_"));
    }

    #[test]
    fn context_percentage_calculation() {
        let mut s = Session::new(
            "test".into(),
            PathBuf::from("/tmp"),
            Tool::Claude,
        );
        assert!(s.context_percentage().is_none());

        s.context_used = Some(100_000);
        let pct = s.context_percentage().unwrap();
        assert!((pct - 50.0).abs() < 0.01);
    }

    #[test]
    fn tmux_name_uses_sanitize() {
        let s = Session::new(
            "My Cool Project".into(),
            PathBuf::from("/tmp"),
            Tool::Claude,
        );
        // Should use the tmux::client::sanitize_session_name format.
        assert!(s.tmux_name.starts_with("agentick_"));
        assert!(s.tmux_name.contains("my-cool-project"));
    }
}
