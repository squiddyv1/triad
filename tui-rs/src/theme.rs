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

// --- the Cairn graph palette ------------------------------------------------------
//
// RGB rather than the terminal's 16 colours, so the canvas looks the same everywhere the
// reference screenshots were taken. The bright half lights the origin-to-goal path; the
// dim half recedes off-path and unclaimed work. Amber marks hints and the pulse, which is
// deliberately the only warm colour on an otherwise cool graph.

/// Origin and goal, the two prominent circles.
pub const ORIGIN: Color = Color::Rgb(120, 190, 255);
pub const GOAL: Color = Color::Rgb(90, 225, 140);
/// Facts on the path, and the path's edges.
pub const PATH_NODE: Color = Color::Rgb(90, 160, 245);
pub const PATH_EDGE: Color = Color::Rgb(110, 220, 170);
/// Facts off the path.
pub const DIM_NODE: Color = Color::Rgb(96, 102, 118);
/// Edges off the path: concluded a shade brighter than an unclaimed direction.
pub const DIM_EDGE: Color = Color::Rgb(74, 82, 100);
pub const DIM_EDGE_FAINT: Color = Color::Rgb(52, 58, 72);
/// A claimed intent that has not concluded.
pub const WORK_EDGE: Color = Color::Rgb(210, 185, 105);
/// Hints, which are informative but not wired into the graph.
pub const HINT: Color = Color::Rgb(255, 176, 64);
/// The frontier marker travelling along an open edge.
pub const PULSE: Color = Color::Rgb(255, 236, 150);
