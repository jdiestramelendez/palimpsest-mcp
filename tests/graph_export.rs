//! Ticket 006 tests: graph export snapshot, time filtering, and the three
//! output formats.

use palimpsest_mcp::core::graphviz;
use palimpsest_mcp::db::queries::{self, EntityRef, GraphPayload, NewMemory, TripleRef};
use palimpsest_mcp::db::Db;

const DIMS: usize = 4;

fn seed(db: &mut Db) -> (i64, i64) {
    queries::ensure_project(db.conn(), "p").expect("project");
    let insert = |db: &mut Db, content: &str, entities: &[(&str, &str)], triple: (&str, &str, &str), supersedes: Option<i64>| {
        queries::insert_memory(
            db.conn_mut(),
            &NewMemory {
                project_id: Some("p".into()),
                content: content.into(),
                kind: "fact".into(),
                importance: 3,
                source_session_id: None,
            },
            &[0.0; DIMS],
            supersedes,
            Some(&GraphPayload {
                entities: entities
                    .iter()
                    .map(|(n, k)| EntityRef { name: n.to_string(), kind: k.to_string() })
                    .collect(),
                triples: vec![TripleRef {
                    source: triple.0.into(),
                    predicate: triple.1.into(),
                    target: triple.2.into(),
                }],
            }),
        )
        .expect("insert")
    };
    let old = insert(db, "p deploys to Heroku", &[("p", "project"), ("Heroku", "technology")], ("p", "deploys-to", "Heroku"), None);
    let new = insert(db, "p deploys to Fly.io", &[("p", "project"), ("Fly.io", "technology")], ("p", "deploys-to", "Fly.io"), Some(old));
    (old, new)
}

#[test]
fn export_carries_windows_and_scope_includes_globals() {
    let mut db = Db::open_in_memory(DIMS).expect("db");
    seed(&mut db);
    // A global entity via a global memory.
    queries::insert_memory(
        db.conn_mut(),
        &NewMemory {
            project_id: None,
            content: "the user prefers tabs".into(),
            kind: "preference".into(),
            importance: 3,
            source_session_id: None,
        },
        &[0.0; DIMS],
        None,
        Some(&GraphPayload {
            entities: vec![EntityRef { name: "tabs".into(), kind: "concept".into() }],
            triples: vec![],
        }),
    )
    .expect("global insert");

    let (nodes, edges) = queries::graph_export(db.conn(), Some("p")).expect("export");
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"Heroku") && names.contains(&"Fly.io") && names.contains(&"tabs"),
        "project scope must include globals: {names:?}");
    // The superseded edge is exported WITH its closed window.
    let heroku_edge = edges.iter().find(|e| e.evidence.contains("Heroku")).expect("old edge exported");
    assert!(heroku_edge.invalid_at.is_some(), "superseded evidence must carry invalid_at");
    let fly_edge = edges.iter().find(|e| e.evidence.contains("Fly.io")).expect("new edge");
    assert!(fly_edge.invalid_at.is_none());
    assert!(nodes.iter().all(|n| !n.windows.is_empty()), "every node needs windows");
}

#[test]
fn mermaid_and_dot_respect_the_time_filter() {
    let mut db = Db::open_in_memory(DIMS).expect("db");
    seed(&mut db);
    // Backdate the old fact so an intermediate instant exists.
    db.conn()
        .execute(
            "UPDATE memories SET valid_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-100 days')
             WHERE content LIKE '%Heroku%'",
            [],
        )
        .expect("backdate");
    let (nodes, edges) = queries::graph_export(db.conn(), Some("p")).expect("export");

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let past = (chrono::Utc::now() - chrono::Duration::days(50))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    let mmd_now = graphviz::render_mermaid(&nodes, &edges, &now);
    assert!(mmd_now.starts_with("graph LR"), "{mmd_now}");
    assert!(mmd_now.contains("Fly.io") && !mmd_now.contains("Heroku"), "now: {mmd_now}");
    assert!(mmd_now.contains("-->|deploys-to|"));

    let mmd_past = graphviz::render_mermaid(&nodes, &edges, &past);
    assert!(mmd_past.contains("Heroku") && !mmd_past.contains("Fly.io"), "past: {mmd_past}");

    let dot_now = graphviz::render_dot(&nodes, &edges, &now);
    assert!(dot_now.starts_with("digraph") && dot_now.ends_with("}\n"));
    assert!(dot_now.contains("label=\"deploys-to\"") && !dot_now.contains("Heroku"), "{dot_now}");
}

#[test]
fn html_embeds_data_and_is_self_contained() {
    let mut db = Db::open_in_memory(DIMS).expect("db");
    seed(&mut db);
    let (nodes, edges) = queries::graph_export(db.conn(), Some("p")).expect("export");
    let html = graphviz::render_html(&nodes, &edges, "p — knowledge graph", None);
    assert!(!html.contains("/*DATA*/"), "marker must be replaced");
    assert!(html.contains("\"nodes\":") && html.contains("Fly.io"));
    assert!(html.contains("p — knowledge graph"));
    // Self-contained: no external fetches of any kind.
    for needle in ["http://", "https://", "src=\"//", "@import"] {
        assert!(!html.contains(needle), "external reference found: {needle}");
    }
    // Labels/quotes in content must not break the embedded JSON (script
    // closing tag is the classic breaker).
    assert!(!html.contains("</script>\"") && html.matches("</script>").count() == 1);
}
