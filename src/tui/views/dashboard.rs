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

    let items = build_display_items(&filtered, collapsed_dirs);

    // --- Render session list (with optional preview pane) -----------------

    if area.width > 100 {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(75),
            ])
            .split(chunks[1]);

        render_session_list(frame, &items, selected, tick_count, h_chunks[0], &theme);
        render_preview_pane(
            frame, sessions, collapsed_dirs, selected, status_filter, preview_content,
            scroll_cache, preview_scroll, focus, h_chunks[1], &theme,
        );
    } else {
        render_session_list(frame, &items, selected, tick_count, chunks[1], &theme);
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

fn build_display_items<'a>(
    sessions: &[&'a Session],
    collapsed_dirs: &HashSet<String>,
) -> Vec<DisplayItem<'a>> {
    let mut items: Vec<DisplayItem<'a>> = Vec::new();

    // Collect unique project paths preserving first-seen order.
    let mut seen = HashSet::new();
    let mut paths: Vec<String> = Vec::new();
    for s in sessions {
        let key = s.project_path.to_string_lossy().to_string();
        if seen.insert(key.clone()) {
            paths.push(key);
        }
    }

    for path_key in &paths {
        let group_sessions: Vec<&'a Session> = sessions
            .iter()
            .filter(|s| s.project_path.to_string_lossy() == path_key.as_str())
            .copied()
            .collect();

        let path = Path::new(path_key);
        let display_name = dir_display_name(path);
        let expanded = !collapsed_dirs.contains(path_key);
        let branch = git_branch(path);

        items.push(DisplayItem::GroupHeader {
            name: display_name,
            count: group_sessions.len(),
            expanded,
            branch,
        });

        if expanded {
            // Separate top-level sessions (no parent) from forks.
            let top_level: Vec<&'a Session> = group_sessions
                .iter()
                .filter(|s| s.forked_from.is_none())
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

                items.push(DisplayItem::SessionRow {
                    session: sess,
                    is_last: parent_is_last,
                    is_fork: false,
                });

                let fork_last_idx = forks.len().saturating_sub(1);
                for (fi, fork) in forks.iter().enumerate() {
                    items.push(DisplayItem::SessionRow {
                        session: fork,
                        is_last: fi == fork_last_idx,
                        is_fork: true,
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
            DisplayItem::SessionRow { session, is_last, is_fork } => {
                let status_str = session.status.to_string();
                let tool_str = session.tool.to_string();

                // Active status: pulse between ○ (dim) and ● (green).
                let (indicator_char, indicator_color) = if session.status == Status::Active {
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
                    // 15 spaces to reach title column, then └─ + space
                    if *is_last { "               \u{2514}\u{2500} " } else { "               \u{251C}\u{2500} " }
                } else {
                    if *is_last { " \u{2514}\u{2500} " } else { " \u{251C}\u{2500} " }
                };

                // Layout: highlight(2) + connector + indicator(2) [+ tool(9) if not fork] + title
                let prefix_w = if *is_fork {
                    2 + 18 + 2  // 15 pad + 3 (└─ ) + indicator, no tool
                } else {
                    2 + 4 + 2 + 9
                };
                let bar_w: usize = 4;
                let has_bar = session.context_percentage().is_some();
                let bar_reserved = if has_bar { 1 + bar_w + 1 } else { 0 }; // " ████████ "
                let max_title = (area.width as usize).saturating_sub(prefix_w + bar_reserved);
                let title_display = if session.title.len() > max_title && max_title > 3 {
                    let mut t: String = session.title.chars().take(max_title - 3).collect();
                    t.push_str("...");
                    t
                } else {
                    session.title.clone()
                };

                let mut spans = vec![
                    Span::styled(
                        connector,
                        Style::default()
                            .fg(theme.border)
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::styled(
                        format!("{} ", indicator_char),
                        Style::default().fg(indicator_color),
                    ),
                ];
                // Skip tool name for forks — it's always the same as the parent.
                if !*is_fork {
                    spans.push(Span::styled(
                        format!("{:<8} ", tool_str),
                        Style::default().fg(tool_color(&tool_str)),
                    ));
                }
                spans.push(Span::styled(
                    title_display.clone(),
                    Style::default()
                        .fg(theme.text)
                        .add_modifier(Modifier::BOLD),
                ));

                // Context bar: 4 chars, filled count + color by bucket
                //   <25% → 1 white  |  25-50% → 2 green  |  50-75% → 3 yellow  |  ≥75% → 4 red
                if let Some(pct) = session.context_percentage() {
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

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::NONE)
                .style(Style::default().bg(theme.bg)),
        )
        .highlight_style(
            Style::default()
                .bg(theme.surface)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{258E} ");

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len().saturating_sub(1))));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Find selected session helper
// ---------------------------------------------------------------------------

/// Walk the flattened display list and return the `Session` at the given
/// `selected` index, or `None` if the index points at a group header or is
/// out of range.
fn find_selected_session<'a>(
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

    let items = build_display_items(&filtered, collapsed_dirs);
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

    build_display_items(&filtered, collapsed_dirs).len()
}
