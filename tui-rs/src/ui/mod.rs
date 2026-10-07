//! The widgets. Drawing reads `App` and never fetches, so every pane stays dumb and the
//! layout can be re-flowed without touching the polling or input code.

mod cairn;
mod detail;
mod form;
mod header;
mod list;
mod strix;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, DetailPane, MessageKind, Page, Target};
use crate::theme;

/// Width of the left column: wide enough for a 23-char run name, a 9-char state and the
/// elapsed time, as the Ink layout settled on.
pub const LEFT_WIDTH: u16 = 46;

/// Worst first, so a critical never hides behind an info.
pub const SEVERITY_ORDER: [&str; 5] = ["critical", "high", "medium", "low", "info"];

/// The dashboard's own keys, before the shared stack/list tail.
const DASHBOARD_KEYS: &str = "n new   ↑/↓ or k/j select   tab target   enter open   r refresh";
/// The dashboard key line on a narrow terminal: no stack keys, condensed select hint.
const DASHBOARD_KEYS_SHORT: &str =
    "n new   ↑/↓ select   tab target   enter open   r refresh   c lists   q quit   ";
/// The two modal pages share this prefix.
const MODAL_KEYS: &str = "esc close   tab pane   ↑/↓ scroll   g/G top/end   r refetch";
/// The stack, collapse and quit keys every page ends with.
const STACK_KEYS: &str = "   u up   x down   c lists   q quit   ";

pub fn draw(frame: &mut Frame, app: &App) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(app.footer_height()),
    ])
    .areas(frame.area());

    header::draw(frame, header_area, app);

    match app.page() {
        Page::Cairn => cairn::draw(frame, body_area, app),
        Page::Detail => {
            let (left_area, modal_area) = body_columns(body_area);
            list::draw(frame, left_area, app);
            strix::draw(frame, modal_area, app);
        }
        Page::Form => {
            let (left_area, form_area) = body_columns(body_area);
            list::draw(frame, left_area, app);
            form::draw(frame, form_area, app);
        }
        Page::Dashboard => {
            let (left_area, detail_area) = body_columns(body_area);
            list::draw(frame, left_area, app);
            detail::draw(frame, detail_area, app);
        }
    }

    draw_footer(frame, footer_area, app);
}

/// The body split every left-column page uses: the list at `LEFT_WIDTH`, the rest to the
/// right.
fn body_columns(body: Rect) -> (Rect, Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(24)]).areas(body);
    (left, right)
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    // The stack confirmation wins over a status line, and both stack above the key line,
    // exactly as the Ink footer renders them.
    let mut lines: Vec<Line> = Vec::new();
    if app.page() != Page::Form {
        if app.pending_stack_down() {
            lines.push(Line::from(Span::styled(
                "stop the stack? (y/n)",
                Style::new().fg(Color::Yellow),
            )));
        } else if let Some(message) = app.message() {
            let (prefix, colour) = match message.kind {
                MessageKind::Err => ("✗ ", Color::Red),
                MessageKind::Ok => ("✓ ", Color::Green),
                MessageKind::Info => ("  ", Color::Gray),
            };
            lines.push(Line::from(Span::styled(
                format!("{prefix}{}", message.text),
                Style::new().fg(colour),
            )));
        }
    }

    match app.page() {
        Page::Form => lines.push(Line::from(Span::styled(
            "form open: esc cancels, no other key acts",
            theme::dim(),
        ))),
        Page::Dashboard => {
            let target = match app.target() {
                Target::Run => "run",
                Target::Cairn => "cairn",
            };
            let indicator = format!("target: {target}");
            lines.push(Line::from(vec![
                Span::styled(
                    dashboard_keys(area.width as usize, indicator.chars().count()),
                    theme::dim(),
                ),
                Span::styled(indicator, theme::accent()),
            ]));
        }
        Page::Cairn => {
            let mut spans: Vec<Span> = vec![Span::styled(modal_keys(), theme::dim())];
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
            lines.push(Line::from(spans));
        }
        Page::Detail => {
            let mut spans: Vec<Span> = vec![Span::styled(modal_keys(), theme::dim())];
            let pane = match app.detail_pane() {
                DetailPane::Findings => "findings",
                DetailPane::Stream => "stream",
            };
            spans.push(Span::styled(format!("pane: {pane}   "), theme::accent()));
            if app.detail_pane() == DetailPane::Stream {
                let tail = if app.detail_follow() {
                    "follow: on (tail -f)".to_string()
                } else if app.detail_new() > 0 {
                    format!("paused · ↓ {} new", app.detail_new())
                } else {
                    "paused · ↑ scrolled".to_string()
                };
                spans.push(Span::styled(tail, theme::dim()));
            }
            if app.error().is_some() {
                spans.push(Span::raw("   "));
                spans.push(Span::styled(
                    "last poll failed",
                    Style::new().fg(Color::Red),
                ));
            }
            lines.push(Line::from(spans));
        }
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// The dashboard key line, condensed on a narrow terminal so the target indicator still fits.
fn dashboard_keys(width: usize, indicator: usize) -> String {
    let full = format!("{DASHBOARD_KEYS}{STACK_KEYS}");
    if full.chars().count() + indicator <= width {
        full
    } else {
        DASHBOARD_KEYS_SHORT.to_string()
    }
}

/// The shared key line for the Cairn and Strix modal pages.
fn modal_keys() -> String {
    format!("{MODAL_KEYS}{STACK_KEYS}")
}

/// A bordered panel whose title carries the `enter` target: `▸ ` + cyan when focused, no
/// marker otherwise, the Ink dashboard's own marker.
pub fn panel(title: &str, focused: bool) -> Block<'static> {
    let marker = if focused { "▸ " } else { "" };
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" {marker}{title} "),
            theme::title(focused),
        ))
}

/// The usable rows inside a bordered block.
pub fn inner_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
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
