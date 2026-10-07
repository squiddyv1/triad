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
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine, Points};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{self, App, CairnPane};
use crate::graph::{Kind, Layout as GraphLayout, PlacedNode};
use crate::theme;
use crate::ui::{elide_line, panel};

/// Pulse positions along a frontier stub, so the marker visibly moves between frames.
const PULSE_STEPS: u64 = 10;
/// Radius of the small open ring a frontier stub ends in, in cells.
const STUB_RING: f64 = 0.45;

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
            Span::styled("no project linked to this run; run ", theme::dim()),
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
    if let Some(note) = &layout.fallback {
        // Too many nodes to draw as a graph: say so instead of rendering a hairball.
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("  {note}"),
                    Style::new().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    "  open the project in Cairn to explore it",
                    theme::dim(),
                )),
            ]),
            area,
        );
        return;
    }
    let height = area.height as f64;
    let width = area.width as f64;
    let pulse = app.pulse();
    let pulsing = app.pulse_active();

    // Node positions are cell columns and rows from the top-left of the pane. Braille
    // addresses a cell centre at `col + 0.5`, and the canvas y axis grows upward, so a
    // row `r` sits at `height - r - 0.5`. Nothing here is scrolled: the layout fits.
    let cx = |col: f64| col + 0.5;
    let cy = |row: f64| height - row - 0.5;

    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, width])
        .y_bounds([0.0, height])
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
                        // A frontier is a short spoke leaving its node and fading out,
                        // never a line to nowhere. It ends in a small open ring so the
                        // stop reads as deliberate, and one hairline keeps it plainly
                        // distinct from the solid concluded links.
                        let (sx, sy) = *source;
                        let (ex, ey) = *stub;
                        for step in 0..3 {
                            let from = step as f64 / 3.0;
                            let to = (step + 1) as f64 / 3.0;
                            ctx.draw(&CanvasLine::new(
                                cx(sx + (ex - sx) * from),
                                cy(sy + (ey - sy) * from),
                                cx(sx + (ex - sx) * to),
                                cy(sy + (ey - sy) * to),
                                fade(base, 1.0 - from * 0.6),
                            ));
                        }
                        draw_ring(ctx, cx(ex), cy(ey), base);
                        if pulsing {
                            let fraction = ((pulse + index as u64 * 3) % (PULSE_STEPS + 1)) as f64
                                / PULSE_STEPS as f64;
                            let (mx, my) =
                                (cx(sx + (ex - sx) * fraction), cy(sy + (ey - sy) * fraction));
                            let marker = [(mx, my), (mx - 0.3, my), (mx + 0.3, my)];
                            ctx.draw(&Points::new(&marker, theme::PULSE));
                        }
                    }
                } else if let Some((tx, ty)) = edge.target {
                    // A concluded edge is a continuous link whose ends the marker blocks
                    // then cover, so it plainly terminates at two nodes. It is drawn
                    // several dot-rows thick so the eye reads a solid stroke, not dashes.
                    for (sx, sy) in &edge.sources {
                        thick_link(ctx, cx(*sx), cy(*sy), cx(tx), cy(ty), color);
                    }
                }
            }
        });
    frame.render_widget(canvas, area);

    draw_markers(frame.buffer_mut(), area, layout);
    draw_labels(frame.buffer_mut(), area, layout);
}

/// Draw a link as a solid band: sample along the stroke and, in each cell the stroke passes
/// through, raise three dots stacked inside that one cell. Keeping the band to a single cell
/// row is what makes it read as a solid line without inflating into a node-sized blob.
fn thick_link(ctx: &mut Context, x0: f64, y0: f64, x1: f64, y1: f64, color: Color) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let steps = (len * 4.0).ceil().max(1.0) as i32;
    let mut dots: Vec<(f64, f64)> = Vec::with_capacity((steps as usize + 1) * 3);
    for s in 0..=steps {
        let t = f64::from(s) / f64::from(steps);
        let x = x0 + dx * t;
        let row = (y0 + dy * t).floor();
        dots.push((x, row + 0.2));
        dots.push((x, row + 0.5));
        dots.push((x, row + 0.8));
    }
    ctx.draw(&Points::new(&dots, color));
}

/// A small open ring drawn at the end of a frontier stub, so the stub stops on purpose
/// rather than looking like a link that ran out of ink.
fn draw_ring(ctx: &mut Context, x: f64, y: f64, color: Color) {
    let ring = [
        (x, y + STUB_RING),
        (x + STUB_RING, y),
        (x, y - STUB_RING),
        (x - STUB_RING, y),
    ];
    ctx.draw(&Points::new(&ring, color));
}

/// The visible slice of `text` when its start column `c0` is clipped to the pane's `[x0, x1)`.
fn clip(text: &str, c0: i32, x0: i32, x1: i32) -> String {
    text.chars()
        .skip((x0 - c0) as usize)
        .take((x1 - x0) as usize)
        .collect()
}

/// A node is a filled block of cells, at least 3x3, over the edges already drawn. The
/// blocks are drawn here rather than on the canvas so a node always has real weight and a
/// footprint the eye can find.
fn draw_markers(buf: &mut Buffer, area: Rect, layout: &GraphLayout) {
    for node in &layout.nodes {
        let (c0, r0, w, h) = node.marker_rect();
        let style = Style::new().fg(node_colour(node));
        let fill = node.kind.glyph().to_string().repeat(w.max(0) as usize);
        for row in 0..h {
            let y = r0 + row;
            if y < 0 || y >= i32::from(area.height) {
                continue;
            }
            let x0 = c0.max(0);
            let x1 = (c0 + w).min(i32::from(area.width));
            if x1 <= x0 {
                continue;
            }
            let text = clip(&fill, c0, x0, x1);
            buf.set_string(area.x + x0 as u16, area.y + y as u16, &text, style);
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

fn node_colour(node: &PlacedNode) -> Color {
    match node.kind {
        Kind::Origin => theme::ORIGIN,
        Kind::Goal => theme::GOAL,
        Kind::Hint => theme::HINT,
        Kind::Fact if node.lit => theme::PATH_NODE,
        Kind::Fact => theme::DIM_NODE,
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

/// Labels are written at the static position the layout chose, and only there. The
/// placement consulted the marker footprints, never the rendered canvas, so a label cannot
/// move when the frontier pulse advances between frames.
fn draw_labels(buf: &mut Buffer, area: Rect, layout: &GraphLayout) {
    for node in &layout.nodes {
        let Some((c0, r0)) = node.label_at else {
            continue;
        };
        let length = node.label.chars().count() as i32;
        if length <= 0 || r0 < 0 || r0 >= i32::from(area.height) {
            continue;
        }
        let x0 = c0.max(0);
        let x1 = (c0 + length).min(i32::from(area.width));
        if x1 <= x0 {
            continue;
        }
        let text = clip(&node.label, c0, x0, x1);
        buf.set_string(
            area.x + x0 as u16,
            area.y + r0 as u16,
            &text,
            label_style(node),
        );
    }
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
        Span::styled("● fact (dim off-path) ", Style::new().fg(theme::PATH_NODE)),
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
