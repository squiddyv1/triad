//! Deterministic layout for the Cairn graph.
//!
//! Nodes are placed in cell coordinates: `x` grows with the node's hop (origin on the
//! left, goal on the right) and `y` spreads the nodes of each layer down the pane. Nothing
//! here reads the clock or a random source, so the same graph and the same pane always
//! produce the same picture. A graph taller than the pane simply grows the layout; the
//! caller scrolls a window over it rather than compressing it into soup.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::data::ProjectGraph;

/// Left/right breathing room, in cell columns.
const MARGIN_X: f64 = 3.0;
/// Vertical breathing room above the first and below the last node, in rows.
const PAD_Y: f64 = 1.5;
/// Minimum rows between two nodes in the same layer.
const ROW_GAP: f64 = 2.0;
/// How far a frontier stub reaches past its node, in columns. Public so the drawing code
/// can fade the same stub the pulse travels along.
pub const FRONTIER_STUB: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Origin,
    Goal,
    Fact,
    Hint,
}

impl Kind {
    fn of(name: &str) -> Self {
        match name {
            "origin" => Kind::Origin,
            "goal" => Kind::Goal,
            "hint" => Kind::Hint,
            _ => Kind::Fact,
        }
    }

    /// Origin and goal sit above facts, hints last; a stable order keeps columns steady.
    fn rank(self) -> u8 {
        match self {
            Kind::Origin => 0,
            Kind::Fact => 1,
            Kind::Hint => 2,
            Kind::Goal => 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlacedNode {
    pub id: String,
    pub kind: Kind,
    pub label: String,
    pub x: f64,
    pub y: f64,
    /// Bright palette (on the origin-to-goal path, or an endpoint) versus dim grey.
    pub lit: bool,
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
    /// Total drawing height in cell rows. It may exceed the visible pane; the caller
    /// scrolls a window over it.
    pub height: f64,
    /// The origin-to-goal fact ids, in order; empty when the goal is unreached.
    pub path: Vec<String>,
}

impl Layout {
    /// Rows the layout needs beyond the visible pane; 0 means it fits.
    pub fn max_scroll(&self, visible: u16) -> u16 {
        let total = self.height.ceil() as i64;
        (total - visible as i64).max(0).min(u16::MAX as i64) as u16
    }
}

/// Build the layout for `width` columns and a pane at least `min_height` rows tall.
pub fn layout(graph: &ProjectGraph, width: f64, min_height: f64) -> Layout {
    let width = width.max(8.0);
    let min_height = min_height.max(1.0);

    let path: Vec<String> = graph.path.iter().map(|step| step.fact.clone()).collect();
    let path_set: HashSet<&str> = path.iter().map(String::as_str).collect();

    // Layer non-hint nodes by hop. BTreeMap gives ascending hops; the sort inside a layer
    // is kind-then-id so two frames can never disagree.
    let mut layers: BTreeMap<i64, Vec<&crate::data::GraphNode>> = BTreeMap::new();
    let mut hint_ids: Vec<&crate::data::GraphNode> = Vec::new();
    for node in &graph.nodes {
        if Kind::of(&node.kind) == Kind::Hint {
            hint_ids.push(node);
            continue;
        }
        layers.entry(node.hop).or_default().push(node);
    }
    for column in layers.values_mut() {
        column.sort_by(|a, b| {
            Kind::of(&a.kind)
                .rank()
                .cmp(&Kind::of(&b.kind).rank())
                .then_with(|| a.id.cmp(&b.id))
        });
    }
    hint_ids.sort_by(|a, b| a.id.cmp(&b.id));

    let max_hop = layers.keys().copied().max().unwrap_or(0);
    let layers_span = (max_hop + 1).max(1) as f64;
    let usable_x = (width - 2.0 * MARGIN_X).max(2.0);
    let step_x = if layers_span > 1.0 {
        usable_x / (layers_span - 1.0)
    } else {
        0.0
    };
    let x_of = |hop: i64| -> f64 {
        if layers_span <= 1.0 {
            width / 2.0
        } else {
            MARGIN_X + hop as f64 * step_x
        }
    };

    // The tallest layer decides how many rows the picture needs; a pane taller than that
    // simply spreads the nodes further apart.
    let widest = layers.values().map(Vec::len).max().unwrap_or(1).max(1);
    let needed = 2.0 * PAD_Y + (widest.saturating_sub(1)) as f64 * ROW_GAP;
    let height = needed.max(min_height);

    // Layered by hop: x advances one column per hop and the layer's centre rides a gentle
    // arc, so a chain is a diagonal journey rather than one flat row and single-node
    // layers still use the pane's height. This is what stops the picture being the neat
    // single column the user rejected, without resorting to a random force layout.
    let centre_y = |hop: i64| -> f64 {
        let mid = height / 2.0;
        if layers_span <= 2.0 {
            return mid;
        }
        let t = hop.clamp(0, max_hop) as f64 / (layers_span - 1.0);
        let amplitude = (mid - PAD_Y).max(0.0) * 0.72;
        mid - (std::f64::consts::PI * t).sin() * amplitude
    };

    // Nodes of a layer spread around its centre, and the centre is clamped so even the
    // widest layer stays inside the pane.
    let place = |index: usize, count: usize, hop: i64| -> f64 {
        if count <= 1 {
            return centre_y(hop).clamp(PAD_Y, height - PAD_Y);
        }
        let half = (height - 2.0 * PAD_Y) / 2.0;
        let centre = centre_y(hop).clamp(PAD_Y + half, height - PAD_Y - half);
        centre - half + index as f64 * (2.0 * half) / (count - 1) as f64
    };

    let mut nodes: Vec<PlacedNode> = Vec::new();
    let mut pos: HashMap<String, (f64, f64)> = HashMap::new();
    for (hop, column) in &layers {
        for (index, node) in column.iter().enumerate() {
            let kind = Kind::of(&node.kind);
            let x = x_of(*hop);
            let y = place(index, column.len(), *hop);
            let endpoint = matches!(kind, Kind::Origin | Kind::Goal);
            let lit = endpoint || path_set.contains(node.id.as_str());
            pos.insert(node.id.clone(), (x, y));
            nodes.push(PlacedNode {
                id: node.id.clone(),
                kind,
                label: node.label.clone(),
                x,
                y,
                lit,
            });
        }
    }

    // Hints have no edge and the payload gives them no fact to hang from, so they float in
    // a short column just past the origin: visible as amber, never wired in as if their
    // connections existed. They are nudged clear of the spine so a hint never hides a path
    // edge.
    if !hint_ids.is_empty() {
        let x = x_of(0) + (step_x * 0.30).clamp(1.5, 4.0);
        let count = hint_ids.len();
        let spine = centre_y(0);
        for (index, node) in hint_ids.iter().enumerate() {
            let base = if count <= 1 {
                height / 2.0
            } else {
                PAD_Y + index as f64 * (height - 2.0 * PAD_Y) / (count - 1) as f64
            };
            let y = if (base - spine).abs() < 2.0 {
                (base + 2.0).min(height - PAD_Y)
            } else {
                base
            };
            pos.insert(node.id.clone(), (x, y));
            nodes.push(PlacedNode {
                id: node.id.clone(),
                kind: Kind::Hint,
                label: node.label.clone(),
                x,
                y,
                lit: false,
            });
        }
    }

    let mut edges: Vec<PlacedEdge> = Vec::new();
    // Two open intents can leave the same fact; fan their stubs so neither hides the
    // other (and so the pulse has two distinguishable tracks).
    let mut fan: HashMap<(i64, i64), f64> = HashMap::new();
    for edge in &graph.edges {
        let sources: Vec<(f64, f64)> = edge
            .from
            .iter()
            .filter_map(|id| pos.get(id).copied())
            .collect();
        if sources.is_empty() {
            continue;
        }
        let target = edge.to.as_deref().and_then(|to| pos.get(to).copied());
        let lit = target.is_some_and(|_| {
            edge.to.as_deref().is_some_and(|to| path_set.contains(to))
                && edge
                    .from
                    .iter()
                    .any(|from| path_set.contains(from.as_str()))
        });
        // A frontier reaches forward and away from the spine, so it is never mistaken for
        // a concluded edge and never lands on top of one.
        let stubs = if target.is_none() {
            sources
                .iter()
                .map(|(sx, sy)| {
                    let key = ((sx * 10.0).round() as i64, (sy * 10.0).round() as i64);
                    let seen = fan.entry(key).or_insert(0.0);
                    let step = *seen;
                    *seen += 1.0;
                    let down = if *sy <= height / 2.0 { 1.0 } else { -1.0 };
                    (
                        sx + FRONTIER_STUB * 0.7,
                        sy + down * (FRONTIER_STUB * 0.6 + step * 1.1),
                    )
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
        height,
        path,
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
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{GraphEdge, GraphNode, GraphProject, PathStep};

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
                node("h1", "hint", 0),
                node("goal", "goal", 2),
            ],
            edges: vec![
                edge("i1", &["origin"], Some("f001"), "concluded"),
                edge("i2", &["f001"], None, "unclaimed"),
            ],
            counts: Default::default(),
            path: vec![
                PathStep {
                    fact: "origin".into(),
                    ..Default::default()
                },
                PathStep {
                    fact: "f001".into(),
                    ..Default::default()
                },
                PathStep {
                    fact: "goal".into(),
                    ..Default::default()
                },
            ],
        }
    }

    fn node(id: &str, kind: &str, hop: i64) -> GraphNode {
        GraphNode {
            id: id.into(),
            kind: kind.into(),
            label: id.into(),
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
        let a = layout(&g, 80.0, 20.0);
        let b = layout(&g, 80.0, 20.0);
        let dump = |l: &Layout| {
            l.nodes
                .iter()
                .map(|n| (n.id.clone(), n.x, n.y, n.lit))
                .collect::<Vec<_>>()
        };
        assert_eq!(dump(&a), dump(&b));
    }

    #[test]
    fn hops_spread_on_x_and_goal_is_last() {
        let l = layout(&graph(), 80.0, 20.0);
        let origin = l.nodes.iter().find(|n| n.id == "origin").unwrap();
        let fact = l.nodes.iter().find(|n| n.id == "f001").unwrap();
        let goal = l.nodes.iter().find(|n| n.id == "goal").unwrap();
        assert!(origin.x < fact.x && fact.x < goal.x);
        assert!(origin.lit && goal.lit && fact.lit);
    }

    #[test]
    fn frontier_has_no_target_and_path_edge_does() {
        let l = layout(&graph(), 80.0, 20.0);
        let concluded = l.edges.iter().find(|e| e.concluded).unwrap();
        let frontier = l.edges.iter().find(|e| e.is_frontier()).unwrap();
        assert!(concluded.target.is_some() && concluded.lit);
        assert!(frontier.target.is_none() && !frontier.lit);
    }

    #[test]
    fn tall_layer_grows_the_layout_instead_of_overlapping() {
        let mut g = graph();
        for index in 0..12 {
            g.nodes.push(node(&format!("f{index:03}"), "fact", 1));
        }
        let l = layout(&g, 80.0, 10.0);
        assert!(l.height > 10.0);
        assert!(l.max_scroll(10) > 0);
    }
}
