use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::tui::theme::dark_theme;

/// Render the search bar overlay.
///
/// Draws a single-line bar at the top of `area` showing:
///   / query|  (N matches)
pub fn render_search_bar(frame: &mut Frame, query: &str, result_count: usize, area: Rect) {
    let theme = dark_theme();

    // The search bar occupies a single line at the top of the area.
    let bar_area = Rect::new(area.x, area.y, area.width, 1.min(area.height));

    frame.render_widget(Clear, bar_area);

    let spans = vec![
        Span::styled(
            " / ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}\u{2588}", query),
            Style::default().fg(theme.text),
        ),
        Span::styled(
            format!("  ({} matches)", result_count),
            Style::default().fg(theme.text_dim),
        ),
    ];

    let line = Line::from(spans);
    let para = Paragraph::new(line).style(Style::default().bg(theme.surface));
    frame.render_widget(para, bar_area);
}
