//! Ticket 003 tests: recency boost and usage tracking.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use palimpsest_mcp::config::{Config, RetrievalConfig};
use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::{MemoryServer, SaveMemoryRequest, SearchMemoryRequest};

const DIMS: usize = 8;

fn axis(i: usize) -> Vec<f32> {
    let mut v = vec![0.0; DIMS];
    v[i] = 1.0;
    v
}

fn server(retrieval: RetrievalConfig) -> (MemoryServer, Arc<MockEmbedder>) {
    let db = palimpsest_mcp::db::Db::open_in_memory(DIMS).expect("db");
    let embedder = Arc::new(MockEmbedder::new(DIMS));
    let s = MemoryServer::new(db, embedder.clone(), Arc::new(MockExtractor::failing()))
        .with_retrieval_config(retrieval);
    (s, embedder)
}

fn structured(r: &CallToolResult) -> serde_json::Value {
    assert_ne!(r.is_error, Some(true), "expected success: {r:?}");
    r.structured_content.clone().expect("structured")
}

async fn save(s: &MemoryServer, content: &str) -> i64 {
    let r = s
        .save_memory(Parameters(SaveMemoryRequest {
            project: Some("p".into()),
            content: content.into(),
            kind: None,
            importance: None,
            supersedes: None,
        }))
        .await
        .expect("ok");
    structured(&r)["memory_id"].as_i64().expect("id")
}

async fn search(s: &MemoryServer, query: &str, as_of: Option<&str>) -> Vec<i64> {
    let r = s
        .search_memory(Parameters(SearchMemoryRequest {
            query: query.into(),
            project: Some("p".into()),
            kinds: None,
            limit: None,
            as_of: as_of.map(String::from),
        }))
        .await
        .expect("ok");
    structured(&r)["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|x| x["memory_id"].as_i64().expect("id"))
        .collect()
}

/// Backdate a memory's valid_at/created_at (test-only surgery).
async fn backdate(s: &MemoryServer, id: i64, days: i64) {
    let db = s.db_handle();
    let db = db.lock().await;
    db.conn()
        .execute(
            "UPDATE memories
             SET valid_at = strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1 || ' days'),
                 created_at = valid_at
             WHERE id = ?2",
            rusqlite::params![format!("-{days}"), id],
        )
        .expect("backdate");
}

#[tokio::test]
async fn fresh_memory_outranks_stale_one_with_better_raw_rank() {
    // stale is vector-rank 1 (identical vector to the query), fresh is
    // rank 2 (slightly off-axis) — with no boost stale wins; recency
    // flips it; the boost stays bounded by 1 + weight.
    let cfg = RetrievalConfig {
        recency_weight: 0.25,
        recency_half_life_days: 30.0,
    };
    let (s, embedder) = server(cfg);
    embedder.pin("release cadence", axis(0));
    embedder.pin("we ship every friday", axis(0)); // exact match -> rank 1
    let mut close = axis(0);
    close[1] = 0.3; // near but rank 2
    embedder.pin("we ship weekly on thursdays now", close.clone());

    let stale = save(&s, "we ship every friday").await;
    let fresh = save(&s, "we ship weekly on thursdays now").await;
    backdate(&s, stale, 365).await;

    let ids = search(&s, "release cadence", None).await;
    assert_eq!(ids[0], fresh, "recency must flip the near-tie: {ids:?}");

    // Same data, boost disabled -> raw ranking wins.
    let (s2, embedder2) = server(RetrievalConfig {
        recency_weight: 0.0,
        recency_half_life_days: 30.0,
    });
    embedder2.pin("release cadence", axis(0));
    embedder2.pin("we ship every friday", axis(0));
    embedder2.pin("we ship weekly on thursdays now", close);
    let stale2 = save(&s2, "we ship every friday").await;
    save(&s2, "we ship weekly on thursdays now").await;
    backdate(&s2, stale2, 365).await;
    let ids2 = search(&s2, "release cadence", None).await;
    assert_eq!(ids2[0], stale2, "weight 0 must disable the boost: {ids2:?}");
}

#[tokio::test]
async fn usage_counters_bump_on_hits_but_not_for_as_of() {
    let (s, embedder) = server(RetrievalConfig::default());
    embedder.pin("database choice", axis(2));
    embedder.pin("we use SQLite", axis(2));
    let id = save(&s, "we use SQLite").await;

    assert!(search(&s, "database choice", None).await.contains(&id));
    assert!(search(&s, "database choice", None).await.contains(&id));

    let (count, last): (i64, Option<String>) = {
        let db = s.db_handle();
        let db = db.lock().await;
        db.conn()
            .query_row(
                "SELECT retrieval_count, last_retrieved_at FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row")
    };
    assert_eq!(count, 2, "two current-time searches -> two bumps");
    let last = last.expect("last_retrieved_at set");

    // Historical search: same hit, no bump, no timestamp change.
    let future = "2099-01-01T00:00:00Z";
    assert!(search(&s, "database choice", Some(future)).await.contains(&id));
    let (count2, last2): (i64, Option<String>) = {
        let db = s.db_handle();
        let db = db.lock().await;
        db.conn()
            .query_row(
                "SELECT retrieval_count, last_retrieved_at FROM memories WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row")
    };
    assert_eq!(count2, 2, "as_of search must not bump");
    assert_eq!(last2.as_deref(), Some(last.as_str()));
}

#[test]
fn retrieval_config_parses_from_toml_and_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[retrieval]\nrecency_weight = 0.5\n").expect("write");
    let cfg = Config::load_from(&path).expect("load");
    assert_eq!(cfg.retrieval.recency_weight, 0.5);
    assert_eq!(cfg.retrieval.recency_half_life_days, 30.0, "unset field keeps default");

    let empty = Config::load_from(&dir.path().join("missing.toml")).expect("defaults");
    assert_eq!(empty.retrieval.recency_weight, 0.25);
}
