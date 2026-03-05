use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::session::instance::{Session, Tool};
use crate::tui::theme::{dark_theme, tool_color, Theme};
use crate::tui::zoxide;

// ---------------------------------------------------------------------------
// Constants & types
// ---------------------------------------------------------------------------

/// Available tool options: (display name, command key).
pub const TOOL_OPTIONS: &[(&str, &str)] = &[
    ("Claude", "claude"),
    ("Gemini", "gemini"),
    ("Codex", "codex"),
    ("OpenCode", "opencode"),
    ("Cursor", "cursor"),
    ("Aider", "aider"),
    ("Vibe", "vibe"),
    ("Shell", "shell"),
];

const MAX_DIR_RESULTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogField { Tool, Directory }

impl DialogField {
    fn next(&self) -> Self {
        match self {
            Self::Tool => Self::Directory,
            Self::Directory => Self::Tool,
        }
    }
    fn prev(&self) -> Self {
        match self {
            Self::Tool => Self::Directory,
            Self::Directory => Self::Tool,
        }
    }
}

pub enum DialogAction {
    Continue,
    Cancel,
    Create(Session),
}

// ---------------------------------------------------------------------------
// NewSessionDialog
// ---------------------------------------------------------------------------

pub struct NewSessionDialog {
    pub focus: DialogField,
    pub tool_index: usize,
    pub dir_query: String,
    pub zoxide_dirs: Vec<zoxide::ZoxideEntry>,
    pub dir_selected: usize,
    dir_confirmed: bool,
}

impl NewSessionDialog {
    pub fn new() -> Self {
        Self {
            focus: DialogField::Tool,
            tool_index: 0,
            dir_query: String::new(),
            zoxide_dirs: zoxide::load_zoxide_dirs(),
            dir_selected: 0,
            dir_confirmed: false,
        }
    }

    /// Handle a key event, returning the action the caller should take.
    pub fn handle_key(&mut self, key: KeyEvent) -> DialogAction {
        if key.code == KeyCode::Esc {
            return DialogAction::Cancel;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Enter {
            return self.try_create();
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            self.focus = if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.code == KeyCode::BackTab
            {
                self.focus.prev()
            } else {
                self.focus.next()
            };
            return DialogAction::Continue;
        }
        match self.focus {
            DialogField::Tool => self.handle_tool_key(key),
            DialogField::Directory => self.handle_dir_key(key),
        }
    }

    fn handle_tool_key(&mut self, key: KeyEvent) -> DialogAction {
        match key.code {
            KeyCode::Left => {
                self.tool_index = if self.tool_index == 0 {
                    TOOL_OPTIONS.len() - 1
                } else {
                    self.tool_index - 1
                };
            }
            KeyCode::Right => self.tool_index = (self.tool_index + 1) % TOOL_OPTIONS.len(),
            _ => {}
        }
        DialogAction::Continue
    }

    fn handle_dir_key(&mut self, key: KeyEvent) -> DialogAction {
        match key.code {
            KeyCode::Up => self.dir_selected = self.dir_selected.saturating_sub(1),
            KeyCode::Down => {
                let n = self.filtered_dir_count();
                if n > 0 { self.dir_selected = (self.dir_selected + 1).min(n - 1); }
            }
            KeyCode::Enter => {
                if let Some(path) = self.selected_dir_path() {
                    self.dir_query = path.clone();
                    self.dir_confirmed = true;
                    return self.try_create();
                }
            }
            KeyCode::Backspace => { self.dir_query.pop(); self.dir_selected = 0; self.dir_confirmed = false; }
            KeyCode::Char(c) => { self.dir_query.push(c); self.dir_selected = 0; self.dir_confirmed = false; }
            _ => {}
        }
        DialogAction::Continue
    }

    fn try_create(&self) -> DialogAction {
        let Some(path) = self.resolved_path() else { return DialogAction::Continue };
        let (_, cmd) = TOOL_OPTIONS[self.tool_index];
        DialogAction::Create(Session::new(String::new(), path, Tool::from_command(cmd)))
    }

    fn resolved_path(&self) -> Option<PathBuf> {
        if self.dir_confirmed { return Some(PathBuf::from(&self.dir_query)); }
        self.selected_dir_path().map(PathBuf::from)
    }

    fn selected_dir_path(&self) -> Option<String> {
        zoxide::fuzzy_filter(&self.zoxide_dirs, &self.dir_query, MAX_DIR_RESULTS)
            .get(self.dir_selected)
            .map(|e| e.path.clone())
    }

    fn filtered_dir_count(&self) -> usize {
        zoxide::fuzzy_filter(&self.zoxide_dirs, &self.dir_query, MAX_DIR_RESULTS).len()
    }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Render the "New Session" modal dialog centered on top of `area`.
pub fn render_new_session_dialog(frame: &mut Frame, dialog: &NewSessionDialog, area: Rect) {
    let theme = dark_theme();

    // Center the dialog: 60% width, 18 lines tall.
    let w = (area.width * 60 / 100).max(40).min(area.width);
    let h = 18u16.min(area.height);
    let vert = Layout::default().direction(Direction::Vertical).constraints([
        Constraint::Length((area.height.saturating_sub(h)) / 2),
        Constraint::Length(h),
        Constraint::Min(0),
    ]).split(area);
    let horiz = Layout::default().direction(Direction::Horizontal).constraints([
        Constraint::Length((area.width.saturating_sub(w)) / 2),
        Constraint::Length(w),
        Constraint::Min(0),
    ]).split(vert[1]);
    let dialog_area = horiz[1];

    // Dark overlay behind everything, then clear the dialog region.
    frame.render_widget(Block::default().style(Style::default().bg(theme.bg)), area);
    frame.render_widget(Clear, dialog_area);

    let outer = Block::default()
        .title(Span::styled(" New Session ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.surface));
    let inner = outer.inner(dialog_area);
    frame.render_widget(outer, dialog_area);

    let rows = Layout::default().direction(Direction::Vertical).constraints([
        Constraint::Length(3), // Tool
        Constraint::Length(3), // Directory input
        Constraint::Length(5), // Directory results
        Constraint::Min(0),   // spacer
        Constraint::Length(1), // footer
    ]).split(inner);

    render_tool_field(frame, dialog, rows[0], &theme);
    render_dir_input(frame, dialog, rows[1], &theme);
    render_dir_results(frame, dialog, rows[2], &theme);

    let footer = Line::from(vec![
        Span::styled("Tab", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(": next field  ", Style::default().fg(theme.text_dim)),
        Span::styled("Ctrl+Enter", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(": create  ", Style::default().fg(theme.text_dim)),
        Span::styled("Esc", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(": cancel", Style::default().fg(theme.text_dim)),
    ]);
    frame.render_widget(Paragraph::new(footer).style(Style::default().bg(theme.surface)), rows[4]);
}

// ---------------------------------------------------------------------------
// Field renderers
// ---------------------------------------------------------------------------

fn render_tool_field(frame: &mut Frame, dialog: &NewSessionDialog, area: Rect, theme: &Theme) {
    let focused = dialog.focus == DialogField::Tool;
    let (name, cmd) = TOOL_OPTIONS[dialog.tool_index];
    let arrow_fg = if focused { theme.accent } else { theme.text_dim };

    let content = Line::from(vec![
        Span::styled("\u{25C0} ", Style::default().fg(arrow_fg)),
        Span::styled(name, Style::default().fg(tool_color(cmd)).add_modifier(Modifier::BOLD)),
        Span::styled(" \u{25B6}", Style::default().fg(arrow_fg)),
    ]);
    let block = field_block(" Tool ", focused, theme);
    frame.render_widget(Paragraph::new(content).block(block), area);
}

fn render_dir_input(frame: &mut Frame, dialog: &NewSessionDialog, area: Rect, theme: &Theme) {
    let focused = dialog.focus == DialogField::Directory;
    let span = if dialog.dir_query.is_empty() && !focused {
        Span::styled("type to search directories...", Style::default().fg(theme.text_dim))
    } else {
        let cursor = if focused { "\u{2588}" } else { "" };
        Span::styled(format!("{}{}", dialog.dir_query, cursor), Style::default().fg(theme.text))
    };
    let block = field_block(" Directory ", focused, theme);
    frame.render_widget(Paragraph::new(Line::from(span)).block(block), area);
}

fn render_dir_results(frame: &mut Frame, dialog: &NewSessionDialog, area: Rect, theme: &Theme) {
    let filtered = zoxide::fuzzy_filter(&dialog.zoxide_dirs, &dialog.dir_query, MAX_DIR_RESULTS);
    if filtered.is_empty() {
        let p = Paragraph::new(Span::styled("  no matching directories", Style::default().fg(theme.text_dim)))
            .style(Style::default().bg(theme.surface));
        frame.render_widget(p, area);
        return;
    }
    let items: Vec<ListItem> = filtered.iter().enumerate().map(|(i, entry)| {
        let sel = i == dialog.dir_selected;
        let prefix = if sel { "\u{25B6} " } else { "  " };
        let path = shorten_home(&entry.path);
        ListItem::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(if sel { theme.accent } else { theme.text_dim })),
            Span::styled(path, Style::default().fg(if sel { theme.text } else { theme.text_dim })),
            Span::styled(format!(" ({:.0})", entry.score), Style::default().fg(theme.text_dim)),
        ]))
    }).collect();

    let mut state = ListState::default();
    state.select(Some(dialog.dir_selected.min(filtered.len() - 1)));
    let list = List::new(items)
        .style(Style::default().bg(theme.surface))
        .highlight_style(Style::default().bg(theme.bg));
    frame.render_stateful_widget(list, area, &mut state);
}

fn field_block<'a>(title: &'a str, focused: bool, theme: &'a Theme) -> Block<'a> {
    let bc = if focused { theme.border_focused } else { theme.border };
    Block::default()
        .title(Span::styled(title, Style::default().fg(theme.text_dim)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(bc))
        .style(Style::default().bg(theme.surface))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn shorten_home(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy();
        if path.starts_with(h.as_ref()) {
            return format!("~{}", &path[h.len()..]);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_dialog() -> NewSessionDialog {
        NewSessionDialog {
            focus: DialogField::Tool,
            tool_index: 0,
            dir_query: String::new(),
            zoxide_dirs: vec![
                crate::tui::zoxide::ZoxideEntry {
                    path: "/projects/alpha".into(),
                    score: 10.0,
                },
                crate::tui::zoxide::ZoxideEntry {
                    path: "/projects/beta".into(),
                    score: 5.0,
                },
            ],
            dir_selected: 0,
            dir_confirmed: false,
        }
    }

    #[test]
    fn esc_cancels() {
        let mut dialog = make_dialog();
        let action = dialog.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, DialogAction::Cancel));
    }

    #[test]
    fn tab_switches_focus() {
        let mut dialog = make_dialog();
        assert_eq!(dialog.focus, DialogField::Tool);
        dialog.handle_key(key(KeyCode::Tab));
        assert_eq!(dialog.focus, DialogField::Directory);
        dialog.handle_key(key(KeyCode::Tab));
        assert_eq!(dialog.focus, DialogField::Tool);
    }

    #[test]
    fn backtab_switches_focus_reverse() {
        let mut dialog = make_dialog();
        dialog.focus = DialogField::Directory;
        dialog.handle_key(key(KeyCode::BackTab));
        assert_eq!(dialog.focus, DialogField::Tool);
    }

    #[test]
    fn tool_right_wraps_around() {
        let mut dialog = make_dialog();
        assert_eq!(dialog.tool_index, 0);
        // Move right through all tools
        for _ in 0..TOOL_OPTIONS.len() {
            dialog.handle_key(key(KeyCode::Right));
        }
        // Should wrap back to 0
        assert_eq!(dialog.tool_index, 0);
    }

    #[test]
    fn tool_left_wraps_to_end() {
        let mut dialog = make_dialog();
        assert_eq!(dialog.tool_index, 0);
        dialog.handle_key(key(KeyCode::Left));
        assert_eq!(dialog.tool_index, TOOL_OPTIONS.len() - 1);
    }

    #[test]
    fn dir_typing_updates_query() {
        let mut dialog = make_dialog();
        dialog.focus = DialogField::Directory;
        dialog.handle_key(key(KeyCode::Char('a')));
        dialog.handle_key(key(KeyCode::Char('l')));
        assert_eq!(dialog.dir_query, "al");
    }

    #[test]
    fn dir_backspace_removes_char() {
        let mut dialog = make_dialog();
        dialog.focus = DialogField::Directory;
        dialog.dir_query = "abc".into();
        dialog.handle_key(key(KeyCode::Backspace));
        assert_eq!(dialog.dir_query, "ab");
    }

    #[test]
    fn dir_backspace_on_empty_is_noop() {
        let mut dialog = make_dialog();
        dialog.focus = DialogField::Directory;
        dialog.handle_key(key(KeyCode::Backspace));
        assert_eq!(dialog.dir_query, "");
    }

    #[test]
    fn dir_up_saturates_at_zero() {
        let mut dialog = make_dialog();
        dialog.focus = DialogField::Directory;
        dialog.dir_selected = 0;
        dialog.handle_key(key(KeyCode::Up));
        assert_eq!(dialog.dir_selected, 0);
    }

    #[test]
    fn dir_down_clamps_to_last() {
        let mut dialog = make_dialog();
        dialog.focus = DialogField::Directory;
        dialog.dir_selected = 0;
        // Move down past the end
        dialog.handle_key(key(KeyCode::Down));
        dialog.handle_key(key(KeyCode::Down));
        dialog.handle_key(key(KeyCode::Down));
        // Should clamp to last item index (1, since we have 2 dirs)
        assert!(dialog.dir_selected <= 1);
    }

    #[test]
    fn ctrl_enter_with_dir_creates_session() {
        let mut dialog = make_dialog();
        // Select first directory
        dialog.focus = DialogField::Directory;
        dialog.handle_key(key(KeyCode::Enter)); // select dir
        // Now Ctrl+Enter should create
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(matches!(action, DialogAction::Create(_)));
    }

    #[test]
    fn dialog_field_next_prev_cycle() {
        assert_eq!(DialogField::Tool.next(), DialogField::Directory);
        assert_eq!(DialogField::Directory.next(), DialogField::Tool);
        assert_eq!(DialogField::Tool.prev(), DialogField::Directory);
        assert_eq!(DialogField::Directory.prev(), DialogField::Tool);
    }
}
