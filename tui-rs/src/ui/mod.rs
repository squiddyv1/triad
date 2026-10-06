//! The widgets. Drawing reads `App` and never fetches, so every pane stays dumb and the
//! layout can be re-flowed without touching the polling or input code.

mod detail;
mod header;
mod list;

use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;

/// Width of the left column: wide enough for a 23-char run name, a 9-char state and the
/// elapsed time, as the Ink layout settled on.
pub const LEFT_WIDTH: u16 = 46;

/// Worst first, so a critical never hides behind an info.
pub const SEVERITY_ORDER: [&str; 5] = ["critical", "high", "medium", "low", "info"];

pub fn draw(frame: &mut Frame, app: &App) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    header::draw(frame, header_area, app);

    let [left_area, detail_area] =
        Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(24)]).areas(body_area);
    list::draw(frame, left_area, app);
    detail::draw(frame, detail_area, app);

    draw_footer(frame, footer_area, app);
}

fn draw_footer(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let mut spans = vec![Span::styled(
        "↑/↓ or k/j select   r refresh   q quit",
        theme::dim(),
    )];
    if app.error().is_some() {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            "last poll failed",
            Style::new().fg(ratatui::style::Color::Red),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Truncate with an ellipsis or pad to a fixed width, so a row never reflows.
pub fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let keep = width.saturating_sub(1);
        let mut out: String = text.chars().take(keep).collect();
        out.push('…');
        out
    } else {
        let mut out = text.to_string();
        out.push_str(&" ".repeat(width - count));
        out
    }
}

pub fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// A fixed-width filling bar, as the Ink dashboard draws it.
pub fn progress_bar(done: i64, total: i64, width: usize) -> String {
    let filled = if total <= 0 {
        0
    } else {
        ((done as f64 / total as f64) * width as f64).round() as i64
    };
    let filled = filled.clamp(0, width as i64) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}
