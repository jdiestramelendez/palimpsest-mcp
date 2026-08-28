//! Ticket 002 tests: entity graph extraction, edge expiry via
//! supersession, the entity ranking leg, and get_related neighborhoods.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use palimpsest_mcp::db::queries::{
    self, EntityRef, GraphPayload, NewMemory, TripleRef,
};
use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::{
    EndSessionRequest, GetRelatedRequest, MemoryServer, SearchMemoryRequest,
};

const DIMS: usize = 8;

fn axis(i: usize) -> Vec<f32> {
    let mut v = vec![0.0; DIMS];
    v[i] = 1.0;
    v
}

fn server_with(responses: Vec<String>) -> (MemoryServer, Arc<MockEmbedder>) {
    let db = palimpsest_mcp::db::Db::open_in_memory(DIMS).expect("db");
    let embedder = Arc::new(MockEmbedder::new(DIMS));
    let s = MemoryServer::new(db, embedder.clone(), Arc::new(MockExtractor::new(responses)));
    (s, embedder)
}

fn structured(r: &CallToolResult) -> serde_json::Value {
    assert_ne!(r.is_error, Some(true), "expected success: {r:?}");
    r.structured_content.clone().expect("structured")
}

/// Insert a memory with a graph payload directly through the query layer.
async fn seed(
    s: &MemoryServer,
    project: Option<&str>,
    content: &str,
    embedding: Vec<f32>,
    graph: GraphPayload,
    supersedes: Option<i64>,
) -> i64 {
    let db = s.db_handle();
    let mut db = db.lock().await;
    if let Some(p) = project {
        queries::ensure_project(db.conn(), p).expect("project");
    }
    queries::insert_memory(
        db.conn_mut(),
        &NewMemory {
            project_id: project.map(String::from),
            content: content.into(),
            kind: "fact".into(),
            importance: 3,
            source_session_id: None,
        },
        &embedding,
        supersedes,
        Some(&graph),
    )
    .expect("insert")
}

fn entities(names: &[(&str, &str)]) -> Vec<EntityRef> {
    names
        .iter()
        .map(|(n, k)| EntityRef {
            name: n.to_string(),
            kind: k.to_string(),
        })
        .collect()
}

#[tokio::test]
async fn extraction_pipeline_creates_entities_and_relations() {
    let extraction = r#"{
      "summary": "storage decision",
      "facts": [{
        "content": "acme-shop uses PostgreSQL for storage",
        "kind": "decision", "importance": 4, "scope": "project",
        "entities": [
          {"name": "acme-shop", "kind": "project"},
          {"name": "PostgreSQL", "kind": "technology"}
        ],
        "triples": [
          {"source": "acme-shop", "predicate": "uses", "target": "PostgreSQL"}
        ]
      }]
    }"#;
    let (s, _) = server_with(vec![extraction.into(), "# state".into()]);
    let result = s
        .end_session(Parameters(EndSessionRequest {
            project: Some("acme-shop".into()),
            transcript: "[user] we settled on postgres".into(),
            client: None,
        }))
        .await
        .expect("ok");
    assert_eq!(structured(&result)["memories_created"], 1);

    let db = s.db_handle();
    let db = db.lock().await;
    let (ents, mentions, rels): (i64, i64, i64) = (
        db.conn().query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0)).expect("e"),
        db.conn().query_row("SELECT COUNT(*) FROM memory_entities", [], |r| r.get(0)).expect("me"),
        db.conn().query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0)).expect("r"),
    );
    assert_eq!((ents, mentions, rels), (2, 2, 1));
    let kind: String = db
        .conn()
        .query_row("SELECT kind FROM entities WHERE norm_name = 'postgresql'", [], |r| r.get(0))
        .expect("kind");
    assert_eq!(kind, "technology");
}

#[tokio::test]
async fn superseding_evidence_expires_edges() {
    let (s, _) = server_with(vec![]);
    let old = seed(
        &s, Some("p"), "p deploys to Heroku", axis(0),
        GraphPayload {
            entities: entities(&[("p", "project"), ("Heroku", "technology")]),
            triples: vec![TripleRef {
                source: "p".into(), predicate: "deploys-to".into(), target: "Heroku".into(),
            }],
        },
        None,
    ).await;

    // Neighborhood sees Heroku while the fact is valid.
    let before = structured(
        &s.get_related(Parameters(GetRelatedRequest {
            entity: "p".into(), hops: None, project: Some("p".into()),
        })).await.expect("ok"),
    );
    assert_eq!(before["edges"].as_array().expect("edges").len(), 1);

    // Supersede the evidence memory -> the edge must drop out.
    seed(
        &s, Some("p"), "p deploys to Fly.io", axis(0),
        GraphPayload {
            entities: entities(&[("p", "project"), ("Fly.io", "technology")]),
            triples: vec![TripleRef {
                source: "p".into(), predicate: "deploys-to".into(), target: "Fly.io".into(),
            }],
        },
        Some(old),
    ).await;

    let after = structured(
        &s.get_related(Parameters(GetRelatedRequest {
            entity: "p".into(), hops: None, project: Some("p".into()),
        })).await.expect("ok"),
    );
    let edges = after["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 1, "only the Fly.io edge should remain: {edges:?}");
    let names: Vec<&str> = after["nodes"].as_array().expect("nodes")
        .iter().map(|n| n["name"].as_str().expect("name")).collect();
    assert!(names.contains(&"Fly.io") && !names.contains(&"Heroku"), "{names:?}");
}

#[tokio::test]
async fn entity_match_ranks_memory_with_no_lexical_or_vector_overlap() {
    let (s, embedder) = server_with(vec![]);
    // Query and target memory share NO tokens and are on orthogonal axes;
    // only the entity link can surface it above the decoy.
    embedder.pin("kubernetes rollout", axis(1));
    let target = seed(
        &s, Some("p"), "the deployment platform restarts pods automatically", axis(5),
        GraphPayload { entities: entities(&[("Kubernetes", "technology")]), triples: vec![] },
        None,
    ).await;
    // Decoy: same distance profile, no entity link.
    seed(
        &s, Some("p"), "the coffee machine restarts sometimes", axis(6),
        GraphPayload::default(), None,
    ).await;

    let result = s
        .search_memory(Parameters(SearchMemoryRequest {
            query: "kubernetes rollout".into(),
            project: Some("p".into()),
            kinds: None, limit: None, as_of: None,
        }))
        .await
        .expect("ok");
    let json = structured(&result);
    let results = json["results"].as_array().expect("results");
    assert_eq!(
        results[0]["memory_id"].as_i64(),
        Some(target),
        "entity-linked memory must rank first: {results:?}"
    );
}

#[tokio::test]
async fn get_related_walks_one_and_two_hops_with_evidence() {
    let (s, _) = server_with(vec![]);
    // p --uses--> Rust --compiled-by--> LLVM
    let m1 = seed(
        &s, Some("p"), "p is written in Rust", axis(0),
        GraphPayload {
            entities: entities(&[("p", "project"), ("Rust", "technology")]),
            triples: vec![TripleRef { source: "p".into(), predicate: "uses".into(), target: "Rust".into() }],
        },
        None,
    ).await;
    let m2 = seed(
        &s, Some("p"), "Rust compiles through LLVM", axis(1),
        GraphPayload {
            entities: entities(&[("Rust", "technology"), ("LLVM", "technology")]),
            triples: vec![TripleRef { source: "Rust".into(), predicate: "compiled-by".into(), target: "LLVM".into() }],
        },
        None,
    ).await;

    let one = structured(
        &s.get_related(Parameters(GetRelatedRequest {
            entity: "p".into(), hops: Some(1), project: Some("p".into()),
        })).await.expect("ok"),
    );
    let one_names: Vec<&str> = one["nodes"].as_array().expect("n")
        .iter().map(|n| n["name"].as_str().expect("s")).collect();
    assert!(one_names.contains(&"Rust") && !one_names.contains(&"LLVM"), "{one_names:?}");
    assert_eq!(one["edges"][0]["evidence_memory_id"].as_i64(), Some(m1));

    let two = structured(
        &s.get_related(Parameters(GetRelatedRequest {
            entity: "p".into(), hops: Some(2), project: Some("p".into()),
        })).await.expect("ok"),
    );
    let two_names: Vec<&str> = two["nodes"].as_array().expect("n")
        .iter().map(|n| n["name"].as_str().expect("s")).collect();
    assert!(two_names.contains(&"LLVM"), "2 hops must reach LLVM: {two_names:?}");
    assert!(two["edges"].as_array().expect("e").iter()
        .any(|e| e["evidence_memory_id"].as_i64() == Some(m2)));

    // Unknown entity -> actionable tool error.
    let missing = s
        .get_related(Parameters(GetRelatedRequest {
            entity: "quantum-toaster".into(), hops: None, project: None,
        }))
        .await
        .expect("no protocol error");
    assert_eq!(missing.is_error, Some(true));
}
