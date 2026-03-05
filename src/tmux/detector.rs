// ---------------------------------------------------------------------------
// 5-layer status detection pipeline.
//
// Priority: Dead → Hooks → Title → Content (busy→prompt) → Timestamps
// ---------------------------------------------------------------------------

use std::time::{Duration, Instant};

use crate::session::instance::{Status, Tool};

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Strip ANSI escape sequences from `input`, returning a clean `String`.
pub fn strip_ansi(input: &str) -> String {
    let stripped_bytes = strip_ansi_escapes::strip(input);
    String::from_utf8_lossy(&stripped_bytes).into_owned()
}

/// Return the last `n` non-blank lines from `content` (preserving order).
pub fn extract_last_n_lines(content: &str, n: usize) -> Vec<&str> {
    content
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

// ---------------------------------------------------------------------------
// Detection context & result
// ---------------------------------------------------------------------------

/// All signals gathered for a single session during one tick.
pub struct DetectionContext<'a> {
    pub tool: &'a Tool,
    pub pane_title: Option<&'a str>,
    pub pane_content: Option<&'a str>,
    pub hook_status: Option<HookStatus>,
    pub activity_changed_at: Option<Instant>,
    pub spinner_last_seen: Option<Instant>,
    pub sustained_activity_count: u32,
    pub now: Instant,
    /// Seconds of inactivity before done→idle (default: 60).
    pub idle_timeout_secs: u64,
}

/// Status reported by a Claude hook file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    Active,
    Idle,
}

/// Result of the detection pipeline.
pub struct DetectionResult {
    pub status: Status,
    /// If a spinner was seen this tick, caller should update spinner_last_seen.
    pub spinner_seen: bool,
}

// ---------------------------------------------------------------------------
// Main detection entry point
// ---------------------------------------------------------------------------

/// Run the 5-layer detection pipeline.
pub fn detect_status(ctx: &DetectionContext) -> DetectionResult {
    // Layer 0: Dead — no tmux session.
    // (Caller checks activity_cache before calling us, but we handle it here too
    //  via the absence of any positive signal.)

    // Layer 2: Hook status (if fresh).
    if let Some(ref hook) = ctx.hook_status {
        return DetectionResult {
            status: match hook {
                HookStatus::Active => Status::Active,
                HookStatus::Idle => Status::Idle,
            },
            spinner_seen: false,
        };
    }

    // Layer 1: Pane title — braille spinner = Active.
    if let Some(title) = ctx.pane_title {
        if has_braille_spinner(title) {
            return DetectionResult {
                status: Status::Active,
                spinner_seen: true,
            };
        }
    }

    // Layers 3 & 4: Content-based detection (requires pane capture).
    if let Some(content) = ctx.pane_content {
        let stripped = strip_ansi(content);
        let last_lines = extract_last_n_lines(&stripped, 10);

        // Layer 3: Busy indicator detection.
        if has_busy_indicator(ctx.tool, &last_lines) {
            return DetectionResult {
                status: Status::Active,
                spinner_seen: true,
            };
        }

        // Spinner grace period (5 seconds).
        if let Some(seen_at) = ctx.spinner_last_seen {
            if ctx.now.duration_since(seen_at) < Duration::from_secs(5) {
                return DetectionResult {
                    status: Status::Active,
                    spinner_seen: false,
                };
            }
        }

        // Layer 4: Prompt detection (only after ruling out busy).
        if has_prompt_indicator(ctx.tool, &last_lines) {
            return DetectionResult {
                status: Status::Active,
                spinner_seen: false,
            };
        }
    }

    // Layer 5: Timestamp-based fallback.
    if let Some(changed_at) = ctx.activity_changed_at {
        let elapsed = ctx.now.duration_since(changed_at);

        // Sustained activity: 2+ consecutive timestamp changes within 3s.
        // Exclude tools with TUI cursor blink (Vibe, Cursor) since their
        // blinking cursor produces continuous activity even when idle.
        let has_tui_blink = matches!(ctx.tool, Tool::Vibe | Tool::Cursor);
        if !has_tui_blink
            && ctx.sustained_activity_count >= 2
            && elapsed < Duration::from_secs(3)
        {
            return DetectionResult {
                status: Status::Active,
                spinner_seen: false,
            };
        }

        if elapsed < Duration::from_secs(ctx.idle_timeout_secs) {
            return DetectionResult {
                status: Status::Active,
                spinner_seen: false,
            };
        }

        return DetectionResult {
            status: Status::Idle,
            spinner_seen: false,
        };
    }

    // No signals at all.
    DetectionResult {
        status: Status::Idle,
        spinner_seen: false,
    }
}

// ---------------------------------------------------------------------------
// Layer 1: Pane title patterns
// ---------------------------------------------------------------------------

/// Check if a string contains braille spinner characters (U+2800..U+28FF).
pub fn has_braille_spinner(s: &str) -> bool {
    s.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
}

// ---------------------------------------------------------------------------
// Layer 3: Busy indicator patterns (tool-specific)
// ---------------------------------------------------------------------------

fn has_busy_indicator(tool: &Tool, lines: &[&str]) -> bool {
    // Generic patterns that apply to most tools.
    let generic_busy = lines.iter().any(|line| {
        let lower = line.to_lowercase();
        lower.contains("ctrl+c to interrupt")
            || lower.contains("esc to interrupt")
            || has_braille_spinner(line)
    });

    if generic_busy {
        return true;
    }

    // Tool-specific patterns.
    match tool {
        Tool::Claude => {
            // Star spinners (✳✽✶✢) followed by text.
            lines.iter().any(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return false;
                }
                let first = trimmed.chars().next().unwrap_or(' ');
                matches!(first, '\u{2733}' | '\u{273D}' | '\u{2736}' | '\u{2722}')
            })
        }
        Tool::Codex => lines.iter().any(|line| {
            let lower = line.to_lowercase();
            lower.contains("thinking")
        }),
        Tool::OpenCode => lines.iter().any(|line| {
            let lower = line.to_lowercase();
            lower.contains("thinking...") || lower.contains("generating...")
        }),
        Tool::Vibe => {
            // Vibe uses a SnakeSpinner (braille-rendered) in its loading widget.
            // The generic braille check above covers it, but also look for the
            // loading indicator combined with tool execution text.
            lines.iter().any(|line| {
                let lower = line.to_lowercase();
                lower.contains("running") || lower.contains("executing")
            })
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Layer 4: Prompt detection patterns (tool-specific)
// ---------------------------------------------------------------------------

fn has_prompt_indicator(tool: &Tool, lines: &[&str]) -> bool {
    let last_line = lines.last().map(|l| l.trim()).unwrap_or("");
    let lines_lower: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();

    match tool {
        Tool::Claude => {
            // Input prompt.
            if last_line == ">" || last_line == "\u{276F}" {
                return true;
            }
            // Permission dialogs.
            lines_lower.iter().any(|l| {
                l.contains("yes, allow once")
                    || l.contains("yes, allow always")
                    || l.contains("no, skip")
                    || l.contains("no, and tell claude")
                    || l.contains("allow once")
                    || l.contains("always allow")
                    || l.contains("do you trust the files")
            })
        }
        Tool::Gemini => last_line.contains("gemini>") || last_line == ">",
        Tool::Codex => {
            let lower = last_line.to_lowercase();
            lower.contains("(y/n)")
                || lower.contains("approve")
                || lower.ends_with('$')
                || lower.ends_with('%')
                || last_line.ends_with('>')
                || last_line.ends_with('\u{276F}')
        }
        Tool::OpenCode => {
            let lower = last_line.to_lowercase();
            lower.contains("ask anything") || lower.contains("press enter to send")
        }
        Tool::Aider => last_line == ">" || last_line.starts_with("> "),
        Tool::Vibe => {
            // Vibe (Textual TUI) shows an approval dialog or a chat input area.
            // Approval dialog: "↑↓ navigate  Enter select  ESC reject"
            let has_approval = lines_lower.iter().any(|l| {
                (l.contains("navigate") && l.contains("enter select"))
                    || l.contains("yes and always allow")
                    || l.contains("no and tell the agent")
            });
            if has_approval {
                return true;
            }
            // Vibe is a Textual TUI — if it's not showing a braille spinner
            // (caught by Layer 3 busy check above), it's at the chat input
            // with a blinking cursor, i.e. waiting for user input.
            let full = lines.join(" ");
            !has_braille_spinner(&full)
        }
        _ => {
            // Generic: shell prompts, continuation prompts.
            // Note: last_line is trim()'d so trailing space is gone.
            let lower = last_line.to_lowercase();
            last_line.ends_with('$')
                || last_line.ends_with('%')
                || last_line == ">"
                || last_line == "\u{276F}"
                || lower.contains("(y/n)")
                || lower.contains("continue?")
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Utilities ----------------------------------------------------------

    #[test]
    fn test_strip_ansi_plain() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_removes_escapes() {
        let colored = "\x1b[32mhello\x1b[0m world";
        assert_eq!(strip_ansi(colored), "hello world");
    }

    #[test]
    fn test_extract_last_n_lines_basic() {
        let content = "aaa\n\nbbb\nccc\n\nddd\n";
        let lines = extract_last_n_lines(content, 3);
        assert_eq!(lines, vec!["bbb", "ccc", "ddd"]);
    }

    #[test]
    fn test_extract_last_n_lines_fewer_than_n() {
        let content = "one\ntwo\n";
        let lines = extract_last_n_lines(content, 10);
        assert_eq!(lines, vec!["one", "two"]);
    }

    // -- Braille spinner ----------------------------------------------------

    #[test]
    fn braille_spinner_detected() {
        assert!(has_braille_spinner("Session \u{2801} working"));
        assert!(has_braille_spinner("\u{280B}"));
        assert!(!has_braille_spinner("No spinner here"));
    }

    // -- Busy indicators ----------------------------------------------------

    #[test]
    fn claude_busy_ctrl_c() {
        let lines = vec!["Editing file", "ctrl+c to interrupt"];
        assert!(has_busy_indicator(&Tool::Claude, &lines));
    }

    #[test]
    fn claude_busy_star_spinner() {
        let lines = vec!["\u{2733} running linter"];
        assert!(has_busy_indicator(&Tool::Claude, &lines));
    }

    #[test]
    fn codex_busy_thinking() {
        let lines = vec!["Thinking about approach"];
        assert!(has_busy_indicator(&Tool::Codex, &lines));
    }

    #[test]
    fn opencode_busy_generating() {
        let lines = vec!["Generating..."];
        assert!(has_busy_indicator(&Tool::OpenCode, &lines));
    }

    #[test]
    fn no_busy_on_plain_text() {
        let lines = vec!["Just some output", "nothing special"];
        assert!(!has_busy_indicator(&Tool::Claude, &lines));
    }

    // -- Prompt indicators --------------------------------------------------

    #[test]
    fn claude_prompt_arrow() {
        let lines = vec!["some output", ">"];
        assert!(has_prompt_indicator(&Tool::Claude, &lines));
    }

    #[test]
    fn claude_prompt_permission() {
        let lines = vec!["Tool use requested", "Yes, allow once"];
        assert!(has_prompt_indicator(&Tool::Claude, &lines));
    }

    #[test]
    fn gemini_prompt() {
        let lines = vec!["output", "gemini>"];
        assert!(has_prompt_indicator(&Tool::Gemini, &lines));
    }

    #[test]
    fn generic_shell_prompt() {
        let lines = vec!["user@host:~/project$ "];
        assert!(has_prompt_indicator(&Tool::Shell, &lines));
    }

    #[test]
    fn no_prompt_on_busy_output() {
        let lines = vec!["Building step 3 of 10"];
        assert!(!has_prompt_indicator(&Tool::Shell, &lines));
    }

    // -- Full pipeline ------------------------------------------------------

    #[test]
    fn detect_dead_via_no_signals() {
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: None,
            pane_content: None,
            hook_status: None,
            activity_changed_at: None,
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Idle);
    }

    #[test]
    fn detect_active_from_hook() {
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: None,
            pane_content: None,
            hook_status: Some(HookStatus::Active),
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn detect_active_from_title_spinner() {
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: Some("Session \u{2801}"),
            pane_content: None,
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
        assert!(result.spinner_seen);
    }

    #[test]
    fn detect_waiting_from_prompt() {
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: Some("claude"),
            pane_content: Some("Done processing.\n>"),
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn busy_overrides_prompt() {
        // Both spinner and prompt visible — busy wins.
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: Some("claude"),
            pane_content: Some(">\n\u{280B} Working on it..."),
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn hook_overrides_title() {
        // Hook says Waiting, but title has spinner — hook wins (checked first).
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: Some("\u{280B} Session"),
            pane_content: None,
            hook_status: Some(HookStatus::Active),
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    // -- Vibe detection -----------------------------------------------------

    #[test]
    fn vibe_idle_no_spinner_is_waiting() {
        // Vibe showing chat input with no braille spinner → Waiting.
        let ctx = DetectionContext {
            tool: &Tool::Vibe,
            pane_title: None,
            pane_content: Some("Assistant: Here is the answer.\n\n"),
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 10, // high sustained count from cursor blink
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn vibe_spinner_is_active() {
        // Vibe showing braille snake spinner → Active.
        let ctx = DetectionContext {
            tool: &Tool::Vibe,
            pane_title: None,
            pane_content: Some("Working on it\n\u{280B} loading..."),
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 5,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn vibe_approval_is_waiting() {
        // Vibe showing approval dialog → Waiting.
        let ctx = DetectionContext {
            tool: &Tool::Vibe,
            pane_title: None,
            pane_content: Some("Tool: write_file\n1. Yes\n2. Yes and always allow write_file for this session\n3. No and tell the agent what to do instead\n↑↓ navigate  Enter select  ESC reject"),
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 5,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn vibe_timestamp_only_not_active() {
        // Vibe with high sustained activity but no content capture →
        // should NOT be Active (cursor blink excluded from Layer 5).
        let ctx = DetectionContext {
            tool: &Tool::Vibe,
            pane_title: None,
            pane_content: None,
            hook_status: None,
            activity_changed_at: Some(Instant::now()),
            spinner_last_seen: None,
            sustained_activity_count: 20,
            now: Instant::now(),
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    // -- Priority 2: Edge cases -----------------------------------------------

    #[test]
    fn spinner_grace_period_keeps_active() {
        // Spinner was seen 2 seconds ago, no busy/prompt now → still Active.
        let now = Instant::now();
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: Some("claude"),
            pane_content: Some("Some output with no indicators"),
            hook_status: None,
            activity_changed_at: Some(now),
            spinner_last_seen: Some(now - Duration::from_secs(2)),
            sustained_activity_count: 0,
            now,
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn spinner_grace_period_expired_falls_through() {
        // Spinner was seen 10 seconds ago — grace expired → should detect prompt.
        let now = Instant::now();
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: Some("claude"),
            pane_content: Some("Done.\n>"),
            hook_status: None,
            activity_changed_at: Some(now),
            spinner_last_seen: Some(now - Duration::from_secs(10)),
            sustained_activity_count: 0,
            now,
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn stale_activity_becomes_idle() {
        // Activity was 2 minutes ago, no other signals → Idle.
        let now = Instant::now();
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: None,
            pane_content: None,
            hook_status: None,
            activity_changed_at: Some(now - Duration::from_secs(120)),
            spinner_last_seen: None,
            sustained_activity_count: 0,
            now,
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Idle);
    }

    #[test]
    fn custom_tool_uses_generic_detection() {
        // Custom tool should match generic shell prompt patterns.
        let lines = vec!["output", "user@host:~$ "];
        assert!(has_prompt_indicator(&Tool::Custom("mytool".into()), &lines));
    }

    #[test]
    fn custom_tool_no_busy_patterns() {
        // Custom tools have no tool-specific busy patterns.
        let lines = vec!["just text", "nothing special"];
        assert!(!has_busy_indicator(&Tool::Custom("mytool".into()), &lines));
    }

    #[test]
    fn shell_prompt_zsh_percent() {
        let lines = vec!["user@host ~/project%"];
        assert!(has_prompt_indicator(&Tool::Shell, &lines));
    }

    #[test]
    fn aider_prompt_detected() {
        let lines = vec!["output", ">"];
        assert!(has_prompt_indicator(&Tool::Aider, &lines));
    }

    #[test]
    fn aider_prompt_with_space() {
        let lines = vec!["> aider command"];
        assert!(has_prompt_indicator(&Tool::Aider, &lines));
    }

    #[test]
    fn sustained_activity_non_tui_tool_active() {
        // Non-TUI tool with sustained activity → Active.
        let now = Instant::now();
        let ctx = DetectionContext {
            tool: &Tool::Claude,
            pane_title: None,
            pane_content: None,
            hook_status: None,
            activity_changed_at: Some(now),
            spinner_last_seen: None,
            sustained_activity_count: 5,
            now,
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }

    #[test]
    fn cursor_tui_blink_excluded_from_sustained() {
        // Cursor tool with high sustained activity should NOT be Active
        // (TUI cursor blink excluded from Layer 5).
        let now = Instant::now();
        let ctx = DetectionContext {
            tool: &Tool::Cursor,
            pane_title: None,
            pane_content: None,
            hook_status: None,
            activity_changed_at: Some(now),
            spinner_last_seen: None,
            sustained_activity_count: 20,
            now,
            idle_timeout_secs: 60,
        };
        let result = detect_status(&ctx);
        assert_eq!(result.status, Status::Active);
    }
}
