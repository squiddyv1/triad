//! The colour vocabulary the Ink dashboard settled on, in one place so every widget
//! agrees on what "running" or "critical" looks like.

use ratatui::style::{Color, Modifier, Style};

pub fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}

/// Run state: running green, completed cyan, paused yellow, stopped/failed red, stale yellow.
pub fn state_colour(state: &str) -> Color {
    match state {
        "running" => Color::Green,
        "completed" => Color::Cyan,
        "paused" => Color::Yellow,
        "stopped" | "failed" | "timeout" => Color::Red,
        "quiet" => Color::Cyan,
        "stale" => Color::Yellow,
        _ => Color::Gray,
    }
}

/// Findings by severity: critical magenta, high red, medium yellow, low cyan, info gray.
pub fn severity_colour(severity: &str) -> Color {
    match severity.to_ascii_lowercase().as_str() {
        "critical" => Color::Magenta,
        "high" => Color::Red,
        "medium" => Color::Yellow,
        "low" => Color::Cyan,
        "info" | "informational" => Color::Gray,
        _ => Color::Gray,
    }
}

/// Project status: an active project reads green, anything else yellow.
pub fn project_colour(status: &str) -> Color {
    if status == "active" {
        Color::Green
    } else {
        Color::Yellow
    }
}
