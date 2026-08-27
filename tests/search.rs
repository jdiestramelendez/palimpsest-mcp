//! Hybrid retrieval tests (Step 4). Seeded in-memory DB + pinned mock
//! vectors so similarity is fully controllable.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::{MemoryServer, SaveMemoryRequest, SearchMemoryRequest};

const DIMS: usize = 8;

/// Axis-aligned unit vector along `axis`, for exact similarity control.
fn axis(axis: usize) -> Vec<f32> {
    let mut v = vec![0.0; DIMS];
    v[axis] = 1.0;
    v
}

/// Server + a handle to the embedder for pinning vectors.
fn server() -> (MemoryServer, Arc<MockEmbedder>) {
    let db = palimpsest_mcp::db::Db::open_in_memory(DIMS).expect("db");
    let embedder = Arc::new(MockEmbedder::new(DIMS));
    let server = MemoryServer::new(db, embedder.clone(), Arc::new(MockExtractor::failing()));
    (server, embedder)
}

async fn save(s: &MemoryServer, project: Option<&str>, content: &str, supersedes: Option<i64>) -> i64 {
    let result = s
        .save_memory(Parameters(SaveMemoryRequest {
            project: project.map(String::from),
            content: content.into(),
            kind: None,
            importance: None,
            supersedes,
        }))
        .await
        .expect("no protocol error");
    assert_ne!(result.is_error, Some(true), "save failed: {result:?}");
    result.structured_content.as_ref().expect("structured")["memory_id"]
        .as_i64()
        .expect("memory_id")
}

async fn search(s: &MemoryServer, req: SearchMemoryRequest) -> CallToolResult {
    s.search_memory(Parameters(req)).await.expect("no protocol error")
}

fn req(query: &str, project: Option<&str>) -> SearchMemoryRequest {
    SearchMemoryRequest {
        query: query.into(),
        project: project.map(String::from),
        kinds: None,
        limit: None,
        as_of: None,
    }
}

fn hit_ids(result: &CallToolResult) -> Vec<i64> {
    assert_ne!(result.is_error, Some(true), "search failed: {result:?}");
    result.structured_content.as_ref().expect("structured")["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["memory_id"].as_i64().expect("id"))
        .collect()
}

#[tokio::test]
async fn lexical_only_hit_is_found() {
    let (s, embedder) = server();
    embedder.pin("zorbification quota", axis(0)); // query vector
    embedder.pin("the zorbification quota is 7 per day", axis(4)); // orthogonal
    let id = save(&s, Some("p"), "the zorbification quota is 7 per day", None).await;
    // noise
    save(&s, Some("p"), "unrelated fact about databases", None).await;

    let ids = hit_ids(&search(&s, req("zorbification quota", Some("p"))).await);
    assert!(ids.contains(&id), "verbatim term must be found despite distant vector: {ids:?}");
}

#[tokio::test]
async fn vector_only_hit_is_found() {
    let (s, embedder) = server();
    embedder.pin("how do we authenticate", axis(2));
    embedder.pin("login uses signed JWTs from the gateway", axis(2)); // identical vector
    let id = save(&s, Some("p"), "login uses signed JWTs from the gateway", None).await;
    save(&s, Some("p"), "coffee machine is on floor 3", None).await;

    // Query shares no token with the memory -> lexical leg cannot see it.
    let ids = hit_ids(&search(&s, req("how do we authenticate", Some("p"))).await);
    assert!(ids.contains(&id), "semantically close memory must be found: {ids:?}");
}

#[tokio::test]
async fn dual_signal_match_outranks_single_signal() {
    let (s, embedder) = server();
    embedder.pin("deploy pipeline", axis(1));
    embedder.pin("the deploy pipeline runs on GitHub Actions", axis(1)); // vector + lexical
    embedder.pin("deploy tokens rotate weekly", axis(5));                // lexical only
    embedder.pin("CI executes automated release workflows", axis(1));    // vector only

    let both = save(&s, Some("p"), "the deploy pipeline runs on GitHub Actions", None).await;
    let lex = save(&s, Some("p"), "deploy tokens rotate weekly", None).await;
    let vec_only = save(&s, Some("p"), "CI executes automated release workflows", None).await;

    let ids = hit_ids(&search(&s, req("deploy pipeline", Some("p"))).await);
    let pos = |id: i64| ids.iter().position(|x| *x == id);
    assert_eq!(pos(both), Some(0), "dual-signal hit must rank first: {ids:?}");
    assert!(pos(lex).is_some() && pos(vec_only).is_some(), "single-signal hits present: {ids:?}");
}

#[tokio::test]
async fn diacritics_fold_in_lexical_search() {
    let (s, embedder) = server();
    embedder.pin("configuración", axis(0));
    embedder.pin("la configuracion vive en config.toml", axis(6));
    let id = save(&s, Some("p"), "la configuracion vive en config.toml", None).await;

    let ids = hit_ids(&search(&s, req("configuración", Some("p"))).await);
    assert!(ids.contains(&id), "accented query must match unaccented row: {ids:?}");
}

#[tokio::test]
async fn as_of_returns_old_fact_and_now_returns_new() {
    let (s, embedder) = server();
    embedder.pin("database engine", axis(3));
    embedder.pin("we use Postgres for storage", axis(3));
    embedder.pin("we use SQLite for storage", axis(3));

    let old_id = save(&s, Some("p"), "we use Postgres for storage", None).await;
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    // Instant between the old fact and its supersession.
    let between: String = {
        let db = s.db_handle();
        let db = db.lock().await;
        db.conn()
            .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))
            .expect("now")
    };
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    let new_id = save(&s, Some("p"), "we use SQLite for storage", Some(old_id)).await;

    // Current view: only the new fact.
    let now_ids = hit_ids(&search(&s, req("database engine", Some("p"))).await);
    assert!(now_ids.contains(&new_id) && !now_ids.contains(&old_id), "now: {now_ids:?}");

    // Historical view: only the old fact.
    let mut historical = req("database engine", Some("p"));
    historical.as_of = Some(between);
    let then_ids = hit_ids(&search(&s, historical).await);
    assert!(then_ids.contains(&old_id) && !then_ids.contains(&new_id), "then: {then_ids:?}");
}

#[tokio::test]
async fn invalid_as_of_is_a_protocol_invalid_params_error() {
    let (s, _) = server();
    let mut r = req("anything", None);
    r.as_of = Some("not-a-date".into());
    let err = s
        .search_memory(Parameters(r))
        .await
        .expect_err("must be a protocol-level error");
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{err:?}");
}

#[tokio::test]
async fn global_memories_appear_in_project_scoped_search() {
    let (s, embedder) = server();
    embedder.pin("editor preferences", axis(7));
    embedder.pin("user prefers 2-space indentation everywhere", axis(7));
    let global_id = save(&s, None, "user prefers 2-space indentation everywhere", None).await;
    save(&s, Some("p"), "project uses rustfmt defaults", None).await;

    let ids = hit_ids(&search(&s, req("editor preferences", Some("p"))).await);
    assert!(ids.contains(&global_id), "global memory must appear in project scope: {ids:?}");

    // And scoping still excludes other projects' memories.
    let other = save(&s, Some("q"), "editor preferences of project q", None).await;
    let ids = hit_ids(&search(&s, req("editor preferences", Some("p"))).await);
    assert!(!ids.contains(&other), "other project's memory must not leak: {ids:?}");
}
