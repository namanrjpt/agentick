use std::collections::HashSet;
use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use std::path::PathBuf;

use crate::session::instance::{Session, Status};
use crate::tui::app::FocusPane;
use crate::tui::theme::{dark_theme, status_color, tool_color, Theme};

// ---------------------------------------------------------------------------
// Inline new-session render state (passed from app.rs)
// ---------------------------------------------------------------------------

/// Data needed to render the inline new-session input in the session list.
pub struct InlineNewRenderState<'a> {
    pub query: &'a str,
    pub suggestions: Vec<(String, f64)>, // (path, score)
    pub dir_selected: usize,
    pub is_dir_search: bool,
}

// ---------------------------------------------------------------------------
// Quick-create key map
// ---------------------------------------------------------------------------

/// (key, display_label, command_name) for the which-key quick-create sheet.
pub const QUICK_CREATE_KEYS: &[(char, &str, &str)] = &[
    ('c', "Claude", "claude"),
    ('x', "Codex", "codex"),
    ('g', "Gemini", "gemini"),
    ('r', "Cursor", "cursor"),
    ('v', "Vibe", "vibe"),
    ('a', "Aider", "aider"),
    ('s', "Shell", "shell"),
    ('o', "OpenCode", "opencode"),
];

// ---------------------------------------------------------------------------
// Display item -- either a group header or a session row in the flat list
// ---------------------------------------------------------------------------

enum DisplayItem<'a> {
    GroupHeader {
        name: String,
        count: usize,
        expanded: bool,
        branch: Option<String>,
    },
    SessionRow {
        session: &'a Session,
        is_last: bool,
        is_fork: bool,
        /// For fork rows: whether the parent's tree continues (more top-level
        /// sessions follow), so we draw a │ continuation line.
        parent_continues: bool,
    },
}

// ---------------------------------------------------------------------------
// Public render entry point
// ---------------------------------------------------------------------------

/// Render the main dashboard into `area`.
///
/// * `sessions`       -- all sessions (will be filtered by `status_filter`).
/// * `collapsed_dirs` -- set of project paths whose groups are collapsed.
/// * `selected`       -- cursor index in the flattened display list.
/// * `status_filter`  -- if `Some`, only sessions with this status are shown.
pub fn render_dashboard(
    frame: &mut Frame,
    sessions: &[Session],
    collapsed_dirs: &HashSet<String>,
    selected: usize,
    status_filter: Option<&str>,
    preview_content: Option<&Text<'static>>,
    scroll_cache: Option<&Text<'static>>,
    preview_scroll: usize,
    focus: FocusPane,
    tick_count: u32,
    area: Rect,
    rename_state: Option<(&str, &str)>, // (session_id, buffer)
    inline_new: Option<&InlineNewRenderState<'_>>,
    search_matches: &HashSet<usize>,
    confirm_delete_id: Option<&str>,
) {
    let theme = dark_theme();

    // --- Layout: top bar | main list | bottom help bar --------------------

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // top bar
            Constraint::Min(1),   // session list
            Constraint::Length(1), // help bar
        ])
        .split(area);

    // --- Top bar ----------------------------------------------------------

    render_top_bar(frame, sessions, status_filter, chunks[0], &theme);

    // --- Build flattened display list -------------------------------------

    let filtered: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            status_filter
                .map(|f| s.status.to_string() == f)
                .unwrap_or(true)
        })
        .collect();

    // Derive stable group ordering from the full (unfiltered) session list so
    // that folders don't jump around when sessions change status or filters apply.
    let canonical_paths = canonical_group_order(sessions);
    let items = build_display_items(&filtered, collapsed_dirs, &canonical_paths);

    // --- Render session list (with optional preview pane) -----------------

    if area.width > 100 {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(75),
            ])
            .split(chunks[1]);

        render_session_list(frame, &items, selected, tick_count, h_chunks[0], &theme, rename_state, inline_new, search_matches, confirm_delete_id);
        render_preview_pane(
            frame, sessions, collapsed_dirs, selected, status_filter, preview_content,
            scroll_cache, preview_scroll, focus, h_chunks[1], &theme,
        );
    } else {
        render_session_list(frame, &items, selected, tick_count, chunks[1], &theme, rename_state, inline_new, search_matches, confirm_delete_id);
    }

    // --- Bottom help bar --------------------------------------------------

    render_help_bar(frame, focus, chunks[2], &theme);
}

// ---------------------------------------------------------------------------
// Top bar
// ---------------------------------------------------------------------------

fn render_top_bar(
    frame: &mut Frame,
    sessions: &[Session],
    status_filter: Option<&str>,
    area: Rect,
    theme: &Theme,
) {
    let active = sessions
        .iter()
        .filter(|s| s.status == Status::Active)
        .count();
    let waiting = sessions
        .iter()
        .filter(|s| s.status == Status::Waiting)
        .count();
    let done = sessions
        .iter()
        .filter(|s| s.status == Status::Done)
        .count();
    let idle = sessions
        .iter()
        .filter(|s| s.status == Status::Idle)
        .count();
    let total = sessions.len();

    let title_span = Span::styled(
        " AGENTICK ",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );

    let mut spans = vec![
        title_span,
        Span::raw("  "),
        Span::styled(
            format!("\u{25CF} {} active", active),
            Style::default().fg(theme.green),
        ),
        Span::raw("  "),
        Span::styled(
            format!("\u{25C9} {} waiting", waiting),
            Style::default().fg(theme.yellow),
        ),
        Span::raw("  "),
        Span::styled(
            format!("\u{25CF} {} done", done),
            Style::default().fg(theme.green),
        ),
        Span::raw("  "),
        Span::styled(
            format!("\u{25CB} {} idle", idle),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled(
            " \u{2502} ",
            Style::default().fg(theme.border),
        ),
        Span::styled(
            format!("{} total", total),
            Style::default().fg(theme.text_dim),
        ),
    ];

    // Show active filter if one is set
    if let Some(filter) = status_filter {
        spans.push(Span::styled(
            "  \u{2502} ",
            Style::default().fg(theme.border),
        ));
        spans.push(Span::styled(
            format!("filter: {}", filter),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let counts = Line::from(spans);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let para = Paragraph::new(counts).block(block);
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Build flattened display list (auto-grouped by project_path)
// ---------------------------------------------------------------------------

/// Read the current git branch for a directory by parsing `.git/HEAD`.
///
/// Returns `None` if the path is not a git repo.  For detached HEAD,
/// returns the first 7 characters of the commit hash.
fn git_branch(path: &Path) -> Option<String> {
    let head = path.join(".git/HEAD");
    let content = std::fs::read_to_string(head).ok()?;
    let content = content.trim();
    if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else if content.len() >= 7 {
        // Detached HEAD — show short hash
        Some(content.chars().take(7).collect())
    } else {
        None
    }
}

/// Extract the directory display name from a project path.
fn dir_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("unknown"))
        .to_string()
}

/// Derive a stable group ordering from the full (unfiltered) session list.
/// Groups appear in the order their first session was added to the store.
fn canonical_group_order(sessions: &[Session]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for s in sessions {
        let key = s.project_path.to_string_lossy().to_string();
        if seen.insert(key.clone()) {
            paths.push(key);
        }
    }
    paths
}

fn build_display_items<'a>(
    sessions: &[&'a Session],
    collapsed_dirs: &HashSet<String>,
    canonical_paths: &[String],
) -> Vec<DisplayItem<'a>> {
    let mut items: Vec<DisplayItem<'a>> = Vec::new();

    // Use the canonical (insertion-order) path list so groups never reorder.
    // Only emit groups that have at least one session in the filtered set.
    let paths: Vec<&String> = canonical_paths
        .iter()
        .filter(|p| sessions.iter().any(|s| s.project_path.to_string_lossy() == p.as_str()))
        .collect();

    for path_key in &paths {
        let group_sessions: Vec<&'a Session> = sessions
            .iter()
            .filter(|s| s.project_path.to_string_lossy() == path_key.as_str())
            .copied()
            .collect();

        let path = Path::new(path_key.as_str());
        let display_name = dir_display_name(path);
        let expanded = !collapsed_dirs.contains(path_key.as_str());
        let branch = git_branch(path);

        items.push(DisplayItem::GroupHeader {
            name: display_name,
            count: group_sessions.len(),
            expanded,
            branch,
        });

        if expanded {
            // Collect all session IDs in this group so we can detect orphaned forks.
            let group_ids: HashSet<&str> = group_sessions.iter().map(|s| s.id.as_str()).collect();

            // Separate top-level sessions (no parent) and orphaned forks
            // (parent was deleted) from forks whose parent is still present.
            let top_level: Vec<&'a Session> = group_sessions
                .iter()
                .filter(|s| match &s.forked_from {
                    None => true,
                    Some(parent_id) => !group_ids.contains(parent_id.as_str()),
                })
                .copied()
                .collect();
            let top_last_idx = top_level.len().saturating_sub(1);

            for (i, sess) in top_level.iter().enumerate() {
                // Collect forks of this session within this group.
                let forks: Vec<&'a Session> = group_sessions
                    .iter()
                    .filter(|s| s.forked_from.as_deref() == Some(&sess.id))
                    .copied()
                    .collect();

                let has_forks = !forks.is_empty();
                // Parent is "last" in tree only if it's the last top-level
                // AND has no forks (forks extend the subtree).
                let parent_is_last = i == top_last_idx && !has_forks;
                // Whether more top-level sessions follow this parent's subtree.
                let more_siblings = i < top_last_idx;

                items.push(DisplayItem::SessionRow {
                    session: sess,
                    is_last: parent_is_last,
                    is_fork: false,
                    parent_continues: false,
                });

                let fork_last_idx = forks.len().saturating_sub(1);
                for (fi, fork) in forks.iter().enumerate() {
                    let fork_is_last = fi == fork_last_idx;
                    items.push(DisplayItem::SessionRow {
                        session: fork,
                        is_last: fork_is_last,
                        is_fork: true,
                        parent_continues: more_siblings,
                    });
                }
            }
        }
    }

    items
}

// ---------------------------------------------------------------------------
// Session list
// ---------------------------------------------------------------------------

fn render_session_list(
    frame: &mut Frame,
    items: &[DisplayItem],
    selected: usize,
    tick_count: u32,
    area: Rect,
    theme: &Theme,
    rename_state: Option<(&str, &str)>, // (session_id, buffer)
    inline_new: Option<&InlineNewRenderState<'_>>,
    search_matches: &HashSet<usize>,
    confirm_delete_id: Option<&str>,
) {
    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(idx, item)| match item {
            DisplayItem::GroupHeader {
                name,
                count,
                expanded,
                branch,
            } => {
                let arrow = if *expanded { "\u{25BC}" } else { "\u{25B6}" };

                let mut lines: Vec<Line> = Vec::new();

                // Empty separator line before each group (except the first)
                if idx > 0 {
                    lines.push(Line::from(""));
                }

                // Group header: arrow + folder emoji + dirname + (count) ... branch
                let mut spans = vec![
                    Span::styled(
                        format!(" {} ", arrow),
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(
                        format!("\u{1F5C3}\u{FE0F}  {}", name),
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(
                        format!(" ({})", count),
                        Style::default()
                            .fg(theme.text_dim)
                            .add_modifier(Modifier::DIM),
                    ),
                ];

                // Git branch after the count
                if let Some(br) = branch {
                    spans.push(Span::styled(
                        format!("  \u{f418} {}", br),
                        Style::default().fg(theme.purple),
                    ));
                }

                lines.push(Line::from(spans));

                ListItem::new(lines)
            }
            DisplayItem::SessionRow { session, is_last, is_fork, parent_continues } => {
                let is_deleting = confirm_delete_id
                    .map(|did| did == session.id)
                    .unwrap_or(false);
                let status_str = session.status.to_string();
                let tool_str = session.tool.to_string();

                // Active status: pulse between ○ (dim) and ● (green).
                let (indicator_char, indicator_color) = if is_deleting {
                    (session.status.indicator(), ratatui::style::Color::White)
                } else if session.status == Status::Active {
                    if tick_count % 2 == 0 {
                        ("\u{25CF}", status_color(&status_str))
                    } else {
                        ("\u{25CB}", theme.text_dim)
                    }
                } else {
                    (session.status.indicator(), status_color(&status_str))
                };

                // Tree connector — forks align └─ with parent title's left edge
                // Parent layout: connector(4) + indicator(2) + tool(9) = 15 cols before title
                let connector = if *is_fork {
                    // Show │ continuation when more top-level sessions follow.
                    // " │             └─ " or "               └─ "
                    match (*parent_continues, *is_last) {
                        (true,  true)  => " \u{2502}             \u{2514}\u{2500} ",
                        (true,  false) => " \u{2502}             \u{251C}\u{2500} ",
                        (false, true)  => "               \u{2514}\u{2500} ",
                        (false, false) => "               \u{251C}\u{2500} ",
                    }
                } else if *is_last {
                    " \u{2514}\u{2500} "
                } else {
                    " \u{251C}\u{2500} "
                };

                // Layout: highlight(2) + connector + indicator(2) [+ tool(9) if not fork] + title
                let prefix_w = if *is_fork {
                    2 + 18 + 2  // 15 pad + 3 (└─ ) + indicator, no tool
                } else {
                    2 + 4 + 2 + 9
                };
                let delete_label = "Delete (Y/esc)?";
                let bar_w: usize = 4;
                let has_bar = session.context_percentage().is_some();
                let bar_reserved = if is_deleting {
                    1 + delete_label.len()
                } else if has_bar {
                    1 + bar_w + 1
                } else {
                    0
                };
                let max_title = (area.width as usize).saturating_sub(prefix_w + bar_reserved);

                // Check if this session is being renamed inline.
                let is_renaming = rename_state
                    .map(|(rid, _)| rid == session.id)
                    .unwrap_or(false);

                let title_display = if is_renaming {
                    let buf = rename_state.unwrap().1;
                    format!("{}\u{2588}", buf) // buffer + block cursor
                } else if session.title.len() > max_title && max_title > 3 {
                    let mut t: String = session.title.chars().take(max_title - 3).collect();
                    t.push_str("...");
                    t
                } else {
                    session.title.clone()
                };

                let delete_style = Style::default()
                    .fg(ratatui::style::Color::White)
                    .bg(theme.red);

                let mut spans = vec![
                    Span::styled(
                        connector,
                        if is_deleting {
                            delete_style.add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                                .fg(theme.border)
                                .add_modifier(Modifier::DIM)
                        },
                    ),
                    Span::styled(
                        format!("{} ", indicator_char),
                        if is_deleting {
                            delete_style.add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(indicator_color)
                        },
                    ),
                ];
                // Skip tool name for forks — it's always the same as the parent.
                if !*is_fork {
                    spans.push(Span::styled(
                        format!("{:<8} ", tool_str),
                        if is_deleting {
                            delete_style.add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(tool_color(&tool_str))
                        },
                    ));
                }
                let is_search_match = search_matches.contains(&idx);
                let title_style = if is_deleting {
                    delete_style.add_modifier(Modifier::BOLD)
                } else if is_renaming {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else if is_search_match {
                    Style::default()
                        .fg(theme.yellow)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default()
                        .fg(theme.text)
                        .add_modifier(Modifier::BOLD)
                };
                spans.push(Span::styled(title_display.clone(), title_style));

                if is_deleting {
                    // Right-align "Delete (Y/esc)?" label
                    let used_w = prefix_w + title_display.len();
                    let label_col = (area.width as usize).saturating_sub(delete_label.len() + 1);
                    let gap = label_col.saturating_sub(used_w);
                    if gap > 0 {
                        spans.push(Span::styled(" ".repeat(gap), delete_style));
                    }
                    spans.push(Span::styled(
                        delete_label,
                        delete_style.add_modifier(Modifier::BOLD),
                    ));
                } else if let Some(pct) = session.context_percentage() {
                    // Context bar: 4 chars, filled count + color by bucket
                    //   <25% → 1 white  |  25-50% → 2 green  |  50-75% → 3 yellow  |  ≥75% → 4 red
                    let (filled, color) = if pct >= 75.0 {
                        (4, theme.red)
                    } else if pct >= 50.0 {
                        (3, theme.yellow)
                    } else if pct >= 25.0 {
                        (2, theme.green)
                    } else {
                        (1, theme.text_dim)
                    };
                    let empty = bar_w - filled;

                    // Right-align the bar
                    let used_w = prefix_w + title_display.len();
                    let bar_col = (area.width as usize).saturating_sub(bar_w + 1);
                    let gap = bar_col.saturating_sub(used_w);
                    if gap > 0 {
                        spans.push(Span::raw(" ".repeat(gap)));
                    }
                    spans.push(Span::styled(
                        "\u{2501}".repeat(filled),
                        Style::default().fg(color),
                    ));
                    if empty > 0 {
                        spans.push(Span::styled(
                            "\u{2501}".repeat(empty),
                            Style::default().fg(theme.text_dim).add_modifier(Modifier::DIM),
                        ));
                    }
                }

                let line = Line::from(spans);
                ListItem::new(vec![line])
            }
        })
        .collect();

    // --- Append inline new-session rows when active --------------------------
    let (final_items, pin_selection) = if let Some(ins) = inline_new {
        let mut all = list_items;
        // Separator line.
        all.push(ListItem::new(Line::from(Span::styled(
            "\u{2500}".repeat(area.width.saturating_sub(2) as usize),
            Style::default().fg(theme.border).add_modifier(Modifier::DIM),
        ))));

        // Input row: " + New: query█" or placeholder.
        let input_idx = all.len();
        let display_query = if ins.query.is_empty() && ins.is_dir_search {
            "\u{2588} type to search directories...".to_string()
        } else {
            format!("{}\u{2588}", ins.query)
        };
        all.push(ListItem::new(Line::from(vec![
            Span::styled(
                " + New: ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                display_query,
                Style::default()
                    .fg(if ins.is_dir_search { theme.text } else { theme.accent })
                    .add_modifier(Modifier::BOLD),
            ),
        ])));

        // Suggestion rows (only during DirSearch step).
        if ins.is_dir_search {
            for (i, (path, _score)) in ins.suggestions.iter().enumerate() {
                let is_sel = i == ins.dir_selected;
                let prefix = if is_sel { "   \u{25B6} " } else { "     " };
                let short = shorten_home(path);
                all.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default().fg(if is_sel { theme.accent } else { theme.text_dim }),
                    ),
                    Span::styled(
                        short,
                        Style::default().fg(if is_sel { theme.text } else { theme.text_dim }),
                    ),
                ])));
            }
        }

        (all, Some(input_idx))
    } else {
        (list_items, None)
    };

    // When confirming delete, the selected row gets a red highlight instead of
    // the default surface highlight, so the per-span bg colors aren't overridden.
    let is_delete_selected = confirm_delete_id.is_some();
    let highlight_style = if is_delete_selected {
        Style::default()
            .bg(theme.red)
            .fg(ratatui::style::Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(theme.surface)
            .add_modifier(Modifier::BOLD)
    };

    let list = List::new(final_items)
        .block(
            Block::default()
                .borders(Borders::NONE)
                .style(Style::default().bg(theme.bg)),
        )
        .highlight_style(highlight_style)
        .highlight_symbol("\u{258C} ")
        .repeat_highlight_symbol(true);

    let mut state = ListState::default();
    if let Some(pin) = pin_selection {
        state.select(Some(pin));
    } else if !items.is_empty() {
        state.select(Some(selected.min(items.len().saturating_sub(1))));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

/// Replace `$HOME` prefix with `~` for display.
fn shorten_home(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let h = home.to_string_lossy();
        if path.starts_with(h.as_ref()) {
            return format!("~{}", &path[h.len()..]);
        }
    }
    path.to_string()
}

// ---------------------------------------------------------------------------
// Find selected session helper
// ---------------------------------------------------------------------------

/// Walk the flattened display list and return the `Session` at the given
/// `selected` index, or `None` if the index points at a group header or is
/// out of range.
pub fn find_selected_session<'a>(
    sessions: &'a [Session],
    collapsed_dirs: &HashSet<String>,
    selected: usize,
    status_filter: Option<&str>,
) -> Option<&'a Session> {
    let filtered: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            status_filter
                .map(|f| s.status.to_string() == f)
                .unwrap_or(true)
        })
        .collect();

    let canonical_paths = canonical_group_order(sessions);
    let items = build_display_items(&filtered, collapsed_dirs, &canonical_paths);
    match items.get(selected) {
        Some(DisplayItem::SessionRow { session, .. }) => Some(session),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Preview pane
// ---------------------------------------------------------------------------

fn render_preview_pane(
    frame: &mut Frame,
    sessions: &[Session],
    collapsed_dirs: &HashSet<String>,
    selected: usize,
    status_filter: Option<&str>,
    preview_content: Option<&Text<'static>>,
    scroll_cache: Option<&Text<'static>>,
    preview_scroll: usize,
    focus: FocusPane,
    area: Rect,
    theme: &Theme,
) {
    let selected_session = find_selected_session(sessions, collapsed_dirs, selected, status_filter);
    let is_focused = focus == FocusPane::Right;

    let border_color = if is_focused {
        theme.border_focused
    } else {
        theme.border
    };

    let session = match selected_session {
        Some(s) => s,
        None => {
            render_placeholder(frame, " Preview ", "No preview available", border_color, theme, area);
            return;
        }
    };

    let base_title = if is_focused {
        format!(" {} [INTERACTIVE] ", session.title)
    } else {
        format!(" {} ", session.title)
    };

    // --- Scroll mode: render from cached scrollback snapshot ---------------
    if preview_scroll > 0 {
        if let Some(styled_text) = scroll_cache {
            let title = format!("{} [scroll +{}] ", base_title.trim_end(), preview_scroll);

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.yellow)
                        .add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().bg(theme.bg));

            let all_lines = &styled_text.lines;
            let max_lines = area.height.saturating_sub(2) as usize;
            // Clamp: never scroll past the top of history.
            let clamped_scroll = preview_scroll.min(all_lines.len().saturating_sub(max_lines));
            let end = all_lines.len().saturating_sub(clamped_scroll);
            let start = end.saturating_sub(max_lines);
            let visible_lines = all_lines[start..end].to_vec();

            let para = Paragraph::new(Text::from(visible_lines)).block(block);
            frame.render_widget(para, area);
            return;
        }
    }

    // --- Live mode: render captured pane content as styled text ------------
    match preview_content {
        Some(content) => {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Span::styled(
                    base_title,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ))
                .style(Style::default().bg(theme.bg));

            // Show bottom of content (most recent output), clipped to area.
            let max_lines = area.height.saturating_sub(2) as usize;
            let all_lines = &content.lines;
            let start = all_lines.len().saturating_sub(max_lines);
            let visible_lines = all_lines[start..].to_vec();

            let para = Paragraph::new(Text::from(visible_lines)).block(block);
            frame.render_widget(para, area);
        }
        None => {
            render_placeholder(frame, &base_title, "Connecting...", border_color, theme, area);
        }
    }
}

/// Small helper to render a preview pane with a text message (no terminal content).
fn render_placeholder(
    frame: &mut Frame,
    title: &str,
    message: &str,
    border_color: ratatui::style::Color,
    theme: &Theme,
    area: Rect,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.bg));

    let msg = Text::from(Line::from(Span::styled(
        message.to_string(),
        Style::default().fg(theme.text_dim),
    )));
    let para = Paragraph::new(msg).block(block);
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Confirm dialog
// ---------------------------------------------------------------------------

/// Render a simple Y/N confirmation dialog centered on the screen.
pub fn render_confirm_dialog(frame: &mut Frame, message: &str, area: Rect) {
    let theme = dark_theme();

    // Dialog dimensions: 50% width, 5 lines tall, centered
    let dialog_width = area.width / 2;
    let dialog_height: u16 = 5;
    let x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
    let y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    // Clear the area behind the dialog
    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Confirm ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(theme.surface));

    let text = vec![
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(theme.text),
        )),
        Line::from(Span::styled(
            "y/Y: confirm  any other key: cancel",
            Style::default().fg(theme.text_dim),
        )),
    ];

    let para = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(para, dialog_area);
}

// ---------------------------------------------------------------------------
// Quick-create bottom sheet
// ---------------------------------------------------------------------------

/// Render a 3-line which-key overlay anchored to the bottom of the screen.
pub fn render_quick_create_sheet(frame: &mut Frame, project_path: &Path, area: Rect) {
    let theme = dark_theme();

    let sheet_height: u16 = 3;
    let y = area.height.saturating_sub(sheet_height);
    let sheet_area = Rect::new(area.x, y, area.width, sheet_height);

    frame.render_widget(Clear, sheet_area);

    let dir_name = project_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Line 1: title + dirname
    let mut line1_spans = vec![
        Span::styled(
            " Quick Create ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("in {}", dir_name),
            Style::default().fg(theme.text_dim),
        ),
    ];
    // Pad to fill width
    let line1_len: usize = line1_spans.iter().map(|s| s.content.len()).sum();
    if (area.width as usize) > line1_len {
        line1_spans.push(Span::raw(" ".repeat(area.width as usize - line1_len)));
    }

    // Line 2: key-action pairs
    let mut line2_spans: Vec<Span> = vec![Span::raw(" ")];
    for (key, label, cmd) in QUICK_CREATE_KEYS {
        line2_spans.push(Span::styled(
            format!("{}", key),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        line2_spans.push(Span::styled(
            format!(" {} ", label),
            Style::default().fg(tool_color(cmd)),
        ));
        line2_spans.push(Span::styled(" ", Style::default().fg(theme.text_dim)));
    }

    // Line 3: cancel / full dialog hints
    let line3_spans = vec![
        Span::styled(
            " Esc ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("cancel  ", Style::default().fg(theme.text_dim)),
        Span::styled(
            "N ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("full dialog", Style::default().fg(theme.text_dim)),
    ];

    let text = Text::from(vec![
        Line::from(line1_spans),
        Line::from(line2_spans),
        Line::from(line3_spans),
    ]);

    let para = Paragraph::new(text).style(Style::default().bg(theme.surface));
    frame.render_widget(para, sheet_area);
}

// ---------------------------------------------------------------------------
// Help bar
// ---------------------------------------------------------------------------

fn render_help_bar(frame: &mut Frame, focus: FocusPane, area: Rect, theme: &Theme) {
    let key_style = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(theme.text_dim);

    let help_spans = if focus == FocusPane::Right {
        vec![
            Span::styled(" ` ", key_style),
            Span::styled("back to list  ", dim_style),
            Span::styled("scroll ", key_style),
            Span::styled("history  ", dim_style),
            Span::styled("Ctrl+C ", key_style),
            Span::styled("quit  ", dim_style),
            Span::styled("all other keys ", Style::default().fg(theme.yellow)),
            Span::styled("forwarded to agent", dim_style),
        ]
    } else {
        vec![
            Span::styled(" n ", key_style),
            Span::styled("quick new  ", dim_style),
            Span::styled("N ", key_style),
            Span::styled("new session  ", dim_style),
            Span::styled("d ", key_style),
            Span::styled("delete  ", dim_style),
            Span::styled("K ", key_style),
            Span::styled("kill  ", dim_style),
            Span::styled("f ", key_style),
            Span::styled("fork  ", dim_style),
            Span::styled("` ", key_style),
            Span::styled("interact  ", dim_style),
            Span::styled("/ ", key_style),
            Span::styled("search  ", dim_style),
            Span::styled("r ", key_style),
            Span::styled("rename  ", dim_style),
            Span::styled("R ", key_style),
            Span::styled("refresh  ", dim_style),
            Span::styled("Tab ", key_style),
            Span::styled("filter  ", dim_style),
            Span::styled("Enter ", key_style),
            Span::styled("attach (Ctrl+q detach)  ", dim_style),
            Span::styled("q ", key_style),
            Span::styled("quit", dim_style),
        ]
    };

    let help = Paragraph::new(Line::from(help_spans)).style(Style::default().bg(theme.surface));
    frame.render_widget(help, area);
}

// ---------------------------------------------------------------------------
// Helpers -- count display items to know the list length
// ---------------------------------------------------------------------------

/// Return the total number of display items (group headers + visible session
/// rows) for the given sessions and collapsed dirs. Useful for clamping the cursor.
pub fn display_item_count(
    sessions: &[Session],
    collapsed_dirs: &HashSet<String>,
    status_filter: Option<&str>,
) -> usize {
    let filtered: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            status_filter
                .map(|f| s.status.to_string() == f)
                .unwrap_or(true)
        })
        .collect();

    let canonical_paths = canonical_group_order(sessions);
    build_display_items(&filtered, collapsed_dirs, &canonical_paths).len()
}

/// Returns `true` if the display item at `index` is a group header (not a session row).
pub fn is_group_header(
    sessions: &[Session],
    collapsed_dirs: &HashSet<String>,
    index: usize,
    status_filter: Option<&str>,
) -> bool {
    let filtered: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            status_filter
                .map(|f| s.status.to_string() == f)
                .unwrap_or(true)
        })
        .collect();

    let canonical_paths = canonical_group_order(sessions);
    let items = build_display_items(&filtered, collapsed_dirs, &canonical_paths);
    matches!(items.get(index), Some(DisplayItem::GroupHeader { .. }))
}

/// Find the display index for a session with the given id.
///
/// Uses the same `build_display_items` logic as the renderer to guarantee
/// the returned index is consistent with what's drawn on screen.
/// Find the display index for a session with the given id.
///
/// Uses the same `build_display_items` logic as the renderer to guarantee
/// the returned index is consistent with what's drawn on screen.
pub fn find_session_display_index(
    sessions: &[Session],
    collapsed_dirs: &HashSet<String>,
    target_id: &str,
    status_filter: Option<&str>,
) -> Option<usize> {
    let filtered: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            status_filter
                .map(|f| s.status.to_string() == f)
                .unwrap_or(true)
        })
        .collect();

    let canonical_paths = canonical_group_order(sessions);
    let items = build_display_items(&filtered, collapsed_dirs, &canonical_paths);
    items.iter().position(|item| matches!(item, DisplayItem::SessionRow { session, .. } if session.id == target_id))
}

/// Return display indices of session rows matching a search query.
///
/// Uses `build_display_items` for consistent index computation.
pub fn search_display_indices(
    sessions: &[Session],
    collapsed_dirs: &HashSet<String>,
    query: &str,
    status_filter: Option<&str>,
) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();

    let filtered: Vec<&Session> = sessions
        .iter()
        .filter(|s| {
            status_filter
                .map(|f| s.status.to_string() == f)
                .unwrap_or(true)
        })
        .collect();

    let canonical_paths = canonical_group_order(sessions);
    let items = build_display_items(&filtered, collapsed_dirs, &canonical_paths);
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            DisplayItem::SessionRow { session, .. } => {
                if session.title.to_lowercase().contains(&query_lower)
                    || session.short_path().to_lowercase().contains(&query_lower)
                {
                    Some(i)
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}
