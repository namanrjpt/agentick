use std::collections::{HashMap, HashSet};
use std::io::stdout;
use std::path::PathBuf;
use std::thread::JoinHandle;
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

use crate::session::instance::Status;
use crate::session::store::SessionStore;
use crate::tmux::client as tmux;
use crate::tmux::control::TmuxControlClient;
use crate::tmux::detector::{self, DetectionContext, HookStatus};
use crate::tui::views::dashboard;
use crate::tui::views::dashboard::InlineNewRenderState;
use crate::tui::views::new_session::{self, DialogAction, NewSessionDialog};
use crate::tui::views::search;
use crate::tui::zoxide::{self, ZoxideEntry};

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

/// Which step of the inline new-session flow we're on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InlineNewStep {
    /// Typing a fuzzy directory search query.
    DirSearch,
    /// Directory chosen — pick a tool via quick-create keys.
    ToolPick,
}

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
    QuickCreate {
        project_path: PathBuf,
    },
    Rename {
        session_id: String,
        buf: String,
    },
    InlineNew {
        query: String,
        zoxide_dirs: Vec<ZoxideEntry>,
        dir_selected: usize,
        step: InlineNewStep,
        project_path: Option<PathBuf>,
    },
}

// ---------------------------------------------------------------------------
// Key normalisation
// ---------------------------------------------------------------------------

/// Normalise Kitty-protocol key events: Shift+lowercase → uppercase.
///
/// The Kitty keyboard protocol (`REPORT_ALL_KEYS_AS_ESCAPE_CODES`) reports
/// Shift+N as `Char('n')` with `SHIFT` modifier instead of `Char('N')`.
/// This converts to the traditional representation so match arms on uppercase
/// characters (e.g. `KeyCode::Char('N')`) work regardless of protocol.
fn normalize_shift_char(mut key: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(c) = key.code {
        if c.is_ascii_lowercase() && key.modifiers.contains(KeyModifiers::SHIFT) {
            key.code = KeyCode::Char(c.to_ascii_uppercase());
            key.modifiers.remove(KeyModifiers::SHIFT);
        }
    }
    key
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
    /// Background thread capturing scrollback (so it doesn't block the event loop).
    scroll_capture_handle: Option<JoinHandle<Option<String>>>,
    /// Styled preview content from capture-pane, refreshed on activity.
    preview_content: Option<Text<'static>>,
    /// Set when preview needs re-capture (output detected, cursor moved, etc).
    preview_stale: bool,
    /// Cache for token file parsing — avoids re-reading unchanged JSONL files.
    token_cache: crate::session::tokens::TokenCache,
    /// Project paths whose auto-groups are collapsed (not persisted).
    collapsed_dirs: HashSet<String>,
    /// Background threads generating LLM summaries for Claude session titles.
    summary_handles: HashMap<String, JoinHandle<(String, Option<String>)>>,
    /// User config loaded from ~/.agentick/config.toml.
    config: crate::config::Config,
    /// Preview pane inner dimensions (cols, rows) from last draw — used to
    /// resize tmux panes so `capture-pane` returns content that fits.
    preview_size: Option<(u16, u16)>,
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
        let config = crate::config::Config::load();
        let mut app = Self {
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
            scroll_capture_handle: None,
            preview_content: None,
            preview_stale: true,
            token_cache,
            collapsed_dirs: HashSet::new(),
            summary_handles: HashMap::new(),
            config,
            preview_size: None,
        };
        app.spawn_summary_threads();
        // Run the first tick immediately so statuses are detected before the
        // first frame is drawn, rather than waiting 500ms.
        app.tick();
        Ok(app)
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

        // Compute preview pane inner size for tmux pane resizing.
        if area.width > 100 {
            // Layout: top bar(3) + main(rest) + help(1). Main splits 25%/75%.
            let main_height = area.height.saturating_sub(4); // 3 top + 1 help
            let preview_width = (area.width as u32 * 75 / 100) as u16;
            // Inner = total - 2 (borders on each side)
            let inner_w = preview_width.saturating_sub(2);
            let inner_h = main_height.saturating_sub(2);
            if inner_w > 0 && inner_h > 0 {
                self.preview_size = Some((inner_w, inner_h));
            }
        } else {
            self.preview_size = None;
        }

        // Extract rename state for dashboard rendering.
        let rename_state = match &self.mode {
            AppMode::Rename { session_id, buf } => Some((session_id.as_str(), buf.as_str())),
            _ => None,
        };

        // Extract inline-new state for dashboard rendering.
        let inline_new = match &self.mode {
            AppMode::InlineNew {
                query,
                zoxide_dirs,
                dir_selected,
                step,
                ..
            } => {
                let suggestions: Vec<(String, f64)> = if *step == InlineNewStep::DirSearch {
                    zoxide::fuzzy_filter(zoxide_dirs, query, 5)
                        .into_iter()
                        .map(|e| (e.path.clone(), e.score))
                        .collect()
                } else {
                    Vec::new()
                };
                Some(InlineNewRenderState {
                    query,
                    suggestions,
                    dir_selected: *dir_selected,
                    is_dir_search: *step == InlineNewStep::DirSearch,
                })
            }
            _ => None,
        };

        // Extract search highlight indices for the dashboard.
        let search_matches: HashSet<usize> = match &self.mode {
            AppMode::Search { filtered_indices, .. } => {
                filtered_indices.iter().copied().collect()
            }
            _ => HashSet::new(),
        };

        // Always render the dashboard underneath.
        dashboard::render_dashboard(
            frame,
            &self.store.sessions,
            &self.collapsed_dirs,
            self.selected,
            self.status_filter.as_deref(),
            self.preview_content.as_ref(),
            self.scroll_cache.as_ref(),
            self.preview_scroll,
            self.focus,
            self.frame_count,
            area,
            rename_state,
            inline_new.as_ref(),
            &search_matches,
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
            AppMode::QuickCreate { project_path } => {
                dashboard::render_quick_create_sheet(frame, project_path, area);
            }
            AppMode::InlineNew {
                step: InlineNewStep::ToolPick,
                project_path: Some(path),
                ..
            } => {
                dashboard::render_quick_create_sheet(frame, path, area);
            }
            AppMode::Normal | AppMode::Rename { .. } | AppMode::InlineNew { .. } => {}
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
                let new_focus = match self.focus {
                    FocusPane::Left => FocusPane::Right,
                    FocusPane::Right => FocusPane::Left,
                };
                // Restore full terminal size when entering interactive mode
                // (preview mode shrinks the pane to fit).
                if new_focus == FocusPane::Right {
                    if let Some(name) = self.selected_session_tmux_name() {
                        let h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(40);
                        let _ = tmux::resize_window(&name, self.terminal_width, h);
                    }
                }
                self.focus = new_focus;
            }
            return;
        }

        // When right pane is focused in Normal mode, forward all keys to tmux.
        // Use the raw (un-normalised) key so keymap.rs sees the original event.
        if self.focus == FocusPane::Right && matches!(self.mode, AppMode::Normal) {
            self.forward_key_to_tmux(key);
            return;
        }

        // Normalise Kitty-protocol key events for TUI handling.
        // The Kitty keyboard protocol (REPORT_ALL_KEYS_AS_ESCAPE_CODES) reports
        // Shift+N as Char('n') + SHIFT instead of Char('N'). Normalise so that
        // match arms on uppercase characters work correctly.
        let key = normalize_shift_char(key);

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
                        let session_id = session.id.clone();
                        self.store.add_session(session);
                        let _ = self.store.save();
                        // Auto-select the newly created session.
                        self.select_session_by_id(&session_id);
                    }
                }
            }
            return;
        }

        match &self.mode {
            AppMode::Normal => self.handle_normal_key(key),
            AppMode::NewSession(_) => unreachable!(),
            AppMode::QuickCreate { .. } => {
                self.handle_quick_create_key(key);
            }
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
            AppMode::Rename { .. } => {
                self.handle_rename_key(key);
            }
            AppMode::InlineNew { .. } => {
                self.handle_inline_new_key(key);
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
                if let Some(path) = self.selected_project_path() {
                    self.mode = AppMode::QuickCreate { project_path: path };
                }
            }
            KeyCode::Char('N') => {
                let dirs = zoxide::load_zoxide_dirs();
                self.mode = AppMode::InlineNew {
                    query: String::new(),
                    zoxide_dirs: dirs,
                    dir_selected: 0,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
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
                self.mode = AppMode::Search {
                    query: String::new(),
                    filtered_indices: Vec::new(),
                    selected: 0,
                };
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.toggle_selected_group(false);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.toggle_selected_group(true);
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.selected_session_id() {
                    self.mode = AppMode::Rename {
                        session_id: id,
                        buf: String::new(),
                    };
                }
            }
            KeyCode::Char('R') => {
                self.preview_stale = true;
                self.scroll_cache = None;
                self.scroll_capture_handle = None;
                self.preview_scroll = 0;
                self.refresh_preview_content();
            }
            KeyCode::Char('f') => {
                self.fork_selected_session();
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
                // First observation (prev_ts is None) is NOT a change — treat
                // as baseline so sessions start as Idle, not Done.
                let changed = prev_ts.map(|p| p != current_ts).unwrap_or(false);

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

        // Poll completed LLM summary threads.
        let done_ids: Vec<String> = self
            .summary_handles
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in done_ids {
            if let Some(handle) = self.summary_handles.remove(&id) {
                if let Ok((sid, Some(summary))) = handle.join() {
                    if let Some(session) = self.store.find_session_mut(&sid) {
                        if !session.user_renamed {
                            session.title = summary;
                        }
                    }
                    let _ = self.store.save();
                }
            }
        }

        // Fast check: fill empty Claude titles with first user prompt (~5s cadence).
        if self.tick_count % 10 == 0 {
            self.fill_empty_titles();
        }

        // Spawn new LLM summary threads every ~2.5 min.
        if self.tick_count % 300 == 0 {
            self.spawn_summary_threads();
        }
    }

    /// Spawn background threads to generate LLM summary titles for Claude sessions.
    ///
    /// Only spawns for sessions that don't already have a pending handle.
    /// For Claude sessions with empty titles, immediately set the first user
    /// prompt as a placeholder and spawn an LLM thread for a proper title.
    /// Try to fill the title of the currently hovered session only.
    ///
    /// Sets the first user message as a placeholder, then spawns an LLM
    /// thread for a proper summary title.
    fn fill_empty_titles(&mut self) {
        use crate::session::tokens::{
            collect_context_for_tool, extract_first_user_message_for_tool,
            generate_llm_summary, supports_auto_title,
        };

        let session = match self.selected_session_id() {
            Some(id) => match self.store.sessions.iter().find(|s| s.id == id) {
                Some(s) if supports_auto_title(&s.tool) && s.title.is_empty() && !s.user_renamed => s.clone(),
                _ => return,
            },
            None => return,
        };

        // Set first user message as placeholder title.
        if let Some(msg) = extract_first_user_message_for_tool(&session) {
            if let Some(s) = self.store.find_session_mut(&session.id) {
                s.title = msg;
            }
        }

        // Spawn LLM thread for a proper summary.
        let session_id = session.id.clone();
        if self.summary_handles.contains_key(&session_id) {
            return;
        }
        let sid = session_id.clone();
        let handle = std::thread::spawn(move || {
            let result = collect_context_for_tool(&session)
                .and_then(|ctx| generate_llm_summary(&ctx));
            (sid, result)
        });
        self.summary_handles.insert(session_id, handle);
    }

    /// Spawn LLM summary threads for all Claude sessions to refresh titles.
    /// Spawn an LLM summary thread for the currently hovered session only.
    fn spawn_summary_threads(&mut self) {
        use crate::session::tokens::{collect_context_for_tool, generate_llm_summary, supports_auto_title};

        let session = match self.selected_session_id() {
            Some(id) => match self.store.sessions.iter().find(|s| s.id == id) {
                Some(s) if supports_auto_title(&s.tool) && !s.user_renamed && !self.summary_handles.contains_key(&s.id) => s.clone(),
                _ => return,
            },
            None => return,
        };

        let session_id = session.id.clone();
        let sid = session_id.clone();
        let handle = std::thread::spawn(move || {
            let result = collect_context_for_tool(&session)
                .and_then(|ctx| generate_llm_summary(&ctx));
            (sid, result)
        });
        self.summary_handles.insert(session_id, handle);
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

        // Resize the tmux pane to match the preview width so capture-pane
        // returns content that fits.  Only in preview mode (left pane focused);
        // interactive mode uses the control client's refresh-client -C instead.
        if self.focus == FocusPane::Left {
            if let Some((cols, rows)) = self.preview_size {
                let _ = tmux::resize_window(&name, cols, rows);
            }
        }

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
    ///
    /// The actual `capture-pane -S` call runs in a background thread so it
    /// never blocks the event loop.  Until the capture completes the preview
    /// keeps showing live content; once it arrives the scroll_cache is
    /// populated and scroll mode kicks in.
    fn update_scroll_cache(&mut self) {
        if self.preview_scroll == 0 {
            // Back to live view — drop everything.
            self.scroll_cache = None;
            self.scroll_capture_handle = None;
            return;
        }

        // If the cache is already populated, just clamp the scroll position.
        if let Some(ref text) = self.scroll_cache {
            let total = text.lines.len();
            if total > 0 && self.preview_scroll >= total {
                self.preview_scroll = total - 1;
            }
            return;
        }

        // Check if a background capture has finished.
        if let Some(handle) = self.scroll_capture_handle.take() {
            if handle.is_finished() {
                if let Ok(Some(content)) = handle.join() {
                    if let Ok(text) = content.as_bytes().into_text() {
                        self.scroll_cache = Some(text);
                    }
                }
                // Clamp after populating.
                if let Some(ref text) = self.scroll_cache {
                    let total = text.lines.len();
                    if total > 0 && self.preview_scroll >= total {
                        self.preview_scroll = total - 1;
                    }
                }
            } else {
                // Still running — put the handle back.
                self.scroll_capture_handle = Some(handle);
            }
            return;
        }

        // No cache and no pending capture — kick off a background capture.
        if let Some(name) = self.selected_session_tmux_name() {
            let handle = std::thread::spawn(move || {
                tmux::capture_pane_scrollback(&name).ok()
            });
            self.scroll_capture_handle = Some(handle);
        }
    }

    // --- Cursor helpers ---------------------------------------------------

    fn display_count(&self) -> usize {
        dashboard::display_item_count(
            &self.store.sessions,
            &self.collapsed_dirs,
            self.status_filter.as_deref(),
        )
    }

    fn move_cursor_down(&mut self) {
        let count = self.display_count();
        if count > 0 && self.selected < count - 1 {
            self.selected += 1;
            self.preview_scroll = 0;
            self.scroll_cache = None;
            self.scroll_capture_handle = None;
            self.preview_stale = true;
        }
    }

    fn move_cursor_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.preview_scroll = 0;
            self.scroll_cache = None;
            self.scroll_capture_handle = None;
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
        dashboard::find_selected_session(
            &self.store.sessions,
            &self.collapsed_dirs,
            self.selected,
            self.status_filter.as_deref(),
        )
        .map(|s| s.id.clone())
    }

    /// Get the project path of the currently selected item, whether it's a
    /// group header or a session row.
    fn selected_project_path(&self) -> Option<PathBuf> {
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

        let mut seen = HashSet::new();
        let mut paths: Vec<String> = Vec::new();
        for s in &filtered {
            let key = s.project_path.to_string_lossy().to_string();
            if seen.insert(key.clone()) {
                paths.push(key);
            }
        }

        let mut idx: usize = 0;
        for path_key in &paths {
            let group_sessions: Vec<&&crate::session::instance::Session> = filtered
                .iter()
                .filter(|s| s.project_path.to_string_lossy() == path_key.as_str())
                .collect();

            let expanded = !self.collapsed_dirs.contains(path_key);

            // Group header
            if idx == self.selected {
                return Some(PathBuf::from(path_key));
            }
            idx += 1;

            if expanded {
                for sess in &group_sessions {
                    if idx == self.selected {
                        return Some(sess.project_path.clone());
                    }
                    idx += 1;
                }
            }
        }

        None
    }

    /// Handle key events in QuickCreate mode.
    fn handle_quick_create_key(&mut self, key: KeyEvent) {
        // N transitions to full dialog
        if key.code == KeyCode::Char('N') {
            self.mode = AppMode::NewSession(Box::new(NewSessionDialog::new()));
            return;
        }

        // Esc cancels
        if key.code == KeyCode::Esc {
            self.mode = AppMode::Normal;
            return;
        }

        let ch = match key.code {
            KeyCode::Char(c) => c,
            _ => {
                self.mode = AppMode::Normal;
                return;
            }
        };

        // Look up the pressed char in QUICK_CREATE_KEYS
        let entry = dashboard::QUICK_CREATE_KEYS
            .iter()
            .find(|(k, _, _)| *k == ch);

        let (_key, _label, cmd) = match entry {
            Some(e) => e,
            None => {
                // Unmapped key — cancel
                self.mode = AppMode::Normal;
                return;
            }
        };

        // Extract project_path before overwriting mode
        let project_path = match &self.mode {
            AppMode::QuickCreate { project_path } => project_path.clone(),
            _ => unreachable!(),
        };

        self.mode = AppMode::Normal;
        self.create_session_and_select(project_path, cmd);
    }

    // --- Inline rename ------------------------------------------------------

    fn handle_rename_key(&mut self, key: KeyEvent) {
        let (session_id, buf) = match &mut self.mode {
            AppMode::Rename { session_id, buf } => (session_id.clone(), buf),
            _ => return,
        };

        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
            }
            KeyCode::Enter => {
                let new_title = buf.trim().to_string();
                if let Some(session) = self.store.find_session_mut(&session_id) {
                    session.title = new_title;
                    session.user_renamed = true;
                }
                let _ = self.store.save();
                self.mode = AppMode::Normal;
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => {
                buf.push(c);
            }
            _ => {}
        }
    }

    // --- Inline new session -------------------------------------------------

    /// Create a new session with the given tool and project path, add it to the
    /// store, and move the cursor to select it.
    fn create_session_and_select(&mut self, project_path: PathBuf, cmd: &str) {
        let tool = crate::session::instance::Tool::from_command(cmd);
        let session =
            crate::session::instance::Session::new(String::new(), project_path.clone(), tool);
        let _ = tmux::create_session(&session.tmux_name, &session.project_path, &session.command);
        let session_id = session.id.clone();
        self.store.add_session(session);
        let _ = self.store.save();

        // Select the newly created session by finding its index in the display list.
        self.select_session_by_id(&session_id);
    }

    /// Move the cursor to the session with the given id.
    fn select_session_by_id(&mut self, target_id: &str) {
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

        let mut seen = HashSet::new();
        let mut paths: Vec<String> = Vec::new();
        for s in &filtered {
            let key = s.project_path.to_string_lossy().to_string();
            if seen.insert(key.clone()) {
                paths.push(key);
            }
        }

        let mut idx: usize = 0;
        for path_key in &paths {
            let group_sessions: Vec<&&crate::session::instance::Session> = filtered
                .iter()
                .filter(|s| s.project_path.to_string_lossy() == path_key.as_str())
                .collect();

            let expanded = !self.collapsed_dirs.contains(path_key);

            // Group header
            idx += 1;

            if expanded {
                for sess in &group_sessions {
                    if sess.id == target_id {
                        self.selected = idx;
                        self.preview_stale = true;
                        return;
                    }
                    idx += 1;
                }
            }
        }
    }

    fn handle_inline_new_key(&mut self, key: KeyEvent) {
        // Extract fields to avoid borrow conflicts.
        let (query, zoxide_dirs, dir_selected, step, project_path) = match &mut self.mode {
            AppMode::InlineNew {
                query,
                zoxide_dirs,
                dir_selected,
                step,
                project_path,
            } => (
                query.clone(),
                std::mem::take(zoxide_dirs),
                *dir_selected,
                *step,
                project_path.clone(),
            ),
            _ => return,
        };

        match step {
            InlineNewStep::DirSearch => {
                self.handle_inline_dir_search(key, query, zoxide_dirs, dir_selected);
            }
            InlineNewStep::ToolPick => {
                self.handle_inline_tool_pick(key, query, zoxide_dirs, dir_selected, project_path);
            }
        }
    }

    fn handle_inline_dir_search(
        &mut self,
        key: KeyEvent,
        mut query: String,
        zoxide_dirs: Vec<ZoxideEntry>,
        mut dir_selected: usize,
    ) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
            }
            KeyCode::Enter => {
                // Confirm the selected directory.
                let filtered = zoxide::fuzzy_filter(&zoxide_dirs, &query, 5);
                let path = if let Some(entry) = filtered.get(dir_selected) {
                    PathBuf::from(&entry.path)
                } else if !query.is_empty() {
                    // Treat raw input as a path.
                    PathBuf::from(&query)
                } else {
                    // Nothing to confirm.
                    self.mode = AppMode::InlineNew {
                        query,
                        zoxide_dirs,
                        dir_selected,
                        step: InlineNewStep::DirSearch,
                        project_path: None,
                    };
                    return;
                };

                // If auto_create_tool is configured, create immediately and select.
                if let Some(ref tool_cmd) = self.config.auto_create_tool.clone() {
                    self.mode = AppMode::Normal;
                    self.create_session_and_select(path, tool_cmd);
                } else {
                    self.mode = AppMode::InlineNew {
                        query,
                        zoxide_dirs,
                        dir_selected,
                        step: InlineNewStep::ToolPick,
                        project_path: Some(path),
                    };
                }
            }
            KeyCode::Up => {
                dir_selected = dir_selected.saturating_sub(1);
                self.mode = AppMode::InlineNew {
                    query,
                    zoxide_dirs,
                    dir_selected,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
            }
            KeyCode::Down => {
                let max = zoxide::fuzzy_filter(&zoxide_dirs, &query, 5)
                    .len()
                    .saturating_sub(1);
                if dir_selected < max {
                    dir_selected += 1;
                }
                self.mode = AppMode::InlineNew {
                    query,
                    zoxide_dirs,
                    dir_selected,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
            }
            KeyCode::Backspace => {
                query.pop();
                dir_selected = 0;
                self.mode = AppMode::InlineNew {
                    query,
                    zoxide_dirs,
                    dir_selected,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
            }
            KeyCode::Char(c) => {
                query.push(c);
                dir_selected = 0;
                self.mode = AppMode::InlineNew {
                    query,
                    zoxide_dirs,
                    dir_selected,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
            }
            _ => {
                self.mode = AppMode::InlineNew {
                    query,
                    zoxide_dirs,
                    dir_selected,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
            }
        }
    }

    fn handle_inline_tool_pick(
        &mut self,
        key: KeyEvent,
        query: String,
        zoxide_dirs: Vec<ZoxideEntry>,
        dir_selected: usize,
        project_path: Option<PathBuf>,
    ) {
        match key.code {
            KeyCode::Esc => {
                // Back to DirSearch.
                self.mode = AppMode::InlineNew {
                    query,
                    zoxide_dirs,
                    dir_selected,
                    step: InlineNewStep::DirSearch,
                    project_path: None,
                };
            }
            KeyCode::Char('N') => {
                // Full dialog.
                self.mode = AppMode::NewSession(Box::new(NewSessionDialog::new()));
            }
            KeyCode::Char(ch) => {
                let entry = dashboard::QUICK_CREATE_KEYS
                    .iter()
                    .find(|(k, _, _)| *k == ch);

                let (_key, _label, cmd) = match entry {
                    Some(e) => e,
                    None => {
                        // Unmapped key — back to normal.
                        self.mode = AppMode::Normal;
                        return;
                    }
                };

                let path = match project_path {
                    Some(p) => p,
                    None => {
                        self.mode = AppMode::Normal;
                        return;
                    }
                };

                self.mode = AppMode::Normal;
                self.create_session_and_select(path, cmd);
            }
            _ => {
                self.mode = AppMode::Normal;
            }
        }
    }

    // --- Fork --------------------------------------------------------------

    /// Fork the currently selected Claude session.
    ///
    /// Uses `claude --resume <uuid> --fork-session` to create a new Claude
    /// session that inherits the parent's full conversation history.
    fn fork_selected_session(&mut self) {
        use crate::session::instance::Tool;
        use crate::session::tokens::find_claude_jsonl;

        let session_id = match self.selected_session_id() {
            Some(id) => id,
            None => return,
        };

        let parent = match self.store.find_session(&session_id) {
            Some(s) => s.clone(),
            None => return,
        };

        // Only Claude sessions can be forked.
        if parent.tool != Tool::Claude {
            return;
        }

        // Find the Claude JSONL file — its filename stem is the Claude session UUID.
        let claude_uuid = match find_claude_jsonl(&parent) {
            Some(path) => match path.file_stem().and_then(|s| s.to_str()) {
                Some(stem) => stem.to_string(),
                None => return,
            },
            None => return,
        };

        let mut forked = crate::session::instance::Session::new_fork(&parent);
        forked.command = format!(
            "claude --resume {} --fork-session --dangerously-skip-permissions",
            claude_uuid
        );

        // Spawn the tmux session for the fork.
        let _ = tmux::create_session(&forked.tmux_name, &forked.project_path, &forked.command);

        self.store.add_session(forked);
        let _ = self.store.save();
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

        // Ensure the status bar is hidden (covers sessions created before this fix).
        let _ = tmux::set_option(&tmux_name, "status", "off");

        // Restore full terminal size (preview mode shrinks the pane to fit).
        let _ = tmux::resize_window(&tmux_name, self.terminal_width, crossterm::terminal::size().map(|(_, h)| h).unwrap_or(40));

        // Leave TUI alternate screen and disable mouse capture, but keep
        // keyboard enhancement active so the outer terminal keeps sending
        // modifier info (Kitty/xterm protocol). tmux needs this to detect
        // Shift+Enter and re-encode it as CSI u for Claude.
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            crossterm::event::DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0)
        );

        // Attach (blocking).
        let _ = tmux::attach_session(&tmux_name);

        // Re-enter TUI alternate screen and re-enable mouse capture.
        // Keyboard enhancement was never popped so no push needed.
        let _ = enable_raw_mode();
        let _ = execute!(
            stdout(),
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
        );
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
        self.scroll_capture_handle = None;
        self.preview_stale = true;
        self.last_keystroke_at = Instant::now();

        // Verify the control client is alive before attempting to use it.
        // Without this check, a dead client silently swallows keystrokes
        // until the next tick() (up to 500ms) detects the disconnect.
        if let Some(ref mut client) = self.control_client {
            if !client.is_alive() {
                self.control_client = None;
                self.control_session = None;
            }
        }

        // If the control client is gone, try to reconnect immediately so the
        // current keystroke can use the fast pipe path instead of subprocess.
        if self.control_client.is_none() && self.focus == FocusPane::Right {
            if self.activity_cache.contains_key(&tmux_name) {
                if let Ok(client) = TmuxControlClient::attach(&tmux_name) {
                    self.control_client = Some(client);
                    self.control_session = Some(tmux_name.clone());
                }
            }
        }

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
            crate::tui::keymap::TmuxKey::RawHex(hex) => {
                let used_control = if let Some(ref mut client) = self.control_client {
                    client.send_keys_hex(&hex).is_ok()
                } else {
                    false
                };
                if !used_control {
                    let _ = tmux::send_keys_hex(&tmux_name, &hex);
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

    // --- Group toggle (auto-groups by project_path) -----------------------

    /// Toggle the directory group that the cursor is currently on.
    /// `expand`: if true, try to expand; if false, try to collapse.
    fn toggle_selected_group(&mut self, _expand: bool) {
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

        // Collect unique paths in order.
        let mut seen = HashSet::new();
        let mut paths: Vec<String> = Vec::new();
        for s in &filtered {
            let key = s.project_path.to_string_lossy().to_string();
            if seen.insert(key.clone()) {
                paths.push(key);
            }
        }

        let mut idx: usize = 0;
        for path_key in &paths {
            if idx == self.selected {
                // Toggle this path's collapsed state.
                if self.collapsed_dirs.contains(path_key) {
                    self.collapsed_dirs.remove(path_key);
                } else {
                    self.collapsed_dirs.insert(path_key.clone());
                }
                self.clamp_cursor();
                return;
            }
            idx += 1;

            let expanded = !self.collapsed_dirs.contains(path_key);
            if expanded {
                let count = filtered
                    .iter()
                    .filter(|s| s.project_path.to_string_lossy() == path_key.as_str())
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

        // Move the main cursor to the currently selected match in real-time.
        if let Some(&real_idx) = filtered_indices.get(selected) {
            self.selected = real_idx;
            self.preview_stale = true;
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
        if query.is_empty() {
            return Vec::new();
        }
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

        // Collect unique paths in order.
        let mut seen = HashSet::new();
        let mut paths: Vec<String> = Vec::new();
        for s in &filtered {
            let key = s.project_path.to_string_lossy().to_string();
            if seen.insert(key.clone()) {
                paths.push(key);
            }
        }

        let mut indices = Vec::new();
        let mut idx: usize = 0;

        for path_key in &paths {
            let group_sessions: Vec<&&crate::session::instance::Session> = filtered
                .iter()
                .filter(|s| s.project_path.to_string_lossy() == path_key.as_str())
                .collect();

            // Group header row.
            idx += 1;

            let expanded = !self.collapsed_dirs.contains(path_key);
            if expanded {
                for sess in &group_sessions {
                    if sess.title.to_lowercase().contains(&query_lower)
                        || sess.short_path().to_lowercase().contains(&query_lower)
                    {
                        indices.push(idx);
                    }
                    idx += 1;
                }
            }
        }

        indices
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
        let now = Instant::now();
        let since_keystroke = now.duration_since(app.last_keystroke_at);
        let poll_duration = if since_keystroke < Duration::from_millis(500) {
            Duration::from_millis(16)
        } else if since_keystroke < Duration::from_secs(2) {
            Duration::from_millis(50)
        } else if app.focus == FocusPane::Right {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(250)
        };

        // Tick-based status refresh: ~500ms.
        if app.last_tick.elapsed() >= Duration::from_millis(500) {
            app.tick();
            app.last_tick = Instant::now();
        }

        // Check if a background scrollback capture has finished.
        if app.scroll_capture_handle.is_some() {
            app.update_scroll_cache();
        }

        if event::poll(poll_duration)? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key),
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            if app.focus == FocusPane::Right {
                                app.preview_scroll += 3;
                                app.update_scroll_cache();
                            } else {
                                app.move_cursor_up();
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if app.focus == FocusPane::Right {
                                app.preview_scroll = app.preview_scroll.saturating_sub(3);
                                app.update_scroll_cache();
                            } else {
                                app.move_cursor_down();
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}
