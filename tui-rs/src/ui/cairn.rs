//! The Cairn page: the project graph drawn with a Braille canvas over the Cairn log tail.
//!
//! The page is read-only, like the rest of the dashboard. It reads `App` and never
//! fetches; `App` owns the scroll offsets and the graph layout so a key press and a frame
//! can never disagree about how tall the picture is.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Painter, Points, Shape};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{self, App, CairnPane};
use crate::graph::{Kind, Layout as GraphLayout, PlacedNode};
use crate::theme;
use crate::ui::elide_line;

/// Pulse positions along a frontier stub, so the marker visibly moves between frames.
const PULSE_STEPS: u64 = 10;
/// Longest node label that may be written on the canvas.
const LABEL_WIDTH: usize = 18;

pub fn draw(frame: &mut Frame, body: Rect, app: &App) {
    let (graph_rows, log_rows) = app::cairn_split_body(body.height);
    let [graph_area, log_area] =
        Layout::vertical([Constraint::Length(graph_rows), Constraint::Length(log_rows)])
            .areas(body);
    draw_graph(frame, graph_area, app);
    draw_logs(frame, log_area, app);
}

fn draw_graph(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.cairn_pane() == CairnPane::Graph;
    let title = match app.graph() {
        Some(graph) => format!(
            "GRAPH ({} facts · {} intents)",
            graph.counts.facts, graph.counts.intents
        ),
        None if app.graph_error().is_some() => "GRAPH (unavailable)".to_string(),
        None => "GRAPH".to_string(),
    };
    let block = panel(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [canvas_area, legend_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(app::CAIRN_LEGEND_ROWS),
    ])
    .areas(inner);

    match app.graph_layout() {
        Some(layout) => {
            draw_canvas(frame, canvas_area, layout, app);
        }
        None => {
            let message = graph_message(app);
            frame.render_widget(Paragraph::new(message), inner);
        }
    }
    legend(frame, legend_area, app);
}

/// Why there is no picture, in the order the reader needs it: no link first (never invite
/// a duplicate), then Cairn being down, then the ordinary loading state.
fn graph_message(app: &App) -> Line<'static> {
    let linked = app.selected_run().and_then(|run| run.project.as_deref());
    if linked.is_none() {
        return Line::from(vec![
            Span::raw("  "),
            Span::styled("no project linked to this run — run ", theme::dim()),
            Span::styled("triad engage", theme::accent()),
            Span::styled(" to link one", theme::dim()),
        ]);
    }
    if let Some(error) = app.graph_error() {
        return Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("Cairn is not answering: {error}"),
                Style::new().fg(Color::Red),
            ),
        ]);
    }
    Line::from(vec![
        Span::raw("  "),
        Span::styled("loading the graph…", theme::dim()),
    ])
}

fn draw_canvas(frame: &mut Frame, area: Rect, layout: &GraphLayout, app: &App) {
    let scroll = app.graph_scroll() as f64;
    let height = area.height as f64;
    let width = area.width as f64;
    let pulse = app.pulse();
    let pulsing = app.pulse_active();

    // `App` places nodes in cell rows counted from the top of the whole (possibly
    // scrolled) layout. The canvas y axis is flipped and bounded to the visible window, so
    // a node at absolute row `r` lands at y = 2*scroll + height - r.
    let cy = |row: f64| 2.0 * scroll + height - row;

    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, width])
        .y_bounds([scroll, scroll + height])
        .paint(|ctx| {
            for (index, edge) in layout.edges.iter().enumerate() {
                let color = edge_colour(edge);
                if edge.is_frontier() {
                    let base = if edge.working {
                        theme::WORK_EDGE
                    } else {
                        theme::DIM_EDGE
                    };
                    for (source, stub) in edge.sources.iter().zip(edge.stubs.iter()) {
                        // A frontier is a stub leaving its node and fading out, never a
                        // line to nowhere, and angled away from the spine.
                        let (sx, sy) = *source;
                        let (ex, ey) = *stub;
                        for step in 0..3 {
                            let from = step as f64 / 3.0;
                            let to = (step + 1) as f64 / 3.0;
                            ctx.draw(&CanvasLine::new(
                                sx + (ex - sx) * from,
                                cy(sy + (ey - sy) * from),
                                sx + (ex - sx) * to,
                                cy(sy + (ey - sy) * to),
                                fade(base, 1.0 - from * 0.6),
                            ));
                        }
                        if pulsing {
                            let fraction = ((pulse + index as u64 * 3) % (PULSE_STEPS + 1)) as f64
                                / PULSE_STEPS as f64;
                            let (mx, my) =
                                (sx + (ex - sx) * fraction, cy(sy + (ey - sy) * fraction));
                            let marker = [(mx, my), (mx - 0.3, my), (mx + 0.3, my)];
                            ctx.draw(&Points::new(&marker, theme::PULSE));
                        }
                    }
                } else if let Some((tx, ty)) = edge.target {
                    for (sx, sy) in &edge.sources {
                        ctx.draw(&CanvasLine::new(*sx, cy(*sy), tx, cy(ty), color));
                    }
                }
            }

            for node in &layout.nodes {
                match node.kind {
                    Kind::Origin | Kind::Goal => {
                        let color = if node.kind == Kind::Origin {
                            theme::ORIGIN
                        } else {
                            theme::GOAL
                        };
                        ctx.draw(&Disc::new(node.x, cy(node.y), 0.75, 0.38, color));
                    }
                    Kind::Hint => {
                        ctx.draw(&Disc::new(node.x, cy(node.y), 0.45, 0.22, theme::HINT));
                    }
                    Kind::Fact => {
                        let color = if node.lit {
                            theme::PATH_NODE
                        } else {
                            theme::DIM_NODE
                        };
                        // A fact is a point: a filled dot reads as one on a Braille grid,
                        // where a single pseudo-pixel would be easy to miss.
                        ctx.draw(&Disc::new(node.x, cy(node.y), 0.4, 0.2, color));
                    }
                }
            }
        });
    frame.render_widget(canvas, area);

    draw_labels(frame.buffer_mut(), area, layout, app.graph_scroll());
}

/// A filled ellipse. `Circle` cannot correct for the 2:1 cell aspect of a Braille dot
/// grid, so origin, goal and hints use this to stay visually round.
struct Disc {
    x: f64,
    y: f64,
    rx: f64,
    ry: f64,
    color: Color,
}

impl Disc {
    const fn new(x: f64, y: f64, rx: f64, ry: f64, color: Color) -> Self {
        Self {
            x,
            y,
            rx,
            ry,
            color,
        }
    }
}

impl Shape for Disc {
    fn draw(&self, painter: &mut Painter) {
        let nx = ((self.rx * 8.0).ceil() as i32).max(1);
        let ny = ((self.ry * 8.0).ceil() as i32).max(1);
        for iy in -ny..=ny {
            for ix in -nx..=nx {
                let dx = ix as f64 / nx as f64;
                let dy = iy as f64 / ny as f64;
                if dx * dx + dy * dy <= 1.0 {
                    if let Some((x, y)) =
                        painter.get_point(self.x + dx * self.rx, self.y + dy * self.ry)
                    {
                        painter.paint(x, y, self.color);
                    }
                }
            }
        }
    }
}

/// `lit` edges (on the origin-to-goal path) are bright; everything else recedes to grey.
fn edge_colour(edge: &crate::graph::PlacedEdge) -> Color {
    if edge.lit {
        theme::PATH_EDGE
    } else if edge.working || edge.concluded {
        theme::DIM_EDGE
    } else {
        theme::DIM_EDGE_FAINT
    }
}

fn fade(color: Color, factor: f64) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            (f64::from(r) * factor) as u8,
            (f64::from(g) * factor) as u8,
            (f64::from(b) * factor) as u8,
        ),
        other => other,
    }
}

/// Labels exist only for the path, origin and goal. A label is dropped rather than drawn
/// over an edge or another label: the canvas has already been rendered, so a target cell
/// that is not blank is a braille dot and the label must find another spot or go without.
fn draw_labels(buf: &mut Buffer, area: Rect, layout: &GraphLayout, scroll: u16) {
    let mut candidates: Vec<&PlacedNode> = Vec::new();
    if let Some(node) = layout.nodes.iter().find(|n| n.kind == Kind::Origin) {
        candidates.push(node);
    }
    if let Some(node) = layout.nodes.iter().find(|n| n.kind == Kind::Goal) {
        candidates.push(node);
    }
    for id in &layout.path {
        if let Some(node) = layout
            .nodes
            .iter()
            .find(|n| &n.id == id && n.kind == Kind::Fact)
        {
            if !candidates.iter().any(|placed| placed.id == node.id) {
                candidates.push(node);
            }
        }
    }

    for node in candidates {
        let text = crate::graph::short_label(&node.label, LABEL_WIDTH);
        let length = text.chars().count();
        if length == 0 {
            continue;
        }
        let visible_row = node.y.round() as i32 - scroll as i32;
        if visible_row < 0 || visible_row >= i32::from(area.height) {
            continue;
        }
        let column = node.x.round() as i32;
        // Try beside the node first, then progressively further above/below, always on
        // either side. The first placement that covers nothing wins; none means no label.
        let mut offsets: Vec<(i32, i32)> = vec![(2, 0), (-(length as i32) - 2, 0)];
        for dy in [1, -1, 2, -2, 3, -3, 4, -4, 5, -5] {
            offsets.push((2, dy));
            offsets.push((-(length as i32) - 2, dy));
        }
        for (dx, dy) in offsets {
            let row = visible_row + dy;
            let start = column + dx;
            if row < 0
                || row >= i32::from(area.height)
                || start < 0
                || start + length as i32 > i32::from(area.width)
            {
                continue;
            }
            let x0 = area.x + start as u16;
            let y0 = area.y + row as u16;
            if (0..length as u16).any(|i| !is_blank(buf, x0 + i, y0)) {
                continue;
            }
            buf.set_string(x0, y0, &text, label_style(node));
            break;
        }
    }
}

fn is_blank(buf: &Buffer, x: u16, y: u16) -> bool {
    buf.cell((x, y)).is_none_or(|cell| cell.symbol() == " ")
}

fn label_style(node: &PlacedNode) -> Style {
    let color = match node.kind {
        Kind::Origin => theme::ORIGIN,
        Kind::Goal => theme::GOAL,
        Kind::Fact if node.lit => theme::PATH_NODE,
        _ => theme::DIM_NODE,
    };
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

/// The legend under the canvas: what the symbols mean, and the one-line path summary.
fn legend(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("● fact ", Style::new().fg(theme::PATH_NODE)),
        Span::styled("✦ intent ", Style::new().fg(theme::PATH_EDGE)),
        Span::styled("○ hint ", Style::new().fg(theme::HINT)),
        Span::styled("◉ origin ", Style::new().fg(theme::ORIGIN)),
        Span::styled("◎ goal", Style::new().fg(theme::GOAL)),
        Span::styled("   ", theme::dim()),
    ];
    let summary = match app.graph() {
        Some(graph) if graph.path.is_empty() => {
            Span::styled("path: none yet (goal unreached)", theme::dim())
        }
        Some(graph) => {
            let ids: Vec<&str> = graph.path.iter().map(|step| step.fact.as_str()).collect();
            Span::styled(
                format!("path: {}", ids.join(" → ")),
                Style::new().fg(Color::White),
            )
        }
        None => Span::styled("path: unknown", theme::dim()),
    };
    spans.push(summary);

    if let Some(graph) = app.graph() {
        if graph.counts.open > 0 {
            spans.push(Span::styled(
                format!("   {} open", graph.counts.open),
                Style::new().fg(theme::WORK_EDGE),
            ));
        }
    }
    let line = elide_line(Line::from(spans), area.width.saturating_sub(1) as usize);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_logs(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.cairn_pane() == CairnPane::Logs;
    let source = if app.logs_error().is_some() {
        "unavailable".to_string()
    } else {
        app.logs()
            .map(|logs| logs.source.clone())
            .unwrap_or_else(|| "loading".to_string())
    };
    let block = panel(&format!("CAIRN LOGS ({source})"), focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(error) = app.logs_error() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("  cannot fetch the Cairn log tail: {error}"),
                    Style::new().fg(Color::Red),
                )),
                Line::from(Span::styled("  r retries", theme::dim())),
            ]),
            inner,
        );
        return;
    }

    let Some(logs) = app.logs() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  loading the log tail…",
                theme::dim(),
            ))),
            inner,
        );
        return;
    };

    if logs.source == "none" {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  no Cairn log source: no dispatcher, server or container is running",
                theme::dim(),
            ))),
            inner,
        );
        return;
    }
    if logs.lines.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  (no log lines yet)",
                theme::dim(),
            ))),
            inner,
        );
        return;
    }

    let height = inner.height as usize;
    let start = app
        .log_scroll()
        .min(logs.lines.len().saturating_sub(height));
    let lines: Vec<Line> = logs
        .lines
        .iter()
        .skip(start)
        .take(height)
        .map(|raw| {
            Line::from(Span::styled(
                format!(" {}", raw.replace('\t', "  ")),
                log_style(raw),
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn log_style(line: &str) -> Style {
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR")
        || upper.contains("TRACEBACK")
        || upper.contains("EXCEPTION")
        || upper.contains("CRITICAL")
    {
        Style::new().fg(Color::Red)
    } else if upper.contains("WARN") {
        Style::new().fg(Color::Yellow)
    } else if upper.contains("INFO") || upper.contains("DEBUG") {
        theme::dim()
    } else {
        Style::default()
    }
}

/// A bordered panel whose title brightens when its pane owns the keys.
fn panel(title: &str, focused: bool) -> Block<'static> {
    let marker = if focused { "▸ " } else { "" };
    let colour = if focused { Color::Cyan } else { Color::White };
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" {marker}{title} "),
            theme::bold().fg(colour),
        ))
}
