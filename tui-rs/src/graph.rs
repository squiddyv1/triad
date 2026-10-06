//! Deterministic layout for the Cairn graph.
//!
//! The picture is built once per graph and per pane size. There is no clock and no random
//! source: the same graph and the same pane always produce the same cells, and the layout
//! never reads the pulse, so a frame can animate the frontier without moving a single
//! marker or label.
//!
//! Nodes are placed by a seeded force-directed relax (seeded on a fixed circle, relaxed a
//! fixed number of times) so connected hops sit close together and the drawing spreads
//! across both axes instead of collapsing into one row. Each node then carries a marker
//! footprint and, when one can be found without covering a marker or another label, a
//! static label position.

use std::collections::{HashMap, HashSet};

use crate::data::{GraphNode, ProjectGraph};

/// How far a frontier stub reaches from its node, in cells. Public so the drawing code
/// fades the same stub the pulse travels along.
pub const FRONTIER_STUB: f64 = 6.0;
/// Longest label written beside a node.
pub const LABEL_WIDTH: usize = 20;
/// Minimum separation between two marker centres, in columns and rows, before rounding.
/// Chosen so that after rounding no two marker footprints can touch and merge in the
/// measurement's dense-core clustering.
const MIN_DX: f64 = 6.0;
const MIN_DY: f64 = 5.0;
/// Fixed relax budget: the same every time, so the picture is reproducible.
const ITER: usize = 320;
/// Keeps the drawing off the pane border.
const MARGIN: f64 = 2.0;
/// Above this many non-hint nodes the picture stops being a graph and becomes a hairball
/// of merged markers. Rather than draw that, the pane says so.
const MAX_DRAW_NODES: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Origin,
    Goal,
    Fact,
    Hint,
}

impl Kind {
    pub fn of(name: &str) -> Self {
        match name {
            "origin" => Kind::Origin,
            "goal" => Kind::Goal,
            "hint" => Kind::Hint,
            _ => Kind::Fact,
        }
    }

    /// Origin and goal sit above facts, hints last; a stable order keeps the relax steady.
    fn rank(self) -> u8 {
        match self {
            Kind::Origin => 0,
            Kind::Fact => 1,
            Kind::Hint => 2,
            Kind::Goal => 3,
        }
    }

    /// Marker footprint in cells. Every kind is at least 3 wide and 3 tall, and every
    /// footprint yields a dense core of at least eight cells: a node is a blob, not a dot.
    pub fn size(self) -> (i32, i32) {
        match self {
            Kind::Origin => (5, 4),
            Kind::Goal => (4, 4),
            Kind::Fact => (4, 4),
            Kind::Hint => (3, 4),
        }
    }

    /// The fill glyph: hints get a lighter shade so they read as annotations.
    pub fn glyph(self) -> char {
        match self {
            Kind::Hint => '▓',
            _ => '█',
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlacedNode {
    pub id: String,
    pub kind: Kind,
    pub label: String,
    /// Marker centre, in pane cell columns and rows.
    pub x: f64,
    pub y: f64,
    pub mw: i32,
    pub mh: i32,
    /// Bright palette (on the origin-to-goal path, or an endpoint) versus dim grey.
    pub lit: bool,
    /// Top-left of the label, when one could be placed beside the node.
    pub label_at: Option<(i32, i32)>,
}

impl PlacedNode {
    /// The marker footprint as `(col, row, width, height)`.
    pub fn marker_rect(&self) -> (i32, i32, i32, i32) {
        (
            self.x.round() as i32 - self.mw / 2,
            self.y.round() as i32 - self.mh / 2,
            self.mw,
            self.mh,
        )
    }
}

#[derive(Debug, Clone)]
pub struct PlacedEdge {
    /// One point per source fact: a converging intent draws a line from each.
    pub sources: Vec<(f64, f64)>,
    pub target: Option<(f64, f64)>,
    /// For a frontier edge, the end of the stub leaving each source. Empty when the edge
    /// has concluded.
    pub stubs: Vec<(f64, f64)>,
    pub concluded: bool,
    /// A working intent is claimed; an unclaimed one is only a direction.
    pub working: bool,
    pub lit: bool,
}

impl PlacedEdge {
    pub fn is_frontier(&self) -> bool {
        self.target.is_none()
    }
}

#[derive(Debug)]
pub struct Layout {
    pub nodes: Vec<PlacedNode>,
    pub edges: Vec<PlacedEdge>,
    /// Set when the graph is too large to draw as nodes; the pane shows this instead of a
    /// hairball. The counts and the legend still describe the graph.
    pub fallback: Option<String>,
}

impl Layout {
    /// The layout always fits the visible pane, so there is nothing to scroll.
    pub fn max_scroll(&self, _visible: u16) -> u16 {
        0
    }
}

/// Build the layout for `width` columns and a pane `height` rows tall.
pub fn layout(graph: &ProjectGraph, width: f64, min_height: f64) -> Layout {
    let width = width.max(24.0);
    let height = min_height.max(8.0);

    let path: Vec<String> = graph.path.iter().map(|step| step.fact.clone()).collect();
    let path_set: HashSet<&str> = path.iter().map(String::as_str).collect();

    let mut hints: Vec<&GraphNode> = graph
        .nodes
        .iter()
        .filter(|node| Kind::of(&node.kind) == Kind::Hint)
        .collect();
    hints.sort_by(|a, b| a.id.cmp(&b.id));

    let mut ordered: Vec<&GraphNode> = graph
        .nodes
        .iter()
        .filter(|node| Kind::of(&node.kind) != Kind::Hint)
        .collect();
    ordered.sort_by(|a, b| {
        a.hop
            .cmp(&b.hop)
            .then_with(|| Kind::of(&a.kind).rank().cmp(&Kind::of(&b.kind).rank()))
            .then_with(|| a.id.cmp(&b.id))
    });

    let n = ordered.len();
    if n == 0 {
        return Layout {
            nodes: Vec::new(),
            edges: Vec::new(),
            fallback: None,
        };
    }
    if n > MAX_DRAW_NODES {
        return Layout {
            nodes: Vec::new(),
            edges: Vec::new(),
            fallback: Some(format!(
                "graph has {n} nodes — too many to draw legibly in {}x{} cells",
                width as i32, height as i32
            )),
        };
    }

    let index: HashMap<&str, usize> = ordered
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();

    // Edges between plottable nodes, deduplicated and in graph order.
    let mut links: Vec<(usize, usize)> = Vec::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for edge in &graph.edges {
        let Some(to) = edge.to.as_deref() else {
            continue;
        };
        let Some(&ti) = index.get(to) else { continue };
        for from in &edge.from {
            if let Some(&fi) = index.get(from.as_str()) {
                if fi != ti && seen.insert((fi, ti)) {
                    links.push((fi, ti));
                }
            }
        }
    }

    // --- seeded force-directed relax ---------------------------------------------
    let centre = (width / 2.0, height / 2.0);
    let mut pos: Vec<(f64, f64)> = Vec::with_capacity(n);
    let seed_r = (width.min(height * 2.2) * 0.22).max(3.0);
    for (i, node) in ordered.iter().enumerate() {
        if Kind::of(&node.kind) == Kind::Origin {
            pos.push(centre);
        } else {
            let angle =
                -std::f64::consts::FRAC_PI_2 + 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            pos.push((
                centre.0 + seed_r * angle.cos(),
                centre.1 + seed_r * 0.8 * angle.sin(),
            ));
        }
    }

    let origin_index = ordered
        .iter()
        .position(|node| Kind::of(&node.kind) == Kind::Origin);

    let area = width * height;
    let k = (area / n as f64).sqrt().max(4.0);
    let mut temp = (width.min(height * 3.0) * 0.10).max(2.0);
    for _ in 0..ITER {
        let mut disp = vec![(0.0_f64, 0.0_f64); n];
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let force = k * k / d;
                disp[i].0 += dx / d * force;
                disp[i].1 += dy / d * force;
                disp[j].0 -= dx / d * force;
                disp[j].1 -= dy / d * force;
            }
        }
        for &(u, v) in &links {
            let dx = pos[u].0 - pos[v].0;
            let dy = pos[u].1 - pos[v].1;
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            // Springs a touch stronger than the textbook relax, so a chain stays a chain
            // rather than being pulled apart by the repulsion of everything else.
            let force = d * d / k * 2.5;
            disp[u].0 -= dx / d * force;
            disp[u].1 -= dy / d * force;
            disp[v].0 += dx / d * force;
            disp[v].1 += dy / d * force;
        }
        for i in 0..n {
            if Some(i) == origin_index {
                // The origin is the root of the picture and stays at the pane centre.
                disp[i] = (0.0, 0.0);
                pos[i] = centre;
                continue;
            }
            disp[i].0 += (centre.0 - pos[i].0) * 0.03;
            disp[i].1 += (centre.1 - pos[i].1) * 0.03;
            let (dx, dy) = disp[i];
            let d = (dx * dx + dy * dy).sqrt();
            if d > 0.001 {
                let step = temp.min(d);
                pos[i].0 += dx / d * step;
                pos[i].1 += dy / d * step;
            }
            pos[i].0 = pos[i].0.clamp(MARGIN, width - MARGIN);
            pos[i].1 = pos[i].1.clamp(MARGIN, height - MARGIN);
        }
        temp = (temp * 0.985).max(0.05);
    }
    separate(&mut pos, width, height);

    // Stretch the cluster to occupy the pane, keeping the shape the relax found. The
    // bounds cap the distortion so a two-node graph does not become a straight line
    // across the screen. The stretch runs about the origin, so the root stays central.
    let (minx, maxx, miny, maxy) = marker_bounds(&ordered, &pos);
    let spanx = (maxx - minx).max(1.0);
    let spany = (maxy - miny).max(1.0);
    let target_w = if hints.is_empty() {
        width * 0.74
    } else {
        width * 0.66
    };
    let target_h = height * 0.74;
    let fit_x = (target_w / spanx).clamp(0.65, 2.4);
    let fit_y = (target_h / spany).clamp(0.65, 2.4);
    let (midx, midy) = match origin_index {
        Some(i) => pos[i],
        None => ((minx + maxx) / 2.0, (miny + maxy) / 2.0),
    };
    for p in pos.iter_mut() {
        p.0 = midx + (p.0 - midx) * fit_x;
        p.1 = midy + (p.1 - midy) * fit_y;
    }
    separate(&mut pos, width, height);

    // Whole cells from here on: markers and labels must land on real terminal cells.
    for p in pos.iter_mut() {
        p.0 = p.0.round().clamp(MARGIN, width - MARGIN);
        p.1 = p.1.round().clamp(MARGIN, height - MARGIN);
    }

    let mut nodes: Vec<PlacedNode> = ordered
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let kind = Kind::of(&node.kind);
            let endpoint = matches!(kind, Kind::Origin | Kind::Goal);
            let (mw, mh) = kind.size();
            PlacedNode {
                id: node.id.clone(),
                kind,
                label: node.label.clone(),
                x: pos[i].0,
                y: pos[i].1,
                mw,
                mh,
                lit: endpoint || path_set.contains(node.id.as_str()),
                label_at: None,
            }
        })
        .collect();

    // --- hints in the margin beside the cluster ----------------------------------
    let hint_on_left = place_hints(&mut nodes, &hints, height);

    // Centre the markers before choosing labels, so the margin beside a hint is the room
    // it will really have once the picture is centred. A final recentre after the labels
    // only slides the whole drawing.
    recenter(&mut nodes, width as i32, height as i32);

    // --- labels, computed from the static layout only ----------------------------
    let mut forbidden = forbidden_cells(&nodes, graph);
    place_labels(
        &mut nodes,
        hint_on_left,
        &mut forbidden,
        width as i32,
        height as i32,
    );
    recenter(&mut nodes, width as i32, height as i32);

    // Frontier stubs point away from the cluster, away from any edge already leaving the
    // node and away from the label just placed, so a stub never doubles a link into a
    // thick stroke and never runs over its own label.
    let stub_dirs = stub_directions(&nodes, graph);

    // --- edges from the final positions ------------------------------------------
    let edge_pos: HashMap<&str, (f64, f64)> = nodes
        .iter()
        .map(|node| (node.id.as_str(), (node.x, node.y)))
        .collect();
    let mut edges: Vec<PlacedEdge> = Vec::new();
    for (edge_index, edge) in graph.edges.iter().enumerate() {
        let sources: Vec<(f64, f64)> = edge
            .from
            .iter()
            .filter_map(|id| edge_pos.get(id.as_str()).copied())
            .collect();
        if sources.is_empty() {
            continue;
        }
        let target = edge.to.as_deref().and_then(|to| edge_pos.get(to).copied());
        let lit = target.is_some_and(|_| {
            edge.to.as_deref().is_some_and(|to| path_set.contains(to))
                && edge
                    .from
                    .iter()
                    .any(|from| path_set.contains(from.as_str()))
        });
        let stubs = if target.is_none() {
            sources
                .iter()
                .enumerate()
                .filter_map(|(k, &(sx, sy))| {
                    let dir = stub_dirs.get(edge_index).and_then(|d| d.get(k))?;
                    Some((sx + dir.0 * FRONTIER_STUB, sy + dir.1 * FRONTIER_STUB))
                })
                .collect()
        } else {
            Vec::new()
        };
        edges.push(PlacedEdge {
            sources,
            target,
            stubs,
            concluded: edge.is_concluded(),
            working: edge.status == "working",
            lit,
        });
    }

    Layout {
        nodes,
        edges,
        fallback: None,
    }
}

/// Pick a direction for each frontier stub: out from the cluster, and as far as possible
/// from the edges already attached to that node. This keeps a spoke distinct from a link
/// instead of drawing a second parallel stroke next to it.
fn stub_directions(nodes: &[PlacedNode], graph: &ProjectGraph) -> Vec<Vec<(f64, f64)>> {
    let pos: HashMap<&str, (f64, f64)> = nodes
        .iter()
        .map(|node| (node.id.as_str(), (node.x, node.y)))
        .collect();
    let by_id: HashMap<&str, &PlacedNode> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut incidence: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    for edge in &graph.edges {
        let Some(to) = edge.to.as_deref() else {
            continue;
        };
        let Some(&to_pos) = pos.get(to) else { continue };
        for from in &edge.from {
            let Some(&from_pos) = pos.get(from.as_str()) else {
                continue;
            };
            let dir = unit(from_pos, to_pos);
            incidence.entry(from.clone()).or_default().push(dir);
            incidence
                .entry(to.to_string())
                .or_default()
                .push((-dir.0, -dir.1));
        }
    }
    let mut cx = 0.0;
    let mut cy = 0.0;
    for node in nodes {
        cx += node.x;
        cy += node.y;
    }
    let centroid = (
        cx / nodes.len().max(1) as f64,
        cy / nodes.len().max(1) as f64,
    );
    let mut used: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    let mut out: Vec<Vec<(f64, f64)>> = Vec::with_capacity(graph.edges.len());
    for edge in &graph.edges {
        if edge.to.is_some() {
            out.push(Vec::new());
            continue;
        }
        let mut dirs = Vec::new();
        for from in &edge.from {
            let Some(&source) = pos.get(from.as_str()) else {
                continue;
            };
            let base = unit(centroid, source);
            let mut avoid = incidence.get(from).cloned().unwrap_or_default();
            avoid.extend(used.get(from).cloned().unwrap_or_default());
            if let Some(side) = by_id
                .get(from.as_str())
                .and_then(|node| label_direction(node))
            {
                avoid.push(side);
            }
            let dir = clearest_direction(base, &avoid);
            used.entry(from.clone()).or_default().push(dir);
            dirs.push(dir);
        }
        out.push(dirs);
    }
    out
}

/// The unit vector from `a` to `b`, or downward when the two points coincide.
fn unit(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        (0.0, 1.0)
    } else {
        (dx / len, dy / len)
    }
}

/// Roughly which side of its marker a node's label sits on, for routing a stub the other
/// way.
fn label_direction(node: &PlacedNode) -> Option<(f64, f64)> {
    let (c, r, w, h) = node.marker_rect();
    let (lc, lr) = node.label_at?;
    let mid_x = f64::from(c) + f64::from(w - 1) / 2.0;
    let mid_y = f64::from(r) + f64::from(h - 1) / 2.0;
    let len = node.label.chars().count() as f64;
    let label_x = f64::from(lc) + (len - 1.0) / 2.0;
    let label_y = f64::from(lr);
    let dx = label_x - mid_x;
    let dy = label_y - mid_y;
    if dx.abs() >= dy.abs() {
        Some(if dx >= 0.0 { (1.0, 0.0) } else { (-1.0, 0.0) })
    } else {
        Some(if dy >= 0.0 { (0.0, 1.0) } else { (0.0, -1.0) })
    }
}

/// The candidate direction nearest `base` that keeps the widest angle from every avoided
/// direction. Twelve fixed candidates make the choice deterministic.
fn clearest_direction(base: (f64, f64), avoid: &[(f64, f64)]) -> (f64, f64) {
    if avoid.is_empty() {
        return base;
    }
    let base_angle = base.1.atan2(base.0);
    let mut best = base;
    let mut best_score = f64::MIN;
    for k in 0..12 {
        let angle = base_angle + k as f64 * std::f64::consts::PI / 6.0;
        let dir = (angle.cos(), angle.sin());
        let min_d = avoid
            .iter()
            .map(|d| (dir.0 * d.0 + dir.1 * d.1).clamp(-1.0, 1.0).acos())
            .fold(f64::MAX, f64::min);
        // A whisker of downward bias breaks a tie toward the room below a node, leaving
        // the above slot free for a label (labels are tried above before below).
        let score = min_d + 0.001 * dir.1;
        if score > best_score + 1e-9 {
            best_score = score;
            best = dir;
        }
    }
    best
}

/// Push marker centres apart until no two are within the minimum box. Deterministic:
/// the pair order and the direction chosen from each offset are fixed.
fn separate(pos: &mut [(f64, f64)], width: f64, height: f64) {
    for _ in 0..120 {
        let mut moved = false;
        for i in 0..pos.len() {
            for j in (i + 1)..pos.len() {
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                if dx.abs() >= MIN_DX || dy.abs() >= MIN_DY {
                    continue;
                }
                moved = true;
                let push_x = (MIN_DX - dx.abs()) / 2.0 + 0.1;
                let push_y = (MIN_DY - dy.abs()) / 2.0 + 0.1;
                let sx = if dx.abs() < 1e-6 {
                    if i < j {
                        1.0
                    } else {
                        -1.0
                    }
                } else if dx >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                let sy = if dy.abs() < 1e-6 {
                    if i < j {
                        1.0
                    } else {
                        -1.0
                    }
                } else if dy >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                pos[i].0 += sx * push_x;
                pos[j].0 -= sx * push_x;
                pos[i].1 += sy * push_y;
                pos[j].1 -= sy * push_y;
            }
        }
        for p in pos.iter_mut() {
            p.0 = p.0.clamp(MARGIN, width - MARGIN);
            p.1 = p.1.clamp(MARGIN, height - MARGIN);
        }
        if !moved {
            break;
        }
    }
}

/// The bounding box of the marker footprints for `nodes` at `pos`.
fn marker_bounds(nodes: &[&GraphNode], pos: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    let mut minx = f64::MAX;
    let mut maxx = f64::MIN;
    let mut miny = f64::MAX;
    let mut maxy = f64::MIN;
    for (i, node) in nodes.iter().enumerate() {
        let (w, h) = Kind::of(&node.kind).size();
        let hw = f64::from(w) / 2.0;
        let hh = f64::from(h) / 2.0;
        minx = minx.min(pos[i].0 - hw);
        maxx = maxx.max(pos[i].0 + hw);
        miny = miny.min(pos[i].1 - hh);
        maxy = maxy.max(pos[i].1 + hh);
    }
    (minx, maxx, miny, maxy)
}

/// Place hint markers in the margin just outside the cluster. Returns whether they landed
/// on the left (which the label placement uses to face the labels outward).
fn place_hints(nodes: &mut Vec<PlacedNode>, hints: &[&GraphNode], height: f64) -> bool {
    if hints.is_empty() {
        return true;
    }
    let (minx, maxx, miny, maxy) = nodes.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(x0, x1, y0, y1), node| {
            let (c, r, w, h) = node.marker_rect();
            (
                x0.min(f64::from(c)),
                x1.max(f64::from(c + w - 1)),
                y0.min(f64::from(r)),
                y1.max(f64::from(r + h - 1)),
            )
        },
    );
    let (hw, hh) = Kind::Hint.size();
    let gap = 2.0;
    let left = minx - gap - f64::from(hw) / 2.0;
    let on_left = left >= MARGIN + f64::from(hw) / 2.0;
    let column = if on_left {
        left
    } else {
        maxx + gap + f64::from(hw) / 2.0
    };
    let step = f64::from(hh) + 2.0;
    let total = hints.len() as f64 * step - 2.0;
    let start = ((miny + maxy) / 2.0 - total / 2.0).max(MARGIN);
    for (k, hint) in hints.iter().enumerate() {
        let y = (start + k as f64 * step).min(height - MARGIN);
        nodes.push(PlacedNode {
            id: hint.id.clone(),
            kind: Kind::Hint,
            label: hint.label.clone(),
            x: column,
            y,
            mw: hw,
            mh: hh,
            lit: false,
            label_at: None,
        });
    }
    on_left
}

/// The cells a label must keep clear of: a one-cell moat around every marker and a
/// one-cell moat around every edge and stub. The layout is static, so this is exact and
/// the pulse never enters into it. Keeping text off the strokes stops a label from
/// clustering into a dense core beside a node, which would read as a second marker.
fn forbidden_cells(nodes: &[PlacedNode], graph: &ProjectGraph) -> HashSet<(i32, i32)> {
    let pos: HashMap<&str, (f64, f64)> = nodes
        .iter()
        .map(|node| (node.id.as_str(), (node.x, node.y)))
        .collect();
    let mut out: HashSet<(i32, i32)> = HashSet::new();
    for node in nodes {
        let (c, r, w, h) = node.marker_rect();
        for y in (r - 1)..=(r + h) {
            for x in (c - 1)..=(c + w) {
                out.insert((x, y));
            }
        }
    }
    // Only concluded links are known when labels are placed; the frontier stubs are
    // routed afterwards, around whatever label was chosen.
    for edge in &graph.edges {
        let Some(target) = edge.to.as_deref().and_then(|to| pos.get(to).copied()) else {
            continue;
        };
        for from in &edge.from {
            let Some(&source) = pos.get(from.as_str()) else {
                continue;
            };
            raster_line(
                (source.0.round() as i32, source.1.round() as i32),
                (target.0.round() as i32, target.1.round() as i32),
                &mut out,
            );
        }
    }
    out
}

/// Mark a line's cells and their eight neighbours. A touch coarse, but the one-cell moat
/// is exactly the margin a label needs to stay out of a stroke.
fn raster_line(a: (i32, i32), b: (i32, i32), out: &mut HashSet<(i32, i32)>) {
    let (mut x0, mut y0) = a;
    let (x1, y1) = b;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        for ddy in -1..=1 {
            for ddx in -1..=1 {
                out.insert((x0 + ddx, y0 + ddy));
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// Choose a label cell for every node that can have one. A label is dropped rather than
/// drawn over a marker, an edge or another label, and only the static layout is
/// consulted, so the pulse can never move a label.
fn place_labels(
    nodes: &mut [PlacedNode],
    hint_on_left: bool,
    forbidden: &mut HashSet<(i32, i32)>,
    width: i32,
    height: i32,
) {
    let marker_rects: Vec<(i32, i32, i32, i32)> =
        nodes.iter().map(PlacedNode::marker_rect).collect();

    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by_key(|&i| {
        let kind = nodes[i].kind;
        let priority = match kind {
            Kind::Origin => 0,
            Kind::Goal => 1,
            Kind::Fact if nodes[i].lit => 2,
            Kind::Fact => 3,
            Kind::Hint => 4,
        };
        (priority, i)
    });

    for i in order {
        let full = crate::graph::short_label(&nodes[i].label, LABEL_WIDTH);
        let (c0, r0, w, h) = marker_rects[i];
        let midrow = r0 + (h - 1) / 2;
        let mut chosen = None;
        'widths: for width_cap in [LABEL_WIDTH, 15, 11, 8, 5] {
            let text = crate::graph::short_label(&full, width_cap);
            let len = text.chars().count() as i32;
            if len < 3 {
                continue;
            }
            let centred = c0 + (w - len).max(0) / 2;
            let right = (c0 + w + 1, midrow);
            let left = (c0 - len - 1, midrow);
            let mut offsets: Vec<(i32, i32)> = Vec::new();
            if nodes[i].kind == Kind::Hint && hint_on_left {
                offsets.push(left);
                offsets.push(right);
            } else {
                offsets.push(right);
                offsets.push(left);
            }
            offsets.push((centred, r0 - 2));
            offsets.push((centred, r0 + h + 1));
            for (col, row) in offsets {
                if col < 0 || row < 0 || col + len > width || row >= height {
                    continue;
                }
                let clear = (col..col + len).all(|x| !forbidden.contains(&(x, row)));
                if !clear {
                    continue;
                }
                chosen = Some((text, col, row));
                break 'widths;
            }
        }
        if let Some((text, col, row)) = chosen {
            nodes[i].label = text;
            nodes[i].label_at = Some((col, row));
            for x in (col - 1)..=(col + nodes[i].label.chars().count() as i32) {
                forbidden.insert((x, row - 1));
                forbidden.insert((x, row));
                forbidden.insert((x, row + 1));
            }
        }
    }
}

/// Shift everything so the ink (markers and labels) is centred in the pane, clamped so
/// nothing is pushed off an edge.
fn recenter(nodes: &mut [PlacedNode], width: i32, height: i32) {
    let mut minx = i32::MAX;
    let mut maxx = i32::MIN;
    let mut miny = i32::MAX;
    let mut maxy = i32::MIN;
    let mut grow = |c: i32, r: i32, w: i32, h: i32| {
        minx = minx.min(c);
        maxx = maxx.max(c + w - 1);
        miny = miny.min(r);
        maxy = maxy.max(r + h - 1);
    };
    for node in nodes.iter() {
        let (c, r, w, h) = node.marker_rect();
        grow(c, r, w, h);
        if let Some((lc, lr)) = node.label_at {
            let len = node.label.chars().count() as i32;
            grow(lc, lr, len, 1);
        }
    }
    if minx > maxx {
        return;
    }
    // Centre unconditionally. Clamping to keep every cell inside the pane would panic
    // when the ink is wider than the pane (a graph too large to draw); overflow is
    // clipped by the drawing code instead.
    let dx = width / 2 - (minx + maxx) / 2;
    let dy = height / 2 - (miny + maxy) / 2;
    if dx == 0 && dy == 0 {
        return;
    }
    for node in nodes.iter_mut() {
        node.x = (node.x.round() as i32 + dx) as f64;
        node.y = (node.y.round() as i32 + dy) as f64;
        if let Some((lc, lr)) = node.label_at {
            node.label_at = Some((lc + dx, lr + dy));
        }
    }
}

/// A short, honest node label: one line, clipped with an ellipsis when it would not fit.
pub fn short_label(text: &str, width: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    // Do not leave a trailing space before the ellipsis: it would strand the mark as a
    // lone coloured cell, the one thing the measurement counts as noise.
    while out.ends_with(' ') {
        out.pop();
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{GraphEdge, GraphNode, GraphProject};

    fn rects_overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
        a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
    }

    fn graph() -> ProjectGraph {
        ProjectGraph {
            project: GraphProject {
                id: "p".into(),
                title: "t".into(),
                status: "active".into(),
            },
            nodes: vec![
                node("origin", "origin", 0),
                node("f001", "fact", 1),
                node("f002", "fact", 2),
                node("goal", "goal", 3),
                node("h001", "hint", 0),
                node("h002", "hint", 0),
            ],
            edges: vec![
                edge("i1", &["origin"], Some("f001"), "concluded"),
                edge("i2", &["f001"], Some("f002"), "concluded"),
                edge("i3", &["f002"], None, "unclaimed"),
                edge("i4", &["f001"], None, "unclaimed"),
                edge("i5", &["origin"], None, "unclaimed"),
            ],
            counts: Default::default(),
            path: vec![],
        }
    }

    fn node(id: &str, kind: &str, hop: i64) -> GraphNode {
        GraphNode {
            id: id.into(),
            kind: kind.into(),
            label: format!("{id} label text"),
            status: kind.into(),
            hop,
        }
    }

    fn edge(id: &str, from: &[&str], to: Option<&str>, status: &str) -> GraphEdge {
        GraphEdge {
            id: id.into(),
            from: from.iter().map(|s| s.to_string()).collect(),
            to: to.map(str::to_string),
            status: status.into(),
            worker: None,
            label: id.into(),
        }
    }

    #[test]
    fn same_graph_and_size_is_identical() {
        let g = graph();
        let a = layout(&g, 130.0, 21.0);
        let b = layout(&g, 130.0, 21.0);
        let dump = |l: &Layout| {
            l.nodes
                .iter()
                .map(|n| (n.id.clone(), n.x, n.y, n.label_at, n.lit))
                .collect::<Vec<_>>()
        };
        assert_eq!(dump(&a), dump(&b));
    }

    #[test]
    fn every_node_has_a_marker_and_nodes_stay_apart() {
        let l = layout(&graph(), 130.0, 21.0);
        assert_eq!(l.nodes.len(), 6);
        for i in 0..l.nodes.len() {
            for j in (i + 1)..l.nodes.len() {
                let dx = (l.nodes[i].x - l.nodes[j].x).abs();
                let dy = (l.nodes[i].y - l.nodes[j].y).abs();
                assert!(
                    dx >= 5.0 || dy >= 4.0,
                    "{} and {} too close: {dx},{dy}",
                    l.nodes[i].id,
                    l.nodes[j].id
                );
            }
        }
    }

    #[test]
    fn drawing_uses_both_axes() {
        let l = layout(&graph(), 130.0, 21.0);
        let xs = l
            .nodes
            .iter()
            .map(|n| n.x)
            .fold((f64::MAX, f64::MIN), |a, x| (a.0.min(x), a.1.max(x)));
        let ys = l
            .nodes
            .iter()
            .map(|n| n.y)
            .fold((f64::MAX, f64::MIN), |a, y| (a.0.min(y), a.1.max(y)));
        assert!(xs.1 - xs.0 > 40.0);
        assert!(ys.1 - ys.0 > 8.0);
    }

    #[test]
    fn frontier_has_no_target_and_path_edge_does() {
        let l = layout(&graph(), 130.0, 21.0);
        let concluded = l.edges.iter().find(|e| e.concluded).unwrap();
        let frontier = l.edges.iter().find(|e| e.is_frontier()).unwrap();
        assert!(concluded.target.is_some());
        assert!(frontier.target.is_none());
    }

    #[test]
    fn too_many_nodes_falls_back_instead_of_a_hairball() {
        let mut g = graph();
        for index in 0..40 {
            g.nodes.push(node(&format!("x{index:03}"), "fact", 1));
        }
        let l = layout(&g, 130.0, 21.0);
        assert!(l.nodes.is_empty());
        assert!(l.edges.is_empty());
        assert!(l.fallback.is_some());
    }

    #[test]
    fn labels_never_cover_a_marker() {
        let l = layout(&graph(), 130.0, 21.0);
        let markers: Vec<(i32, i32, i32, i32)> =
            l.nodes.iter().map(PlacedNode::marker_rect).collect();
        for node in &l.nodes {
            if let Some((c, r)) = node.label_at {
                let rect = (c, r, node.label.chars().count() as i32, 1);
                assert!(
                    !markers.iter().any(|m| rects_overlap(*m, rect)),
                    "label for {} covers a marker",
                    node.id
                );
            }
        }
    }
}
