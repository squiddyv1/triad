//! The widgets. Drawing reads `App` and never fetches, so every pane stays dumb and the
//! layout can be re-flowed without touching the polling or input code.

mod cairn;
mod detail;
mod header;
mod list;

use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Page, Target};
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

    match app.page() {
        Page::Cairn => cairn::draw(frame, body_area, app),
        Page::Dashboard => {
            let [left_area, detail_area] =
                Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(24)])
                    .areas(body_area);
            list::draw(frame, left_area, app);
            detail::draw(frame, detail_area, app);
        }
    }

    draw_footer(frame, footer_area, app);
}

fn draw_footer(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let mut spans = match app.page() {
        Page::Cairn => {
            let mut spans: Vec<Span> = vec![Span::styled(
                "esc close   tab pane   ↑/↓ scroll   g/G top/end   r refetch   q quit   ",
                theme::dim(),
            )];
            let pane = match app.cairn_pane() {
                crate::app::CairnPane::Graph => "graph",
                crate::app::CairnPane::Logs => "logs",
            };
            spans.push(Span::styled(format!("pane: {pane}   "), theme::accent()));
            if app.cairn_pane() == crate::app::CairnPane::Logs {
                let tail = if app.log_follow() {
                    "follow: on (tail -f)".to_string()
                } else if app.log_new() > 0 {
                    format!("paused · ↓ {} new", app.log_new())
                } else {
                    "paused · ↑ scrolled".to_string()
                };
                spans.push(Span::styled(tail, theme::dim()));
            }
            spans
        }
        Page::Dashboard => {
            let target = match app.target() {
                Target::Run => "run",
                Target::Cairn => "cairn",
            };
            vec![
                Span::styled(
                    "↑/↓ or k/j select   tab target   enter open   r refresh   q quit   ",
                    theme::dim(),
                ),
                Span::styled(format!("target: {target}"), theme::accent()),
            ]
        }
    };
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

/// Clip one line to `width` characters, eliding with an ellipsis. A value that cannot fit
/// is cut, never allowed to run into the border.
pub fn elide_line(line: Line<'_>, width: usize) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width {
        return Line::from(
            line.spans
                .into_iter()
                .map(|span| Span::styled(span.content.into_owned(), span.style))
                .collect::<Vec<_>>(),
        );
    }
    let keep = width.saturating_sub(1);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut last_style = Style::default();
    for span in line.spans {
        if used >= keep {
            break;
        }
        let content = span.content.into_owned();
        last_style = span.style;
        let count = content.chars().count();
        if used + count <= keep {
            used += count;
            out.push(Span::styled(content, span.style));
        } else {
            let piece: String = content.chars().take(keep - used).collect();
            used = keep;
            out.push(Span::styled(piece, span.style));
        }
    }
    out.push(Span::styled("…", last_style));
    Line::from(out)
}

/// Clip a block of key/value rows to the pane's inner width, leaving a column of air
/// before the border.
pub fn elide_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(1) as usize;
    lines
        .into_iter()
        .map(|line| elide_line(line, inner))
        .collect()
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
