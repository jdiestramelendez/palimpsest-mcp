//! Knowledge-graph exports: a self-contained HTML viewer (force layout +
//! time slider), Mermaid, and DOT. Everything renders from one
//! [`queries::graph_export`] snapshot; no server, no network.

use crate::db::queries::{ExportEdge, ExportNode};

/// HTML template; `/*DATA*/` is replaced with the graph JSON.
const HTML_TEMPLATE: &str = include_str!("graph_template.html");

/// True when the window `[valid_at, invalid_at)` contains instant `t`
/// (ISO strings in the shared storage format compare lexicographically).
fn window_contains(valid_at: &str, invalid_at: Option<&str>, t: &str) -> bool {
    valid_at <= t && invalid_at.map(|i| i > t).unwrap_or(true)
}

/// A node is alive at `t` when any mentioning memory's window contains it.
fn node_alive(node: &ExportNode, t: &str) -> bool {
    node.windows
        .iter()
        .any(|(v, i)| window_contains(v, i.as_deref(), t))
}

/// Filter a snapshot down to what was true at instant `t`.
fn filter_at<'a>(
    nodes: &'a [ExportNode],
    edges: &'a [ExportEdge],
    t: &str,
) -> (Vec<&'a ExportNode>, Vec<&'a ExportEdge>) {
    let live_edges: Vec<&ExportEdge> = edges
        .iter()
        .filter(|e| window_contains(&e.valid_at, e.invalid_at.as_deref(), t))
        .collect();
    let live_nodes: Vec<&ExportNode> = nodes.iter().filter(|n| node_alive(n, t)).collect();
    (live_nodes, live_edges)
}

/// Render the interactive HTML viewer. `initial_time` seeds the slider
/// (`None` = now, i.e. the slider's right edge).
pub fn render_html(
    nodes: &[ExportNode],
    edges: &[ExportEdge],
    title: &str,
    initial_time: Option<&str>,
) -> String {
    let data = serde_json::json!({
        "title": title,
        "initialTime": initial_time,
        "nodes": nodes,
        "edges": edges,
    });
    HTML_TEMPLATE.replace("/*DATA*/", &data.to_string())
}

/// Sanitize a display name into a Mermaid/DOT-safe label.
fn clean_label(name: &str) -> String {
    name.replace(['"', '\\', '\n', '[', ']', '|', '{', '}'], " ")
        .trim()
        .to_string()
}

/// Render a Mermaid `graph LR` of the graph as of instant `t`.
pub fn render_mermaid(nodes: &[ExportNode], edges: &[ExportEdge], t: &str) -> String {
    let (live_nodes, live_edges) = filter_at(nodes, edges, t);
    let mut out = String::from("graph LR\n");
    for n in &live_nodes {
        out.push_str(&format!("  e{}[\"{}\"]\n", n.id, clean_label(&n.name)));
    }
    for e in &live_edges {
        out.push_str(&format!(
            "  e{} -->|{}| e{}\n",
            e.source,
            clean_label(&e.predicate),
            e.target
        ));
    }
    out
}

/// Render a Graphviz DOT digraph of the graph as of instant `t`.
pub fn render_dot(nodes: &[ExportNode], edges: &[ExportEdge], t: &str) -> String {
    let (live_nodes, live_edges) = filter_at(nodes, edges, t);
    let mut out = String::from("digraph palimpsest {\n  rankdir=LR;\n  node [shape=box, style=rounded];\n");
    for n in &live_nodes {
        out.push_str(&format!(
            "  e{} [label=\"{}\", class=\"{}\"];\n",
            n.id,
            clean_label(&n.name),
            n.kind
        ));
    }
    for e in &live_edges {
        out.push_str(&format!(
            "  e{} -> e{} [label=\"{}\"];\n",
            e.source,
            e.target,
            clean_label(&e.predicate)
        ));
    }
    out.push_str("}\n");
    out
}
