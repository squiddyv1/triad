//! The new-engagement form, ported from the `Form` component in `tui/src/index.tsx`: the
//! five rows, the focused row's marker, the dim placeholders and the validation error line.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::form::{Flow, FormState};
use crate::theme;

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            " NEW ENGAGEMENT ",
            theme::bold().fg(Color::White),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let form = app.form();
    let mut lines: Vec<Line> = vec![
        Line::from(""),
        flow_line(form),
        field_line(
            form,
            1,
            "target",
            &form.target,
            false,
            form.target
                .is_empty()
                .then_some("required (URL or local path)"),
        ),
        field_line(
            form,
            2,
            "title",
            &form.title,
            false,
            form.title
                .is_empty()
                .then_some("required (names the engagement directory)"),
        ),
        field_line(
            form,
            3,
            "goal",
            &form.goal,
            form.flow != Flow::Engage,
            (form.flow == Flow::Engage && form.goal.is_empty()).then_some("required for engage"),
        ),
        mode_line(form),
        Line::from(""),
        Line::from(Span::styled(
            "↑/↓ or tab move   ←/→ or space toggle   enter submit   esc cancel",
            theme::dim(),
        )),
    ];
    if let Some(error) = &form.error {
        lines.push(Line::from(Span::styled(
            format!("✗ {error}"),
            Style::new().fg(Color::Red),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn flow_line(form: &FormState) -> Line<'static> {
    let mut spans = label_spans(form.row, 0, "flow");
    spans.push(marker(form.flow == Flow::Scan));
    spans.push(Span::raw(" scan   "));
    spans.push(marker(form.flow == Flow::Engage));
    spans.push(Span::raw(" engage"));
    Line::from(spans)
}

fn mode_line(form: &FormState) -> Line<'static> {
    let mut spans = label_spans(form.row, 4, "mode");
    spans.push(marker(form.mode == crate::form::ScanMode::Quick));
    spans.push(Span::raw(" quick   "));
    spans.push(marker(form.mode == crate::form::ScanMode::Standard));
    spans.push(Span::raw(" standard   "));
    spans.push(marker(form.mode == crate::form::ScanMode::Deep));
    spans.push(Span::raw(" deep"));
    Line::from(spans)
}

fn field_line(
    form: &FormState,
    row: usize,
    label: &str,
    value: &str,
    value_dim: bool,
    hint: Option<&str>,
) -> Line<'static> {
    let mut spans = label_spans(form.row, row, label);
    spans.push(Span::styled(
        value.to_string(),
        if value_dim {
            theme::dim()
        } else {
            Style::new()
        },
    ));
    if form.row == row {
        spans.push(Span::styled("▏", theme::accent()));
    }
    if let Some(hint) = hint {
        spans.push(Span::styled(format!("   {hint}"), theme::dim()));
    }
    Line::from(spans)
}

/// The `▸ ` marker and cyan bold label on the focused row, a two-space indent otherwise.
fn label_spans(current: usize, row: usize, label: &str) -> Vec<Span<'static>> {
    let text = format!("{label:<7}");
    if row == current {
        let style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        vec![Span::styled("▸ ", style), Span::styled(text, style)]
    } else {
        vec![Span::raw("  "), Span::raw(text)]
    }
}

fn marker(on: bool) -> Span<'static> {
    Span::raw(if on { "◉" } else { "○" })
}
