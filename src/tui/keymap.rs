use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Result of mapping a crossterm KeyEvent for tmux dispatch.
pub enum TmuxKey {
    /// A literal character to send via `send_keys_raw` (-l flag).
    Literal(String),
    /// A special key name to send via `send_keys_special`.
    Special(String),
    /// Raw hex bytes to send via `send-keys -H` (space-separated hex pairs).
    /// Used for escape sequences that contain raw ESC bytes which would be
    /// misinterpreted by tmux's command parser if sent via `-l`.
    RawHex(String),
    /// The key cannot be meaningfully forwarded.
    Ignore,
}

/// Convert a crossterm `KeyEvent` to a tmux-compatible key representation.
pub fn map_key(key: &KeyEvent) -> TmuxKey {
    // Ctrl+<key> combos
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if let KeyCode::Char(c) = key.code {
            return TmuxKey::Special(format!("C-{}", c));
        }
    }

    // Alt+<key> combos
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char(c) = key.code {
            return TmuxKey::Special(format!("M-{}", c));
        }
    }

    // Shift+Enter → newline (not submit) in tools like Claude Code.
    // Send the CSI u escape sequence ESC[13;2u as raw hex bytes via
    // tmux send-keys -H. Using -l with the raw ESC byte (0x1B) causes
    // tmux's control-mode parser to misinterpret the command.
    if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Enter {
        // ESC [ 1 3 ; 2 u  →  1b 5b 31 33 3b 32 75
        return TmuxKey::RawHex("1b 5b 31 33 3b 32 75".into());
    }

    match key.code {
        KeyCode::Char(c) => TmuxKey::Literal(c.to_string()),
        KeyCode::Enter => TmuxKey::Special("Enter".into()),
        KeyCode::Backspace => TmuxKey::Special("BSpace".into()),
        KeyCode::Delete => TmuxKey::Special("DC".into()),
        KeyCode::Tab => TmuxKey::Special("Tab".into()),
        KeyCode::BackTab => TmuxKey::Special("BTab".into()),
        KeyCode::Esc => TmuxKey::Special("Escape".into()),
        KeyCode::Up => TmuxKey::Special("Up".into()),
        KeyCode::Down => TmuxKey::Special("Down".into()),
        KeyCode::Left => TmuxKey::Special("Left".into()),
        KeyCode::Right => TmuxKey::Special("Right".into()),
        KeyCode::Home => TmuxKey::Special("Home".into()),
        KeyCode::End => TmuxKey::Special("End".into()),
        KeyCode::PageUp => TmuxKey::Special("PageUp".into()),
        KeyCode::PageDown => TmuxKey::Special("PageDown".into()),
        KeyCode::Insert => TmuxKey::Special("IC".into()),
        KeyCode::F(n) => TmuxKey::Special(format!("F{}", n)),
        _ => TmuxKey::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn map_char() {
        match map_key(&key(KeyCode::Char('a'))) {
            TmuxKey::Literal(s) => assert_eq!(s, "a"),
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn map_enter() {
        match map_key(&key(KeyCode::Enter)) {
            TmuxKey::Special(s) => assert_eq!(s, "Enter"),
            _ => panic!("expected Special"),
        }
    }

    #[test]
    fn map_ctrl_c() {
        match map_key(&ctrl_key('c')) {
            TmuxKey::Special(s) => assert_eq!(s, "C-c"),
            _ => panic!("expected Special"),
        }
    }

    #[test]
    fn map_backspace() {
        match map_key(&key(KeyCode::Backspace)) {
            TmuxKey::Special(s) => assert_eq!(s, "BSpace"),
            _ => panic!("expected Special"),
        }
    }

    #[test]
    fn map_f_key() {
        match map_key(&key(KeyCode::F(5))) {
            TmuxKey::Special(s) => assert_eq!(s, "F5"),
            _ => panic!("expected Special"),
        }
    }

    #[test]
    fn map_shift_enter() {
        let k = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        match map_key(&k) {
            TmuxKey::RawHex(s) => assert_eq!(s, "1b 5b 31 33 3b 32 75"),
            _ => panic!("expected RawHex with CSI u hex bytes"),
        }
    }

    #[test]
    fn map_arrow_keys() {
        for (code, expected) in [
            (KeyCode::Up, "Up"),
            (KeyCode::Down, "Down"),
            (KeyCode::Left, "Left"),
            (KeyCode::Right, "Right"),
        ] {
            match map_key(&key(code)) {
                TmuxKey::Special(s) => assert_eq!(s, expected),
                _ => panic!("expected Special for {:?}", code),
            }
        }
    }
}
