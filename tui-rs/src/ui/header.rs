//! The one-line header: `TRIAD` and the spinner on the left, stack health on the right.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;
use crate::ui::spans_width;

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let mut left: Vec<Span> = Vec::new();
    if app.animating() {
        let live = app.runs().iter().filter(|r| r.live || r.paused).count();
        left.push(Span::styled(app.spinner_frame(), theme::accent()));
        left.push(Span::raw(" "));
        left.push(Span::styled("TRIAD", theme::bold().fg(Color::Cyan)));
        left.push(Span::styled(
            format!(" scanning · {live} live"),
            theme::dim(),
        ));
    } else {
        left.push(Span::styled("TRIAD", theme::bold().fg(Color::Cyan)));
    }

    let mut right: Vec<Span> = Vec::new();
    match app.snapshot() {
        Some(snapshot) => {
            right.push(Span::styled(snapshot.cairn.base.clone(), theme::dim()));
            right.push(Span::raw(" "));
            right.push(status_word(snapshot.cairn.up));
            right.push(Span::styled("  dispatcher ", theme::dim()));
            right.push(status_word(snapshot.dispatcher.alive));
        }
        None => right.push(Span::styled("loading…", theme::dim())),
    }

    // Space-between by hand: the layout owns the width, the header only pads to it.
    let width = area.width as usize;
    let left_len = spans_width(&left);
    let right_len = spans_width(&right);
    let mut spans = left;
    if left_len + right_len < width {
        spans.push(Span::raw(" ".repeat(width - left_len - right_len)));
    }
    spans.extend(right);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn status_word(up: bool) -> Span<'static> {
    if up {
        Span::styled("up", Style::new().fg(Color::Green))
    } else {
        Span::styled("DOWN", Style::new().fg(Color::Red))
    }
}
