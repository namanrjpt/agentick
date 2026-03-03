use ratatui::style::Color;

/// Tokyo Night dark color theme for the TUI.
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub border: Color,
    pub border_focused: Color,
    pub text: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub green: Color,
    pub yellow: Color,
    pub orange: Color,
    pub red: Color,
    pub purple: Color,
    pub cyan: Color,
}

/// Returns the default Tokyo Night dark theme.
pub fn dark_theme() -> Theme {
    Theme {
        bg: Color::Rgb(0x00, 0x00, 0x00),
        surface: Color::Rgb(0x28, 0x34, 0x57),
        border: Color::Rgb(0x3b, 0x42, 0x61),
        border_focused: Color::Rgb(0x7a, 0xa2, 0xf7),
        text: Color::Rgb(0xc0, 0xca, 0xf5),
        text_dim: Color::Rgb(0x56, 0x5f, 0x89),
        accent: Color::Rgb(0x7a, 0xa2, 0xf7),
        green: Color::Rgb(0x9e, 0xce, 0x6a),
        yellow: Color::Rgb(0xe0, 0xaf, 0x68),
        orange: Color::Rgb(0xff, 0x9e, 0x64),
        red: Color::Rgb(0xf7, 0x76, 0x8e),
        purple: Color::Rgb(0xbb, 0x9a, 0xf7),
        cyan: Color::Rgb(0x7d, 0xcf, 0xff),
    }
}

/// Map a session status string to a themed color.
///
/// - `"active"` → green
/// - `"done"` → green
/// - `"idle"` → text_dim
/// - `"dead"` → red
/// - anything else → text_dim
pub fn status_color(status: &str) -> Color {
    let t = dark_theme();
    match status {
        "active" => t.green,
        "waiting" => t.yellow,
        "done" => t.green,
        "idle" => t.text_dim,
        "dead" => t.red,
        _ => t.text_dim,
    }
}

/// Map an AI tool name to a themed color.
///
/// - `"claude"` → orange
/// - `"gemini"` → purple
/// - `"codex"` → cyan
/// - `"opencode"` → accent
/// - `"cursor"` → green
/// - `"aider"` → yellow
/// - `"shell"` → text_dim
/// - anything else → text
pub fn tool_color(tool: &str) -> Color {
    let t = dark_theme();
    match tool {
        "claude" => t.orange,
        "gemini" => t.purple,
        "codex" => t.cyan,
        "opencode" => t.accent,
        "cursor" => t.green,
        "aider" => t.yellow,
        "vibe" => t.red,
        "shell" => t.text_dim,
        _ => t.text,
    }
}
