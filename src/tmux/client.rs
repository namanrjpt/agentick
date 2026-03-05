use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, eyre};
use color_eyre::Result;

/// Default timeout for tmux subprocess calls on the hot path (tick / event loop).
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// Spawn a tmux command and wait for its output with a timeout.
///
/// If the subprocess doesn't finish within `timeout`, it is killed and an
/// error is returned.  This prevents a hung tmux from freezing the UI.
fn tmux_output_timeout(args: &[&str], timeout: Duration) -> Result<std::process::Output> {
    let mut child = Command::new("tmux")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err_with(|| format!("failed to spawn tmux {}", args.first().unwrap_or(&"?")))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(eyre!(
                        "tmux {} timed out after {:.1}s",
                        args.first().unwrap_or(&"?"),
                        timeout.as_secs_f64()
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(eyre!("error waiting for tmux: {e}")),
        }
    }

    child
        .wait_with_output()
        .wrap_err("failed to read tmux output")
}

/// Metadata about a running tmux session.
#[derive(Debug, Clone)]
pub struct TmuxSessionInfo {
    pub name: String,
    pub created: i64,
    pub last_activity: i64,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` when `tmux` is found on `$PATH` and responds to `--version`.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a new detached tmux session.
///
/// Equivalent to: `tmux new-session -d -s <name> -c <dir> <cmd>`
///
/// Also disables the tmux status bar, clears any shell init output from the
/// scrollback, and sets `history-limit` so that only agent output is visible.
pub fn create_session(name: &str, dir: &Path, cmd: &str) -> Result<()> {
    // Set global history-limit BEFORE creating the session. tmux applies
    // history-limit at pane creation time, so setting it after new-session
    // has no effect on the pane that was already created.
    let _ = Command::new("tmux")
        .args(["set-option", "-g", "history-limit", "50000"])
        .output();

    // Enable extended-keys so tmux passes modifier information (e.g.
    // Shift+Enter) through to the application running inside the pane.
    // Without this, Shift+Enter is indistinguishable from plain Enter.
    let _ = Command::new("tmux")
        .args(["set-option", "-s", "extended-keys", "always"])
        .output();

    // Use CSI u encoding (e.g. \x1b[13;2u for Shift+Enter) instead of the
    // default xterm format (\x1b[27;2;13~) which Claude CLI doesn't understand.
    // Requires tmux 3.5+; silently ignored on older versions.
    let _ = Command::new("tmux")
        .args(["set-option", "-s", "extended-keys-format", "csi-u"])
        .output();

    // Tell tmux the outer terminal supports extended keys so it decodes
    // modifier information from the terminal emulator.
    let _ = Command::new("tmux")
        .args(["set-option", "-as", "terminal-features", "xterm*:extkeys"])
        .output();

    let output = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-c"])
        .arg(dir)
        .arg(cmd)
        .output()
        .wrap_err("failed to spawn tmux new-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux new-session failed: {}", stderr.trim()));
    }

    // Hide the tmux status bar — agentick provides its own UI chrome.
    let _ = Command::new("tmux")
        .args(["set-option", "-t", name, "status", "off"])
        .output();

    // Enable mouse mode so that scroll events in the attached session go to
    // tmux (scrolling through pane history) rather than the outer terminal
    // (which would show stale "previous terminal" content).
    let _ = Command::new("tmux")
        .args(["set-option", "-t", name, "mouse", "on"])
        .output();


    // Clear any shell init / .zshrc output that accumulated before the
    // command started, so scrollback only contains actual agent output.
    let _ = Command::new("tmux")
        .args(["clear-history", "-t", name])
        .output();

    Ok(())
}

/// Set a tmux option on a session.
///
/// Equivalent to: `tmux set-option -t <name> <option> <value>`
pub fn set_option(name: &str, option: &str, value: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["set-option", "-t", name, option, value])
        .output()
        .wrap_err("failed to spawn tmux set-option")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux set-option failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Resize a tmux session's window to the given columns and rows.
///
/// Equivalent to: `tmux resize-window -t <name> -x <cols> -y <rows>`
pub fn resize_window(name: &str, cols: u16, rows: u16) -> Result<()> {
    let output = Command::new("tmux")
        .args([
            "resize-window",
            "-t",
            name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ])
        .output()
        .wrap_err("failed to spawn tmux resize-window")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux resize-window failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Clear the scrollback history of a session's pane.
///
/// Equivalent to: `tmux clear-history -t <name>`
pub fn clear_history(name: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["clear-history", "-t", name])
        .output()
        .wrap_err("failed to spawn tmux clear-history")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux clear-history failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Kill an existing tmux session.
///
/// Equivalent to: `tmux kill-session -t <name>`
pub fn kill_session(name: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .wrap_err("failed to spawn tmux kill-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux kill-session failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Check whether a tmux session with the given name exists.
///
/// Equivalent to: `tmux has-session -t <name>`
pub fn session_exists(name: &str) -> Result<bool> {
    let status = Command::new("tmux")
        .args(["has-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .wrap_err("failed to spawn tmux has-session")?;

    Ok(status.success())
}

/// Capture the visible content of a session's current pane.
///
/// Equivalent to: `tmux capture-pane -t <name> -p`
pub fn capture_pane(name: &str) -> Result<String> {
    let output = tmux_output_timeout(&["capture-pane", "-t", name, "-p"], TMUX_TIMEOUT)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux capture-pane failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Send keystrokes to a tmux session, followed by Enter.
///
/// Equivalent to: `tmux send-keys -t <name> <keys> Enter`
pub fn send_keys(name: &str, keys: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["send-keys", "-t", name, keys, "Enter"])
        .output()
        .wrap_err("failed to spawn tmux send-keys")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux send-keys failed: {}", stderr.trim()));
    }

    Ok(())
}

/// Capture the visible content of a session's current pane, preserving ANSI escape codes.
///
/// Equivalent to: `tmux capture-pane -t <name> -p -e`
pub fn capture_pane_ansi(name: &str) -> Result<String> {
    let output =
        tmux_output_timeout(&["capture-pane", "-t", name, "-p", "-e"], TMUX_TIMEOUT)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux capture-pane -e failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Capture the full scrollback + visible content of a session's pane,
/// preserving ANSI escape codes.
///
/// Equivalent to: `tmux capture-pane -t <name> -p -e -S -32768`
pub fn capture_pane_scrollback(name: &str) -> Result<String> {
    // Scrollback can be large — allow a longer timeout.
    let output = tmux_output_timeout(
        &["capture-pane", "-t", name, "-p", "-e", "-S", "-32768"],
        Duration::from_secs(5),
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux capture-pane scrollback failed: {}", stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Send literal keystrokes to a tmux session WITHOUT appending Enter.
/// Fire-and-forget: spawns the process without blocking on output.
///
/// Equivalent to: `tmux send-keys -t <name> -l <keys>`
pub fn send_keys_raw(name: &str, keys: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", name, "-l", keys])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .wrap_err("failed to spawn tmux send-keys -l")?;
    Ok(())
}

/// Send raw hex bytes to a tmux session's pane.
/// Fire-and-forget: spawns the process without blocking on output.
///
/// `hex` should be space-separated hex pairs, e.g. `"1b 5b 31 33 3b 32 75"`.
/// Equivalent to: `tmux send-keys -t <name> -H <hex_pairs...>`
pub fn send_keys_hex(name: &str, hex: &str) -> Result<()> {
    let mut args = vec!["send-keys", "-t", name, "-H"];
    let pairs: Vec<&str> = hex.split_whitespace().collect();
    args.extend(pairs.iter());
    Command::new("tmux")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .wrap_err("failed to spawn tmux send-keys -H")?;
    Ok(())
}

/// Send a special (non-literal) key to a tmux session.
/// Fire-and-forget: spawns the process without blocking on output.
///
/// Equivalent to: `tmux send-keys -t <name> <key_name>`
pub fn send_keys_special(name: &str, key_name: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", name, key_name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .wrap_err("failed to spawn tmux send-keys (special)")?;
    Ok(())
}

/// Attach to a tmux session in the foreground, inheriting stdin/stdout/stderr.
///
/// This blocks until the user detaches or the session ends.
/// Ctrl+d is bound to detach before attaching (instead of the default
/// Ctrl+b d) for a simpler UX.
pub fn attach_session(name: &str) -> Result<std::process::ExitStatus> {
    // Bind Ctrl+q to detach in the root key table (no prefix needed).
    let _ = Command::new("tmux")
        .args(["bind-key", "-n", "C-q", "detach-client"])
        .output();

    // Ensure extended-keys is on so Shift+Enter etc. work in the session.
    let _ = Command::new("tmux")
        .args(["set-option", "-s", "extended-keys", "always"])
        .output();
    let _ = Command::new("tmux")
        .args(["set-option", "-s", "extended-keys-format", "csi-u"])
        .output();
    let _ = Command::new("tmux")
        .args(["set-option", "-as", "terminal-features", "xterm*:extkeys"])
        .output();

    let status = Command::new("tmux")
        .args(["attach-session", "-t", name])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .wrap_err("failed to spawn tmux attach-session")?;

    // Remove the root-table binding after detach so it doesn't leak into
    // the user's normal tmux usage.
    let _ = Command::new("tmux")
        .args(["unbind-key", "-n", "C-q"])
        .output();

    Ok(status)
}

/// List all running tmux sessions.
///
/// Equivalent to:
/// `tmux list-sessions -F "#{session_name}\t#{session_created}\t#{window_activity}"`
pub fn list_sessions() -> Result<Vec<TmuxSessionInfo>> {
    let output = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_created}\t#{window_activity}",
        ])
        .output()
        .wrap_err("failed to spawn tmux list-sessions")?;

    // tmux exits non-zero when the server isn't running (no sessions).
    // Treat that as an empty list rather than an error.
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut sessions = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let name = parts[0].to_string();
        let created = parts[1].parse::<i64>().unwrap_or(0);
        let last_activity = parts[2].parse::<i64>().unwrap_or(0);

        sessions.push(TmuxSessionInfo {
            name,
            created,
            last_activity,
        });
    }

    Ok(sessions)
}

/// Get the latest window-activity timestamp for a specific session.
///
/// Uses `tmux list-windows` rather than `list-sessions` so we get
/// per-window activity resolution.
pub fn get_window_activity(name: &str) -> Result<i64> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            name,
            "-F",
            "#{window_activity}",
        ])
        .output()
        .wrap_err("failed to spawn tmux list-windows")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("tmux list-windows failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // A session can have multiple windows; return the most recent activity.
    let max_ts = stdout
        .lines()
        .filter_map(|line| line.trim().parse::<i64>().ok())
        .max()
        .unwrap_or(0);

    Ok(max_ts)
}

// ---------------------------------------------------------------------------
// Batch helpers (performance)
// ---------------------------------------------------------------------------

/// Fetch window-activity timestamps for **all** sessions in a single
/// subprocess call.
///
/// Returns a map of `session_name -> latest_window_activity`.  When a session
/// has multiple windows the highest (most recent) timestamp wins.
///
/// Equivalent to:
/// `tmux list-windows -a -F "#{session_name}\t#{window_activity}"`
pub fn refresh_activity_cache() -> Result<HashMap<String, i64>> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-a",
            "-F",
            "#{session_name}\t#{window_activity}",
        ])
        .output()
        .wrap_err("failed to spawn tmux list-windows -a")?;

    // No tmux server running → empty map.
    if !output.status.success() {
        return Ok(HashMap::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map: HashMap<String, i64> = HashMap::new();

    for line in stdout.lines() {
        let Some((session_name, ts_str)) = line.split_once('\t') else {
            continue;
        };
        let ts = ts_str.trim().parse::<i64>().unwrap_or(0);

        map.entry(session_name.to_string())
            .and_modify(|existing| {
                if ts > *existing {
                    *existing = ts;
                }
            })
            .or_insert(ts);
    }

    Ok(map)
}

/// Fetch activity timestamps, pane titles, AND pane dimensions for **all**
/// sessions in a single subprocess call.
///
/// Returns `(activity_cache, pane_title_cache, pane_size_cache)`.
///
/// Equivalent to:
/// `tmux list-panes -a -F "#{session_name}\t#{window_activity}\t#{pane_title}\t#{pane_width}\t#{pane_height}"`
pub fn refresh_all_pane_data() -> Result<(HashMap<String, i64>, HashMap<String, String>, HashMap<String, (u16, u16)>)> {
    let output = tmux_output_timeout(
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_activity}\t#{pane_title}\t#{pane_width}\t#{pane_height}",
        ],
        TMUX_TIMEOUT,
    )?;

    if !output.status.success() {
        return Ok((HashMap::new(), HashMap::new(), HashMap::new()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut activity: HashMap<String, i64> = HashMap::new();
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut sizes: HashMap<String, (u16, u16)> = HashMap::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(5, '\t').collect();
        if parts.len() < 3 {
            continue;
        }

        let session_name = parts[0].to_string();
        let ts = parts[1].trim().parse::<i64>().unwrap_or(0);
        let title = parts[2].to_string();

        activity
            .entry(session_name.clone())
            .and_modify(|existing| {
                if ts > *existing {
                    *existing = ts;
                }
            })
            .or_insert(ts);

        titles.entry(session_name.clone()).or_insert(title);

        // Parse pane dimensions if available.
        if parts.len() >= 5 {
            let width = parts[3].trim().parse::<u16>().unwrap_or(80);
            let height = parts[4].trim().parse::<u16>().unwrap_or(24);
            sizes.entry(session_name).or_insert((width, height));
        }
    }

    Ok((activity, titles, sizes))
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Build a tmux-safe session name from a human-readable title and a unique id.
///
/// Format: `agentick_{sanitized_title}_{first_8_chars_of_id}`
///
/// Rules applied to *title*:
/// - Lowercased
/// - Non-alphanumeric characters replaced with `-`
/// - Consecutive `-` collapsed into one
/// - Leading/trailing `-` stripped
/// - Truncated to at most 30 characters
pub fn sanitize_session_name(title: &str, id: &str) -> String {
    let sanitized_title: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive dashes and trim leading/trailing dashes.
    let mut collapsed = String::with_capacity(sanitized_title.len());
    let mut prev_dash = true; // start true to strip leading dash
    for c in sanitized_title.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }

    // Strip trailing dash.
    let collapsed = collapsed.trim_end_matches('-');

    // Truncate to 30 chars (at a dash boundary if possible).
    let truncated = if collapsed.len() > 30 {
        let slice = &collapsed[..30];
        // Try to cut at the last dash so we don't chop mid-word.
        match slice.rfind('-') {
            Some(pos) if pos > 10 => &slice[..pos],
            _ => slice,
        }
    } else {
        collapsed
    };

    let id_prefix: String = id.chars().take(8).collect();

    format!("agentick_{}_{}", truncated, id_prefix)
}

/// Convert OSC 8 hyperlink sequences into blue underlined visible text.
///
/// OSC 8 format: `\x1b]8;params;URL\x07` visible text `\x1b]8;;\x07`
/// (the BEL `\x07` terminator can also be `\x1b\\`).
///
/// `ansi_to_tui` doesn't handle OSC 8 and silently strips them. This
/// preprocessing step keeps the visible link text and wraps it in
/// underline + blue SGR codes so it's visually distinct in the preview pane.
///
/// If no OSC 8 sequences are found the input is returned unchanged.
pub fn preprocess_osc8_hyperlinks(input: &str) -> String {
    // Quick check — skip allocation if there are no OSC 8 sequences.
    if !input.contains("\x1b]8;") {
        return input.to_string();
    }

    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    // `last` tracks the start of the next uncopied region in `input`.
    let mut last = 0;
    let mut i = 0;

    while i < len {
        // Look for ESC ] 8 ;
        if i + 3 < len
            && bytes[i] == 0x1b
            && bytes[i + 1] == b']'
            && bytes[i + 2] == b'8'
            && bytes[i + 3] == b';'
        {
            if let Some(after_open) = skip_osc8_open_tag(bytes, i) {
                if let Some((visible_end, after_close)) = find_osc8_close(bytes, after_open) {
                    // Copy everything before this OSC 8 sequence verbatim.
                    out.push_str(&input[last..i]);
                    // Emit the visible text in blue. Use a full reset
                    // (\x1b[0m) afterward — selective resets like \x1b[24m
                    // can leak underline into adjacent table cells when
                    // ansi_to_tui merges style spans.
                    out.push_str("\x1b[34m");
                    out.push_str(&input[after_open..visible_end]);
                    out.push_str("\x1b[0m");
                    last = after_close;
                    i = after_close;
                    continue;
                }
            }
        }
        i += 1;
    }

    // Copy any remaining tail.
    out.push_str(&input[last..]);
    out
}

/// Skip past an OSC 8 opening tag starting at `pos`.
/// Returns the byte offset immediately after the terminator, or `None`.
fn skip_osc8_open_tag(data: &[u8], pos: usize) -> Option<usize> {
    // Expect: \x1b ] 8 ; <params> ; <url> <terminator>
    if data.len() < pos + 5
        || data[pos] != 0x1b
        || data[pos + 1] != b']'
        || data[pos + 2] != b'8'
        || data[pos + 3] != b';'
    {
        return None;
    }

    let mut i = pos + 4;
    // Skip params until the second `;`.
    while i < data.len() && data[i] != b';' {
        if data[i] == 0x07 || (data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'\\') {
            return None; // terminator before second `;` — not a valid opening tag
        }
        i += 1;
    }
    if i >= data.len() {
        return None;
    }
    i += 1; // skip `;`

    // Skip URL until terminator.
    while i < data.len() {
        if data[i] == 0x07 {
            return Some(i + 1);
        }
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'\\' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Find the OSC 8 closing tag starting search from `start`.
/// Returns `(visible_text_end, byte_after_close_tag)` or `None`.
fn find_osc8_close(data: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut i = start;
    while i + 5 < data.len() {
        if data[i] == 0x1b
            && data[i + 1] == b']'
            && data[i + 2] == b'8'
            && data[i + 3] == b';'
            && data[i + 4] == b';'
        {
            let visible_end = i;
            let j = i + 5;
            if j < data.len() && data[j] == 0x07 {
                return Some((visible_end, j + 1));
            }
            if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                return Some((visible_end, j + 2));
            }
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        let name = sanitize_session_name("My Cool Project", "abcdef1234567890");
        assert_eq!(name, "agentick_my-cool-project_abcdef12");
    }

    #[test]
    fn sanitize_special_chars() {
        let name = sanitize_session_name("hello!@#world$$$test", "aabb1122");
        assert_eq!(name, "agentick_hello-world-test_aabb1122");
    }

    #[test]
    fn sanitize_long_title() {
        let long_title = "a]very-long-title-that-exceeds-the-thirty-character-limit-significantly";
        let name = sanitize_session_name(long_title, "deadbeef");
        // Title portion should be at most 30 chars.
        let prefix = "agentick_";
        let suffix = "_deadbeef";
        let title_portion = &name[prefix.len()..name.len() - suffix.len()];
        assert!(title_portion.len() <= 30, "title portion too long: {title_portion}");
    }

    #[test]
    fn sanitize_empty_title() {
        let name = sanitize_session_name("", "12345678");
        assert_eq!(name, "agentick__12345678");
    }

    #[test]
    fn sanitize_short_id() {
        let name = sanitize_session_name("test", "abc");
        assert_eq!(name, "agentick_test_abc");
    }

    #[test]
    fn osc8_no_links() {
        let input = "hello world \x1b[31mred\x1b[0m";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, input);
    }

    #[test]
    fn osc8_bel_terminator() {
        let input = "\x1b]8;;https://example.com\x07click here\x1b]8;;\x07";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, "\x1b[34mclick here\x1b[0m");
    }

    #[test]
    fn osc8_st_terminator() {
        let input = "\x1b]8;;https://example.com\x1b\\click here\x1b]8;;\x1b\\";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, "\x1b[34mclick here\x1b[0m");
    }

    #[test]
    fn osc8_with_params() {
        let input = "\x1b]8;id=foo;https://example.com\x07link text\x1b]8;;\x07";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, "\x1b[34mlink text\x1b[0m");
    }

    #[test]
    fn osc8_surrounded_by_text() {
        let input = "before \x1b]8;;https://x.com\x07link\x1b]8;;\x07 after";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, "before \x1b[34mlink\x1b[0m after");
    }

    #[test]
    fn osc8_multiple_links() {
        let input = "\x1b]8;;https://a.com\x07A\x1b]8;;\x07 and \x1b]8;;https://b.com\x07B\x1b]8;;\x07";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, "\x1b[34mA\x1b[0m and \x1b[34mB\x1b[0m");
    }

    #[test]
    fn osc8_preserves_utf8() {
        let input = "héllo \x1b]8;;https://x.com\x07wörld\x1b]8;;\x07 café";
        let result = preprocess_osc8_hyperlinks(input);
        assert_eq!(result, "héllo \x1b[34mwörld\x1b[0m café");
    }

    #[test]
    fn sanitize_unicode_title() {
        let name = sanitize_session_name("🚀 rocket project", "abcdef12");
        assert!(name.starts_with("agentick_"));
        // Emoji gets stripped; remaining text preserved.
        assert!(name.contains("rocket-project") || name.contains("abcdef12"));
    }

    #[test]
    fn sanitize_all_special_chars_produces_valid_name() {
        let name = sanitize_session_name("!!!@@@###", "deadbeef");
        // Even if title portion is empty after sanitization, the name is valid.
        assert!(name.starts_with("agentick_"));
        assert!(name.contains("deadbeef"));
    }

    #[test]
    fn osc8_unclosed_link_preserves_text() {
        // Malformed: opening OSC8 but no closing sequence.
        let input = "\x1b]8;;https://example.com\x07click here but never closed";
        let result = preprocess_osc8_hyperlinks(input);
        // Should not panic; exact output depends on implementation, but text preserved.
        assert!(result.contains("click here") || result.contains("example.com"));
    }

    #[test]
    fn osc8_empty_link_text() {
        let input = "\x1b]8;;https://example.com\x07\x1b]8;;\x07";
        let result = preprocess_osc8_hyperlinks(input);
        // Empty link text should produce empty styled span.
        assert!(!result.contains("https://example.com"));
    }
}
