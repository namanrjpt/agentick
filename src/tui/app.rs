use std::collections::HashMap;
use std::io::stdout;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use color_eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::text::Text;
use ratatui::{DefaultTerminal, Frame};

use crate::session::instance::{Group, Status};
use crate::session::store::SessionStore;
use crate::tmux::client as tmux;
use crate::tmux::control::TmuxControlClient;
use crate::tmux::detector::{self, DetectionContext, HookStatus};
use crate::tui::views::dashboard;
use crate::tui::views::group_dialog;
use crate::tui::views::new_session::{self, DialogAction, NewSessionDialog};
use crate::tui::views::search;

// ---------------------------------------------------------------------------
// Focus pane
// ---------------------------------------------------------------------------

/// Which pane currently has keyboard focus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    /// Left panel: session list navigation (default).
    Left,
    /// Right panel: interactive agent terminal.
    Right,
}

// ---------------------------------------------------------------------------
// App mode
// ---------------------------------------------------------------------------

pub enum AppMode {
    Normal,
    NewSession(Box<NewSessionDialog>),
    ConfirmDelete(String),
    ConfirmKill(String),
    Search {
        query: String,
        filtered_indices: Vec<usize>,
        selected: usize,
    },
    GroupDialog {
        name: String,
        assign_session: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub store: SessionStore,
    pub selected: usize,
    pub should_quit: bool,
    pub status_filter: Option<String>,
    pub mode: AppMode,
    pub focus: FocusPane,
    pub activity_cache: HashMap<String, i64>,
    pub pane_title_cache: HashMap<String, String>,
    pub preview_scroll: usize,
    pub tick_count: u32,
    /// Frame counter incremented every draw — used for animation (flashing dots).
    pub frame_count: u32,
    needs_clear: bool,
    last_tick: Instant,
    /// Track the terminal width so we know if the right pane is visible.
    terminal_width: u16,
    /// Activity timestamps from the previous tick (for change detection).
    prev_activity: HashMap<String, i64>,
    /// When activity last changed for each session (for done→idle transition).
    activity_changed_at: HashMap<String, Instant>,
    /// Hook statuses read from ~/.agentick/hooks/ (refreshed every 5 ticks).
    hook_status_cache: HashMap<String, HookStatus>,
    /// When a spinner was last seen per session (for grace period).
    spinner_last_seen: HashMap<String, Instant>,
    /// Count of consecutive ticks with activity changes (for sustained detection).
    sustained_activity_count: HashMap<String, u32>,
    /// Pane dimensions (cols, rows) from tmux, keyed by session name.
    pane_size_cache: HashMap<String, (u16, u16)>,
    /// Persistent tmux control-mode connection for the selected session.
    control_client: Option<TmuxControlClient>,
    /// Which tmux session the control client is connected to.
    control_session: Option<String>,
    /// When the last keystroke was forwarded (for adaptive poll rate).
    last_keystroke_at: Instant,
    /// When the preview was last refreshed (for debouncing during rapid j/k).
    last_preview_at: Instant,
    /// Cached styled text for scroll mode (captured once on scroll, not every frame).
    scroll_cache: Option<Text<'static>>,
    /// Styled preview content from capture-pane, refreshed on activity.
    preview_content: Option<Text<'static>>,
    /// Set when preview needs re-capture (output detected, cursor moved, etc).
    preview_stale: bool,
    /// Cache for token file parsing — avoids re-reading unchanged JSONL files.
    token_cache: crate::session::tokens::TokenCache,
}

impl App {
    /// Create a new `App`, loading the session store from disk.
    pub fn new() -> Result<Self> {
        let mut store = SessionStore::load()?;
        let mut token_cache = crate::session::tokens::TokenCache::new();
        // Refresh token data immediately so stale persisted values don't show.
        crate::session::tokens::refresh_all(&mut store.sessions, &mut token_cache);
        // Ensure hook handler script and Claude Code hook config are installed.
        crate::hooks::setup::ensure_hooks_installed();
        Ok(Self {
            store,
            selected: 0,
            should_quit: false,
            status_filter: None,
            mode: AppMode::Normal,
            focus: FocusPane::Left,
            activity_cache: HashMap::new(),
            pane_title_cache: HashMap::new(),
            preview_scroll: 0,
            tick_count: 0,
            frame_count: 0,
            needs_clear: true,
            last_tick: Instant::now(),
            terminal_width: 0,
            prev_activity: HashMap::new(),
            activity_changed_at: HashMap::new(),
            hook_status_cache: HashMap::new(),
            spinner_last_seen: HashMap::new(),
            sustained_activity_count: HashMap::new(),
            pane_size_cache: HashMap::new(),
            control_client: None,
            control_session: None,
            last_keystroke_at: Instant::now() - Duration::from_secs(10),
            last_preview_at: Instant::now() - Duration::from_secs(10),
            scroll_cache: None,
            preview_content: None,
            preview_stale: true,
            token_cache,
        })
    }

    /// Draw the current frame.
    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.terminal_width = area.width;
        self.frame_count = self.frame_count.wrapping_add(1);

        // If terminal is too narrow for the right pane, force focus left.
        if area.width <= 100 && self.focus == FocusPane::Right {
            self.focus = FocusPane::Left;
        }

        // Always render the dashboard underneath.
        dashboard::render_dashboard(
            frame,
            &self.store.sessions,
            &self.store.groups,
            self.selected,
            self.status_filter.as_deref(),
            self.preview_content.as_ref(),
            self.scroll_cache.as_ref(),
            self.preview_scroll,
            self.focus,
            self.frame_count,
            area,
        );

        // Render modal overlays on top.
        match &self.mode {
            AppMode::NewSession(dialog) => {
                new_session::render_new_session_dialog(frame, dialog, area);
            }
            AppMode::ConfirmDelete(id) => {
                dashboard::render_confirm_dialog(
                    frame,
                    &format!("Delete session '{}'?", id),
                    area,
                );
            }
            AppMode::ConfirmKill(id) => {
                dashboard::render_confirm_dialog(
                    frame,
                    &format!("Kill tmux session '{}'?", id),
                    area,
                );
            }
            AppMode::Search {
                query,
                filtered_indices,
                ..
            } => {
                search::render_search_bar(frame, query, filtered_indices.len(), area);
            }
            AppMode::GroupDialog {
                name,
                assign_session,
            } => {
                // Resolve the session title for display context.
                let session_title = assign_session.as_ref().and_then(|id| {
                    self.store
                        .find_session(id)
                        .map(|s| s.title.clone())
                });
                group_dialog::render_group_dialog(
                    frame,
                    name,
                    session_title.as_deref(),
                    area,
                );
            }
            AppMode::Normal => {}
        }
    }

    /// Handle a key event.
    fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // Backtick toggles focus between left/right panes (only in Normal mode).
        if matches!(self.mode, AppMode::Normal)
            && key.code == KeyCode::Char('`')
            && key.modifiers.is_empty()
        {
            if self.terminal_width > 100 {
                self.focus = match self.focus {
                    FocusPane::Left => FocusPane::Right,
                    FocusPane::Right => FocusPane::Left,
                };
            }
            return;
        }

        // When right pane is focused in Normal mode, forward all keys to tmux.
        if self.focus == FocusPane::Right && matches!(self.mode, AppMode::Normal) {
            self.forward_key_to_tmux(key);
            return;
        }

        // Handle NewSession mode separately: take ownership of the dialog so we
        // can call handle_key(&mut dialog) without conflicting borrows on self.
        if matches!(self.mode, AppMode::NewSession(_)) {
            let mut mode = std::mem::replace(&mut self.mode, AppMode::Normal);
            if let AppMode::NewSession(ref mut dialog) = mode {
                match dialog.handle_key(key) {
                    DialogAction::Continue => {
                        self.mode = mode;
                    }
                    DialogAction::Cancel => {
                        // mode is already Normal from the replace above.
                    }
                    DialogAction::Create(session) => {
                        // Spawn tmux session.
                        let _ = tmux::create_session(
                            &session.tmux_name,
                            &session.project_path,
                            &session.command,
                        );
                        // Add to store and save.
                        self.store.add_session(session);
                        let _ = self.store.save();
                    }
                }
            }
            return;
        }

        match &self.mode {
            AppMode::Normal => self.handle_normal_key(key),
            AppMode::NewSession(_) => unreachable!(),
            AppMode::ConfirmDelete(id) => {
                let id = id.clone();
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.store.remove_session(&id);
                        let _ = self.store.save();
                        self.clamp_cursor();
                        self.mode = AppMode::Normal;
                    }
                    _ => {
                        self.mode = AppMode::Normal;
                    }
                }
            }
            AppMode::ConfirmKill(id) => {
                let id = id.clone();
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        if let Some(session) = self.store.find_session(&id) {
                            let _ = tmux::kill_session(&session.tmux_name);
                        }
                        self.mode = AppMode::Normal;
                    }
                    _ => {
                        self.mode = AppMode::Normal;
                    }
                }
            }
            AppMode::Search { .. } => {
                self.handle_search_key(key);
            }
            AppMode::GroupDialog { .. } => {
                self.handle_group_dialog_key(key);
            }
        }
    }

    /// Handle key events in Normal mode.
    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_cursor_down();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_cursor_up();
            }
            KeyCode::Enter => {
                self.attach_selected();
            }
            KeyCode::Char('n') => {
                self.mode = AppMode::NewSession(Box::new(NewSessionDialog::new()));
            }
            KeyCode::Char('d') => {
                if let Some(id) = self.selected_session_id() {
                    self.mode = AppMode::ConfirmDelete(id);
                }
            }
            KeyCode::Char('K') => {
                if let Some(id) = self.selected_session_id() {
                    self.mode = AppMode::ConfirmKill(id);
                }
            }
            KeyCode::Tab => {
                self.cycle_status_filter();
            }
            KeyCode::Char('/') => {
                let filtered_indices = self.compute_search_indices("");
                self.mode = AppMode::Search {
                    query: String::new(),
                    filtered_indices,
                    selected: 0,
                };
            }
            KeyCode::Char('g') => {
                let assign_session = self.selected_session_id();
                self.mode = AppMode::GroupDialog {
                    name: String::new(),
                    assign_session,
                };
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.toggle_selected_group(false);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.toggle_selected_group(true);
            }
            _ => {}
        }
    }

    /// Periodic tick: 5-layer status detection pipeline + token refresh.
    ///
    /// Priority: Dead → Hooks → Title → Content (busy→prompt) → Timestamps
    fn tick(&mut self) {
        self.tick_count = self.tick_count.wrapping_add(1);
        let now = Instant::now();

        // --- Batch tmux data: activity timestamps + pane titles + pane sizes in one call ---
        if let Ok((activity, titles, sizes)) = tmux::refresh_all_pane_data() {
            self.activity_cache = activity;
            self.pane_title_cache = titles;
            self.pane_size_cache = sizes;
        }

        // --- Refresh hook statuses every 5 ticks (~2.5s) ---
        if self.tick_count % 5 == 0 {
            self.hook_status_cache = crate::hooks::read_hook_statuses()
                .into_iter()
                .map(|(k, v)| {
                    let hook = match v {
                        crate::tmux::detector::HookStatus::Active => HookStatus::Active,
                        crate::tmux::detector::HookStatus::Waiting => HookStatus::Waiting,
                        crate::tmux::detector::HookStatus::Done => HookStatus::Done,
                    };
                    (k, hook)
                })
                .collect();
        }

        // Update last_activity on each session from the cache.
        for session in &mut self.store.sessions {
            if let Some(&ts) = self.activity_cache.get(&session.tmux_name) {
                session.last_activity = Some(ts);
            }
        }

        // --- Track activity changes for timestamp-based detection (layer 5) ---
        for session in &self.store.sessions {
            let name = &session.tmux_name;
            if let Some(&current_ts) = self.activity_cache.get(name) {
                let prev_ts = self.prev_activity.get(name).copied();
                let changed = prev_ts.map(|p| p != current_ts).unwrap_or(true);

                if changed {
                    self.activity_changed_at.insert(name.clone(), now);
                    let count = self.sustained_activity_count.entry(name.clone()).or_insert(0);
                    *count += 1;
                } else {
                    // No change this tick — reset sustained counter.
                    self.sustained_activity_count.insert(name.clone(), 0);
                }
            }
        }

        // --- Control client management: connect/reconnect to selected session ---
        self.ensure_control_client();

        // --- Refresh preview every tick (~500ms) ---
        self.preview_stale = true;
        self.refresh_preview_content();
        self.last_preview_at = Instant::now();

        // --- Drain any pending control client output ---
        self.check_for_output();

        // --- Per-session 5-layer detection ---
        // Collect session data first to avoid borrow conflicts.
        let session_data: Vec<(String, String, crate::session::instance::Tool)> = self
            .store
            .sessions
            .iter()
            .map(|s| (s.id.clone(), s.tmux_name.clone(), s.tool.clone()))
            .collect();

        // Limit capture_pane subprocess calls to avoid blocking tick too long.
        // When user input is queued, skip detection captures entirely so j/k
        // stays instant.  Statuses will catch up on the next quiet tick.
        let mut capture_budget: u32 =
            if event::poll(Duration::ZERO).unwrap_or(false) { 0 } else { 3 };

        for (session_id, tmux_name, tool) in &session_data {
            // Layer 0: Dead — not in activity cache means no tmux session.
            if !self.activity_cache.contains_key(tmux_name) {
                if let Some(session) = self.store.find_session_mut(session_id) {
                    session.status = Status::Dead;
                }
                continue;
            }

            // Resolve hook status for this session.
            // Hook files are keyed by Claude session UUID, but we also check tmux name.
            let hook_status = self.hook_status_cache.get(tmux_name).cloned()
                .or_else(|| {
                    // Try matching by session id prefix in hook filenames.
                    self.hook_status_cache.iter()
                        .find(|(k, _)| tmux_name.contains(k.as_str()) || k.contains(tmux_name.as_str()))
                        .map(|(_, v)| v.clone())
                });

            // Pane title from batch call.
            let pane_title = self.pane_title_cache.get(tmux_name).map(|s| s.as_str());

            // Smart capture_pane: only fetch content if hooks and title are inconclusive,
            // and we haven't used up our per-tick subprocess budget.
            let needs_content = hook_status.is_none()
                && !pane_title.map(detector::has_braille_spinner).unwrap_or(false);

            let pane_content = if needs_content && capture_budget > 0 {
                capture_budget -= 1;
                tmux::capture_pane(tmux_name).ok()
            } else {
                None
            };

            // Build detection context.
            let ctx = DetectionContext {
                tool: &tool,
                pane_title,
                pane_content: pane_content.as_deref(),
                hook_status,
                activity_changed_at: self.activity_changed_at.get(tmux_name).copied(),
                spinner_last_seen: self.spinner_last_seen.get(tmux_name).copied(),
                sustained_activity_count: self.sustained_activity_count.get(tmux_name).copied().unwrap_or(0),
                now,
            };

            let result = detector::detect_status(&ctx);

            // Update spinner tracking.
            if result.spinner_seen {
                self.spinner_last_seen.insert(tmux_name.clone(), now);
            }

            // Apply detected status.
            if let Some(session) = self.store.find_session_mut(session_id) {
                session.status = result.status;
            }
        }

        // Store current activity as prev for next tick.
        self.prev_activity = self.activity_cache.clone();

        // Token data refresh (every ~5 seconds at 500ms tick interval).
        if self.tick_count % 10 == 0 {
            crate::session::tokens::refresh_all(&mut self.store.sessions, &mut self.token_cache);
        }
    }

    /// Drain pending %output from control client. If any bytes arrived,
    /// mark the preview as stale so it gets re-captured next frame.
    fn check_for_output(&mut self) {
        if let Some(ref client) = self.control_client {
            let bytes = client.drain_output();
            if !bytes.is_empty() {
                self.preview_stale = true;
            }
        }
    }

    /// Ensure the control client is connected to the currently selected session.
    ///
    /// The control client is only kept alive while the right pane is focused
    /// (interactive mode).  Its `refresh-client -C 400x200` resizes the tmux
    /// pane, which distorts `capture-pane` output and prevents the preview
    /// from updating while we stay on the same session.  Dropping it when
    /// focus returns to the left pane restores the original pane size.
    fn ensure_control_client(&mut self) {
        // Only maintain the control client in interactive (right-pane) mode.
        if self.focus != FocusPane::Right {
            if self.control_client.is_some() {
                self.control_client = None;
                self.control_session = None;
                self.preview_stale = true;
            }
            return;
        }

        let selected_tmux = self.selected_session_tmux_name();
        let need_reconnect = match (&selected_tmux, &self.control_session) {
            (Some(sel), Some(cur)) => sel != cur,
            (Some(_), None) => true,
            (None, Some(_)) => true,
            (None, None) => false,
        };

        if need_reconnect {
            self.control_client = None;
            self.control_session = None;
            self.preview_content = None;

            if let Some(ref name) = selected_tmux {
                if self.activity_cache.contains_key(name) {
                    if let Ok(client) = TmuxControlClient::attach(name) {
                        self.control_client = Some(client);
                        self.control_session = Some(name.clone());
                    }
                }
            }

            self.preview_stale = true;
        }

        // Check if control client died — fall back gracefully.
        if let Some(ref mut client) = self.control_client {
            if !client.is_alive() {
                self.control_client = None;
                self.control_session = None;
            }
        }
    }

    /// Capture the selected session's pane content and convert to styled Text.
    fn refresh_preview_content(&mut self) {
        if !self.preview_stale {
            return;
        }
        self.preview_stale = false;

        let name = match self.selected_session_tmux_name() {
            Some(n) => n,
            None => {
                self.preview_content = None;
                return;
            }
        };

        match tmux::capture_pane_ansi(&name) {
            Ok(ansi) => {
                match ansi.as_bytes().into_text() {
                    Ok(mut text) => {
                        // Trim trailing blank lines — the control client's
                        // `refresh-client -C 400x200` can make the pane much
                        // taller than the actual content, producing hundreds
                        // of empty lines that the bottom-anchor would display.
                        while text.lines.last().map_or(false, |line| {
                            line.spans.is_empty()
                                || line.spans.iter().all(|s| s.content.trim().is_empty())
                        }) {
                            text.lines.pop();
                        }
                        self.preview_content = Some(text);
                    }
                    Err(_) => {
                        // ANSI parse failed — show raw text as fallback.
                        let stripped = detector::strip_ansi(&ansi);
                        self.preview_content = Some(Text::raw(stripped));
                    }
                }
            }
            Err(_) => {
                // Capture failed or timed out — keep last known good content.
                // It will retry on the next tick.
            }
        }
    }

    /// Snapshot scrollback content when entering scroll mode, drop it when
    /// returning to live view.  Called after processing scroll events.
    fn update_scroll_cache(&mut self) {
        if self.preview_scroll > 0 && self.scroll_cache.is_none() {
            // Entering scroll mode — capture full scrollback history once.
            if let Some(name) = self.selected_session_tmux_name() {
                if let Ok(content) = tmux::capture_pane_scrollback(&name) {
                    if let Ok(text) = content.as_bytes().into_text() {
                        self.scroll_cache = Some(text);
                    }
                }
            }
        } else if self.preview_scroll == 0 {
            // Back to live view — drop the cache.
            self.scroll_cache = None;
        }
    }

    // --- Cursor helpers ---------------------------------------------------

    fn display_count(&self) -> usize {
        dashboard::display_item_count(
            &self.store.sessions,
            &self.store.groups,
            self.status_filter.as_deref(),
        )
    }

    fn move_cursor_down(&mut self) {
        let count = self.display_count();
        if count > 0 && self.selected < count - 1 {
            self.selected += 1;
            self.preview_scroll = 0;
            self.scroll_cache = None;
            self.preview_stale = true;
        }
    }

    fn move_cursor_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.preview_scroll = 0;
            self.scroll_cache = None;
            self.preview_stale = true;
        }
    }

    fn clamp_cursor(&mut self) {
        let count = self.display_count();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    /// Get the session id of the currently selected item (if it is a session
    /// row, not a group header).
    fn selected_session_id(&self) -> Option<String> {
        // Walk the flattened list to find which item the cursor is on.
        let filtered: Vec<&crate::session::instance::Session> = self
            .store
            .sessions
            .iter()
            .filter(|s| {
                self.status_filter
                    .as_ref()
                    .map(|f| s.status.to_string() == *f)
                    .unwrap_or(true)
            })
            .collect();

        let mut idx: usize = 0;
        for group in &self.store.groups {
            let group_sessions: Vec<&&crate::session::instance::Session> = filtered
                .iter()
                .filter(|s| s.group.as_deref() == Some(&group.name))
                .collect();

            // Group header
            if idx == self.selected {
                return None; // cursor is on a group header
            }
            idx += 1;

            if group.expanded {
                for sess in &group_sessions {
                    if idx == self.selected {
                        return Some(sess.id.clone());
                    }
                    idx += 1;
                }
            }
        }

        // Ungrouped sessions
        let ungrouped: Vec<&&crate::session::instance::Session> = filtered
            .iter()
            .filter(|s| {
                s.group.is_none()
                    || !self
                        .store
                        .groups
                        .iter()
                        .any(|g| Some(g.name.as_str()) == s.group.as_deref())
            })
            .collect();

        if !ungrouped.is_empty() {
            // Ungrouped header
            if idx == self.selected {
                return None;
            }
            idx += 1;

            for sess in &ungrouped {
                if idx == self.selected {
                    return Some(sess.id.clone());
                }
                idx += 1;
            }
        }

        None
    }

    // --- Attach -----------------------------------------------------------

    /// Attach to the selected session's tmux session.
    ///
    /// Leaves the alternate screen, runs `tmux attach` (blocking), then
    /// re-enters the alternate screen.
    fn attach_selected(&mut self) {
        let session_id = match self.selected_session_id() {
            Some(id) => id,
            None => return,
        };

        let tmux_name = match self.store.find_session(&session_id) {
            Some(s) => s.tmux_name.clone(),
            None => return,
        };

        // Check that the tmux session actually exists before trying to attach.
        match tmux::session_exists(&tmux_name) {
            Ok(true) => {}
            _ => return,
        }

        // Leave TUI alternate screen.
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);

        // Attach (blocking).
        let _ = tmux::attach_session(&tmux_name);

        // Re-enter TUI alternate screen and signal that a clear is needed.
        let _ = enable_raw_mode();
        let _ = execute!(stdout(), EnterAlternateScreen);
        self.needs_clear = true;
    }

    // --- Interactive pane: forward keys to tmux ----------------------------

    /// Forward a key event to the currently selected session's tmux pane.
    fn forward_key_to_tmux(&mut self, key: KeyEvent) {
        let tmux_name = match self.selected_session_tmux_name() {
            Some(name) => name,
            None => return,
        };

        // Snap to live view when typing and mark preview stale.
        self.preview_scroll = 0;
        self.scroll_cache = None;
        self.preview_stale = true;
        self.last_keystroke_at = Instant::now();

        match crate::tui::keymap::map_key(&key) {
            crate::tui::keymap::TmuxKey::Literal(text) => {
                // Try control client first (pipe write ~0.07ms), fall back to subprocess (~5.5ms).
                let used_control = if let Some(ref mut client) = self.control_client {
                    client.send_keys_literal(&text).is_ok()
                } else {
                    false
                };
                if !used_control {
                    let _ = tmux::send_keys_raw(&tmux_name, &text);
                }
            }
            crate::tui::keymap::TmuxKey::Special(name) => {
                let used_control = if let Some(ref mut client) = self.control_client {
                    client.send_keys_special(&name).is_ok()
                } else {
                    false
                };
                if !used_control {
                    let _ = tmux::send_keys_special(&tmux_name, &name);
                }
            }
            crate::tui::keymap::TmuxKey::Ignore => {}
        }
    }

    /// Get the tmux session name for the currently selected session.
    fn selected_session_tmux_name(&self) -> Option<String> {
        let session_id = self.selected_session_id()?;
        self.store.find_session(&session_id).map(|s| s.tmux_name.clone())
    }

    // --- Status filter cycling --------------------------------------------

    fn cycle_status_filter(&mut self) {
        self.status_filter = match self.status_filter.as_deref() {
            None => Some("active".to_string()),
            Some("active") => Some("waiting".to_string()),
            Some("waiting") => Some("done".to_string()),
            Some("done") => Some("idle".to_string()),
            Some("idle") => Some("dead".to_string()),
            Some("dead") => None,
            Some(_) => None,
        };
        self.clamp_cursor();
    }

    // --- Group toggle -----------------------------------------------------

    /// Toggle the group that the cursor is currently on.
    /// `expand`: if true, try to expand; if false, try to collapse.
    /// For simplicity we just toggle regardless of the `expand` hint.
    fn toggle_selected_group(&mut self, _expand: bool) {
        // Find the group name at the current cursor position.
        let mut idx: usize = 0;
        for group in &self.store.groups {
            if idx == self.selected {
                let name = group.name.clone();
                self.store.toggle_group(&name);
                self.clamp_cursor();
                return;
            }
            idx += 1;

            if group.expanded {
                let count = self
                    .store
                    .sessions
                    .iter()
                    .filter(|s| {
                        s.group.as_deref() == Some(&group.name)
                            && self
                                .status_filter
                                .as_ref()
                                .map(|f| s.status.to_string() == *f)
                                .unwrap_or(true)
                    })
                    .count();
                idx += count;
            }
        }
    }

    // --- Search mode ---------------------------------------------------------

    /// Handle key events in Search mode.
    fn handle_search_key(&mut self, key: KeyEvent) {
        // Extract current search state to avoid borrow conflicts.
        let (mut query, mut selected) = match &self.mode {
            AppMode::Search {
                query, selected, ..
            } => (query.clone(), *selected),
            _ => return,
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
                return;
            }
            KeyCode::Enter => {
                // Jump the main cursor to the selected search result.
                if let AppMode::Search {
                    filtered_indices, ..
                } = &self.mode
                {
                    if let Some(&real_idx) = filtered_indices.get(selected) {
                        self.selected = real_idx;
                    }
                }
                self.mode = AppMode::Normal;
                return;
            }
            KeyCode::Backspace => {
                query.pop();
            }
            KeyCode::Char(c) => {
                query.push(c);
            }
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
            }
            KeyCode::Down => {
                selected += 1;
            }
            _ => {}
        }

        let filtered_indices = self.compute_search_indices(&query);

        // Clamp selected within filtered results.
        if filtered_indices.is_empty() {
            selected = 0;
        } else if selected >= filtered_indices.len() {
            selected = filtered_indices.len() - 1;
        }

        self.mode = AppMode::Search {
            query,
            filtered_indices,
            selected,
        };
    }

    /// Compute the display-list indices of sessions matching `query`.
    ///
    /// Walks the flattened display list (group headers + session rows) and
    /// returns the indices of session rows whose title or short_path
    /// case-insensitively contain `query`.
    fn compute_search_indices(&self, query: &str) -> Vec<usize> {
        let query_lower = query.to_lowercase();

        let filtered: Vec<&crate::session::instance::Session> = self
            .store
            .sessions
            .iter()
            .filter(|s| {
                self.status_filter
                    .as_ref()
                    .map(|f| s.status.to_string() == *f)
                    .unwrap_or(true)
            })
            .collect();

        let mut indices = Vec::new();
        let mut idx: usize = 0;

        for group in &self.store.groups {
            let group_sessions: Vec<&&crate::session::instance::Session> = filtered
                .iter()
                .filter(|s| s.group.as_deref() == Some(&group.name))
                .collect();

            // Group header row.
            idx += 1;

            if group.expanded {
                for sess in &group_sessions {
                    if query_lower.is_empty()
                        || sess.title.to_lowercase().contains(&query_lower)
                        || sess.short_path().to_lowercase().contains(&query_lower)
                    {
                        indices.push(idx);
                    }
                    idx += 1;
                }
            }
        }

        // Ungrouped sessions.
        let ungrouped: Vec<&&crate::session::instance::Session> = filtered
            .iter()
            .filter(|s| {
                s.group.is_none()
                    || !self
                        .store
                        .groups
                        .iter()
                        .any(|g| Some(g.name.as_str()) == s.group.as_deref())
            })
            .collect();

        if !ungrouped.is_empty() {
            // Ungrouped header row.
            idx += 1;

            for sess in &ungrouped {
                if query_lower.is_empty()
                    || sess.title.to_lowercase().contains(&query_lower)
                    || sess.short_path().to_lowercase().contains(&query_lower)
                {
                    indices.push(idx);
                }
                idx += 1;
            }
        }

        indices
    }

    // --- Group dialog mode ---------------------------------------------------

    /// Handle key events in GroupDialog mode.
    fn handle_group_dialog_key(&mut self, key: KeyEvent) {
        let (mut name, assign_session) = match &self.mode {
            AppMode::GroupDialog {
                name,
                assign_session,
            } => (name.clone(), assign_session.clone()),
            _ => return,
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
                return;
            }
            KeyCode::Enter => {
                if !name.trim().is_empty() {
                    // Create the group.
                    self.store.add_group(Group::new(name.trim().to_string()));

                    // Optionally assign the selected session to this group.
                    if let Some(ref session_id) = assign_session {
                        if let Some(session) = self.store.find_session_mut(session_id) {
                            session.group = Some(name.trim().to_string());
                        }
                    }

                    let _ = self.store.save();
                }
                self.mode = AppMode::Normal;
                return;
            }
            KeyCode::Backspace => {
                name.pop();
            }
            KeyCode::Char(c) => {
                name.push(c);
            }
            _ => {}
        }

        self.mode = AppMode::GroupDialog {
            name,
            assign_session,
        };
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the main TUI event loop until the user quits.
pub fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut app = App::new()?;

    while !app.should_quit {
        // Force a full clear when returning from tmux attach or on first draw.
        if app.needs_clear {
            terminal.clear()?;
            app.needs_clear = false;
        }

        terminal.draw(|frame| app.draw(frame))?;

        // Adaptive poll rate based on recent activity:
        // - 16ms (~60fps) during active typing (last keystroke < 500ms ago)
        // - 50ms when recent activity (< 2s ago)
        // - 100ms when right pane focused (interactive)
        // - 250ms otherwise (list browsing, ~4fps for dot-flash animation)
        let since_keystroke = Instant::now().duration_since(app.last_keystroke_at);
        let poll_timeout = if since_keystroke < Duration::from_millis(500) {
            Duration::from_millis(16)
        } else if since_keystroke < Duration::from_secs(2) {
            Duration::from_millis(50)
        } else if app.focus == FocusPane::Right {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(250)
        };

        // Drain ALL pending events before redrawing — avoids a full redraw
        // between each keystroke during fast typing.
        if event::poll(poll_timeout)? {
            loop {
                match event::read()? {
                    Event::Key(key) => app.handle_key(key),
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            // Scroll up = into history = increase offset.
                            app.preview_scroll = app.preview_scroll.saturating_add(3);
                        }
                        MouseEventKind::ScrollDown => {
                            // Scroll down = toward live = decrease offset.
                            app.preview_scroll = app.preview_scroll.saturating_sub(3);
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {
                        terminal.clear()?;
                    }
                    _ => {}
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            // Snapshot/drop scroll cache based on current scroll position.
            app.update_scroll_cache();
            // Reconnect control client once after all events are drained.
            app.ensure_control_client();
            // Preview refresh is handled by tick (~500ms) to keep j/k instant.
        }

        // Throttle tick to ~500ms regardless of poll rate.
        // Skip tick if more input is already queued — user input always wins.
        let now = Instant::now();
        if now.duration_since(app.last_tick) >= Duration::from_millis(500)
            && !event::poll(Duration::ZERO)?
        {
            app.tick();
            app.last_tick = now;
        }
    }

    Ok(())
}
