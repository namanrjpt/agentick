use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::session::instance::{Group, Session, Status};
use crate::tui::app::FocusPane;
use crate::tui::theme::{dark_theme, status_color, tool_color, Theme};

// ---------------------------------------------------------------------------
// Display item -- either a group header or a session row in the flat list
// ---------------------------------------------------------------------------

enum DisplayItem<'a> {
    GroupHeader {
        name: &'a str,
        count: usize,
        expanded: bool,
    },
    SessionRow {
        session: &'a Session,
        is_last: bool,
    },
}

// ---------------------------------------------------------------------------
// Public render entry point
// ---------------------------------------------------------------------------

/// Render the main dashboard into `area`.
///
/// * `sessions`      -- all sessions (will be filtered by `status_filter`).
/// * `groups`        -- group definitions (ordering / expand state).
/// * `selected`      -- cursor index in the flattened display list.
/// * `status_filter` -- if `Some`, only sessions with this status are shown.
pub fn render_dashboard(
    frame: &mut Frame,
    sessions: &[Session],
    groups: &[Group],
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

    let items = build_display_items(&filtered, groups);

    // --- Render session list (with optional preview pane) -----------------

    if area.width > 100 {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(80),
            ])
            .split(chunks[1]);

        render_session_list(frame, &items, selected, tick_count, h_chunks[0], &theme);
        render_preview_pane(
            frame, sessions, groups, selected, status_filter, preview_content,
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
// Build flattened display list
// ---------------------------------------------------------------------------

fn build_display_items<'a>(
    sessions: &[&'a Session],
    groups: &'a [Group],
) -> Vec<DisplayItem<'a>> {
    let mut items: Vec<DisplayItem<'a>> = Vec::new();

    // Sessions that belong to a defined group.
    for group in groups {
        let group_sessions: Vec<&&Session> = sessions
            .iter()
            .filter(|s| s.group.as_deref() == Some(&group.name))
            .collect();

        items.push(DisplayItem::GroupHeader {
            name: &group.name,
            count: group_sessions.len(),
            expanded: group.expanded,
        });

        if group.expanded {
            let last_idx = group_sessions.len().saturating_sub(1);
            for (i, sess) in group_sessions.iter().enumerate() {
                items.push(DisplayItem::SessionRow {
                    session: sess,
                    is_last: i == last_idx,
                });
            }
        }
    }

    // Ungrouped sessions.
    let ungrouped: Vec<&&Session> = sessions
        .iter()
        .filter(|s| {
            s.group.is_none()
                || !groups
                    .iter()
                    .any(|g| Some(g.name.as_str()) == s.group.as_deref())
        })
        .collect();

    if !ungrouped.is_empty() {
        items.push(DisplayItem::GroupHeader {
            name: "ungrouped",
            count: ungrouped.len(),
            expanded: true,
        });
        let last_idx = ungrouped.len().saturating_sub(1);
        for (i, sess) in ungrouped.iter().enumerate() {
            items.push(DisplayItem::SessionRow {
                session: sess,
                is_last: i == last_idx,
            });
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
            } => {
                let arrow = if *expanded { "\u{25BC}" } else { "\u{25B6}" };

                let mut lines: Vec<Line> = Vec::new();

                // Empty separator line before each group (except the first)
                if idx > 0 {
                    lines.push(Line::from(""));
                }

                // Group header: arrow + lowercase name + (count)
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", arrow),
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(
                        name.to_string(),
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(
                        format!(" ({})", count),
                        Style::default()
                            .fg(theme.text_dim)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));

                ListItem::new(lines)
            }
            DisplayItem::SessionRow { session, is_last } => {
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

                // Tree connector
                let connector = if *is_last { " \u{2514}\u{2500} " } else { " \u{251C}\u{2500} " };

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
                    Span::styled(
                        format!("{:<8} ", tool_str),
                        Style::default().fg(tool_color(&tool_str)),
                    ),
                    Span::styled(
                        session.title.clone(),
                        Style::default()
                            .fg(theme.text)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];

                // Context bar -- only if data available
                if let Some(pct) = session.context_percentage() {
                    let filled = (pct / 10.0).round() as usize;
                    let empty = 10_usize.saturating_sub(filled);

                    let bar_color = if pct > 80.0 {
                        theme.red
                    } else if pct > 60.0 {
                        theme.yellow
                    } else {
                        theme.green
                    };

                    spans.push(Span::styled("  [", Style::default().fg(theme.text_dim)));
                    if filled > 0 {
                        spans.push(Span::styled(
                            "\u{2588}".repeat(filled),
                            Style::default().fg(bar_color),
                        ));
                    }
                    spans.push(Span::styled(
                        "\u{2591}".repeat(empty),
                        Style::default().fg(theme.text_dim),
                    ));
                    spans.push(Span::styled(
                        format!("] {:.0}%", pct),
                        Style::default().fg(theme.text_dim),
                    ));
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
    groups: &'a [Group],
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

    let items = build_display_items(&filtered, groups);
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
    groups: &[Group],
    selected: usize,
    status_filter: Option<&str>,
    preview_content: Option<&Text<'static>>,
    scroll_cache: Option<&Text<'static>>,
    preview_scroll: usize,
    focus: FocusPane,
    area: Rect,
    theme: &Theme,
) {
    let selected_session = find_selected_session(sessions, groups, selected, status_filter);
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
            let end = all_lines.len().saturating_sub(preview_scroll);
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
            Span::styled("new  ", dim_style),
            Span::styled("d ", key_style),
            Span::styled("delete  ", dim_style),
            Span::styled("K ", key_style),
            Span::styled("kill  ", dim_style),
            Span::styled("` ", key_style),
            Span::styled("interact  ", dim_style),
            Span::styled("/ ", key_style),
            Span::styled("search  ", dim_style),
            Span::styled("Tab ", key_style),
            Span::styled("filter  ", dim_style),
            Span::styled("Enter ", key_style),
            Span::styled("attach  ", dim_style),
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
/// rows) for the given sessions and groups. Useful for clamping the cursor.
pub fn display_item_count(
    sessions: &[Session],
    groups: &[Group],
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

    build_display_items(&filtered, groups).len()
}
