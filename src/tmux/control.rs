use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use color_eyre::eyre::eyre;
use color_eyre::Result;

/// A persistent connection to tmux via control mode (`tmux -C`).
///
/// Instead of spawning a new subprocess for every `send-keys` or
/// `capture-pane`, we keep a single tmux control-mode process alive
/// and write commands to its stdin (pipe write ~0.07ms vs fork+exec ~5.5ms).
///
/// A background reader thread parses the control-mode protocol and
/// streams `%output` bytes over an mpsc channel for real-time preview.
pub struct TmuxControlClient {
    child: Child,
    stdin: ChildStdin,
    output_rx: mpsc::Receiver<Vec<u8>>,
    session_name: String,
    alive: bool,
}

impl TmuxControlClient {
    /// Attach to an existing tmux session in control mode.
    ///
    /// Spawns `tmux -C attach-session -t <session_name>` with piped
    /// stdin/stdout and starts a reader thread that parses the control
    /// mode protocol.
    pub fn attach(session_name: &str) -> Result<Self> {
        let mut child = Command::new("tmux")
            .args(["-C", "attach-session", "-t", session_name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| eyre!("failed to spawn tmux -C: {}", e))?;

        let stdin = child.stdin.take().ok_or_else(|| eyre!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| eyre!("no stdout"))?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        // Reader thread: parse tmux control-mode protocol lines.
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break, // pipe closed
                };

                if line.starts_with("%output ") {
                    // Format: %output %<pane_id> <data>
                    // Find the second space (after %<pane_id>) to get data.
                    if let Some(first_space) = line[8..].find(' ') {
                        let data_str = &line[8 + first_space + 1..];
                        let bytes = unescape_tmux_octal(data_str);
                        if tx.send(bytes).is_err() {
                            break; // receiver dropped
                        }
                    }
                } else if line.starts_with("%exit") {
                    break;
                }
                // Ignore %begin, %end, %session-changed, etc.
            }
        });

        let mut client = Self {
            child,
            stdin,
            output_rx: rx,
            session_name: session_name.to_string(),
            alive: true,
        };

        // Set the control client's size large enough so it never shrinks the
        // real tmux pane.  tmux uses the smallest attached client by default,
        // so an unsized control client would collapse the pane to 0x0.
        // `refresh-client -C` tells tmux what size this (invisible) client is.
        let _ = client.write_cmd("refresh-client -C 400x200\n");

        Ok(client)
    }

    /// Send literal text to the tmux pane (no special key interpretation).
    ///
    /// Equivalent to `tmux send-keys -t <session> -l <keys>` but via pipe.
    pub fn send_keys_literal(&mut self, keys: &str) -> Result<()> {
        if !self.is_alive() {
            return Err(eyre!("control client is dead"));
        }
        // Escape single quotes in keys for tmux command
        let escaped = keys.replace('\'', "'\\''");
        let cmd = format!("send-keys -t {} -l '{}'\n", self.session_name, escaped);
        self.write_cmd(&cmd)
    }

    /// Send a special key (Enter, Escape, Tab, etc.) to the tmux pane.
    ///
    /// Equivalent to `tmux send-keys -t <session> <key>` but via pipe.
    pub fn send_keys_special(&mut self, key: &str) -> Result<()> {
        if !self.is_alive() {
            return Err(eyre!("control client is dead"));
        }
        let cmd = format!("send-keys -t {} {}\n", self.session_name, key);
        self.write_cmd(&cmd)
    }

    /// Non-blocking drain of all pending `%output` bytes.
    ///
    /// Returns accumulated bytes from all pending `%output` notifications.
    pub fn drain_output(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        while let Ok(chunk) = self.output_rx.try_recv() {
            buf.extend_from_slice(&chunk);
        }
        buf
    }

    /// Check if the tmux control-mode child process is still running.
    pub fn is_alive(&mut self) -> bool {
        if !self.alive {
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.alive = false;
                false
            }
            Ok(None) => true,
            Err(_) => {
                self.alive = false;
                false
            }
        }
    }

    /// Write a command string to the control-mode stdin.
    fn write_cmd(&mut self, cmd: &str) -> Result<()> {
        if self.stdin.write_all(cmd.as_bytes()).is_err() {
            self.alive = false;
            return Err(eyre!("failed to write to tmux control stdin"));
        }
        if self.stdin.flush().is_err() {
            self.alive = false;
            return Err(eyre!("failed to flush tmux control stdin"));
        }
        Ok(())
    }
}

impl Drop for TmuxControlClient {
    fn drop(&mut self) {
        // Try to detach gracefully, but never block.
        let _ = self.write_cmd("detach\n");
        // Give tmux a brief moment, then force-kill to avoid hanging.
        match self.child.try_wait() {
            Ok(Some(_)) => {} // already exited
            _ => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

/// Decode tmux octal escapes in `%output` data.
///
/// tmux control mode encodes non-printable bytes as `\NNN` (3-digit octal).
/// Backslash itself is encoded as `\\`.
fn unescape_tmux_octal(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0;

    while i < len {
        if bytes[i] == b'\\' && i + 1 < len {
            if bytes[i + 1] == b'\\' {
                out.push(b'\\');
                i += 2;
            } else if i + 3 < len
                && bytes[i + 1].is_ascii_digit()
                && bytes[i + 2].is_ascii_digit()
                && bytes[i + 3].is_ascii_digit()
            {
                // Octal escape: \NNN
                let d1 = (bytes[i + 1] - b'0') as u8;
                let d2 = (bytes[i + 2] - b'0') as u8;
                let d3 = (bytes[i + 3] - b'0') as u8;
                out.push(d1 * 64 + d2 * 8 + d3);
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_plain_text() {
        assert_eq!(unescape_tmux_octal("hello world"), b"hello world");
    }

    #[test]
    fn unescape_backslash() {
        assert_eq!(unescape_tmux_octal("a\\\\b"), b"a\\b");
    }

    #[test]
    fn unescape_octal_newline() {
        // \012 = newline (10 decimal)
        assert_eq!(unescape_tmux_octal("line1\\012line2"), b"line1\nline2");
    }

    #[test]
    fn unescape_octal_escape_char() {
        // \033 = ESC (27 decimal)
        let result = unescape_tmux_octal("\\033[31m");
        assert_eq!(result, b"\x1b[31m");
    }

    #[test]
    fn unescape_mixed() {
        let result = unescape_tmux_octal("hi\\033[0m\\012\\\\done");
        assert_eq!(result, b"hi\x1b[0m\n\\done");
    }
}
