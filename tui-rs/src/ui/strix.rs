//! The Strix detail modal: a three-line header over the FINDINGS pane and the AGENT
//! STREAM pane. Drawing only -- the payload, the scroll offsets and the follow state all
//! live in `App`, so a key press and a frame can never disagree about how tall a pane is.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{self, App, DetailPane};
use crate::strix;
use crate::theme;
use crate::ui::{inner_height, panel};

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let inner_width = app.detail_inner_width();
    let header_rows = app.detail_header_rows();
    let banner_rows = app.detail_banner_rows();
    let [header_area, banner_area, panes_area] = Layout::vertical([
        Constraint::Length(header_rows),
        Constraint::Length(banner_rows),
        Constraint::Min(0),
    ])
    .areas(area);

    draw_header(frame, header_area, app, inner_width);
    draw_banners(frame, banner_area, app);
    if app.detail().is_some() {
        draw_panes(frame, panes_area, app, inner_width);
    }
}

/// The header: three clamped rows when there is a payload, else one line saying why there
/// is not. The current activity rides on the first row, a spinner leading it while the
/// scan runs and `‖ paused` once it is stopped.
fn draw_header(frame: &mut Frame, area: Rect, app: &App, width: usize) {
    let Some(detail) = app.detail() else {
        let (text, style) = match app.detail_error() {
            Some(error) => (format!(" {error}"), Style::new().fg(Color::Red)),
            None => (" loading the Strix detail…".to_string(), theme::dim()),
        };
        frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
        return;
    };

    let state = app.detail_state();
    let activity = if app.detail_live() {
        if app.detail_paused() {
            Some("‖ paused".to_string())
        } else {
            Some(format!(
                "{} {}",
                app.spinner_frame(),
                detail.activity().unwrap_or("working")
            ))
        }
    } else {
        None
    };
    let lines = strix::build_status(detail, width, &state, app.detail_pid(), activity.as_deref());
    frame.render_widget(Paragraph::new(lines), area);
}

/// The one-line banners above the panes: a failed fetch keeps the last good panes below it,
/// and a run with no `agents.db` yet says so rather than drawing an empty stream.
fn draw_banners(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || app.detail().is_none() {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(error) = app.detail_error() {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(error.to_string(), Style::new().fg(Color::Red)),
        ]));
    }
    if app.detail_no_db() {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "no agents.db yet for this run",
                Style::new().fg(Color::Yellow),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_panes(frame: &mut Frame, area: Rect, app: &App, width: usize) {
    let Some(detail) = app.detail() else {
        return;
    };
    let (upper, lower) = app::detail_pane_split(area.height);
    let [findings_area, stream_area] =
        Layout::vertical([Constraint::Length(upper), Constraint::Length(lower)]).areas(area);

    let findings_lines = strix::build_findings(detail, width);
    let findings_height = inner_height(findings_area);
    let findings_offset = app
        .detail_findings_scroll()
        .min(findings_lines.len().saturating_sub(findings_height));
    let findings_mark =
        strix::overflow_mark(findings_offset, findings_lines.len(), findings_height);
    let findings_title = with_mark(
        format!("FINDINGS ({})", detail.findings_count()),
        &findings_mark,
    );
    draw_pane(
        frame,
        findings_area,
        &findings_title,
        findings_lines,
        findings_offset,
        app.detail_pane() == DetailPane::Findings,
    );

    let stream_lines = strix::build_stream(detail, width);
    let stream_height = inner_height(stream_area);
    let stream_offset = app
        .detail_scroll()
        .min(stream_lines.len().saturating_sub(stream_height));
    let stream_mark = strix::overflow_mark(stream_offset, stream_lines.len(), stream_height);
    let stream_title = with_mark(
        format!("AGENT STREAM ({})", detail.messages.len()),
        &stream_mark,
    );
    draw_pane(
        frame,
        stream_area,
        &stream_title,
        stream_lines,
        stream_offset,
        app.detail_pane() == DetailPane::Stream,
    );
}

/// `FINDINGS (12)` becomes `FINDINGS (12)  12/145 ↓ 133 more`; a pane that fits is left
/// alone.
fn with_mark(title: String, mark: &str) -> String {
    if mark.is_empty() {
        title
    } else {
        format!("{title}  {mark}")
    }
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    offset: usize,
    focused: bool,
) {
    let block = panel(title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = inner.height as usize;
    let window: Vec<Line<'static>> = lines.into_iter().skip(offset).take(height).collect();
    frame.render_widget(Paragraph::new(window), inner);
}
