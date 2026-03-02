use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::theme::dark_theme;

/// Render the group creation dialog.
///
/// Centered modal (50% width, 7 lines):
///   +-- Create Group ----------+
///   | Name: query|             |
///   | Assign: session_name     |  (if applicable)
///   | Enter: create  Esc: cancel |
///   +--------------------------+
pub fn render_group_dialog(
    frame: &mut Frame,
    name: &str,
    assign_session: Option<&str>,
    area: Rect,
) {
    let theme = dark_theme();

    // Dialog dimensions: 50% width, 7 lines, centered.
    let dialog_width = (area.width * 50 / 100).max(30).min(area.width);
    let dialog_height: u16 = 7;

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(dialog_height)) / 2),
            Constraint::Length(dialog_height),
            Constraint::Min(0),
        ])
        .split(area);
    let horiz = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(dialog_width)) / 2),
            Constraint::Length(dialog_width),
            Constraint::Min(0),
        ])
        .split(vert[1]);
    let dialog_area = horiz[1];

    // Clear behind the dialog.
    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(Span::styled(
            " Create Group ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.surface));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    // Build lines inside the dialog.
    let mut lines: Vec<Line> = Vec::new();

    // Empty spacer line.
    lines.push(Line::from(""));

    // Name field.
    lines.push(Line::from(vec![
        Span::styled("  Name: ", Style::default().fg(theme.text_dim)),
        Span::styled(
            format!("{}\u{2588}", name),
            Style::default().fg(theme.text),
        ),
    ]));

    // Assign line (if applicable).
    if let Some(session_title) = assign_session {
        lines.push(Line::from(vec![
            Span::styled("  Assign: ", Style::default().fg(theme.text_dim)),
            Span::styled(
                session_title.to_string(),
                Style::default().fg(theme.accent),
            ),
        ]));
    } else {
        lines.push(Line::from(""));
    }

    // Spacer.
    lines.push(Line::from(""));

    // Footer instructions.
    lines.push(Line::from(vec![
        Span::styled(
            "Enter",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": create  ", Style::default().fg(theme.text_dim)),
        Span::styled(
            "Esc",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": cancel", Style::default().fg(theme.text_dim)),
    ]));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(theme.surface))
        .alignment(Alignment::Left);
    frame.render_widget(para, inner);
}
