//! Ticket 009 tests: stale-memory review flow.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use palimpsest_mcp::config::ReviewConfig;
use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::{MemoryServer, ReviewMemoriesRequest, SaveMemoryRequest};

const DIMS: usize = 8;

fn server() -> MemoryServer {
    let db = palimpsest_mcp::db::Db::open_in_memory(DIMS).expect("db");
    MemoryServer::new(
        db,
        Arc::new(MockEmbedder::new(DIMS)),
        Arc::new(MockExtractor::failing()),
    )
    .with_review_config(ReviewConfig {
        stale_after_days: 90,
        max_importance: 3,
    })
}

fn structured(r: &CallToolResult) -> serde_json::Value {
    assert_ne!(r.is_error, Some(true), "expected success: {r:?}");
    r.structured_content.clone().expect("structured")
}

async fn save(s: &MemoryServer, project: Option<&str>, content: &str, kind: &str, importance: i64) -> i64 {
    let r = s
        .save_memory(Parameters(SaveMemoryRequest {
            project: project.map(String::from),
            content: content.into(),
            kind: Some(kind.into()),
            importance: Some(importance),
            supersedes: None,
        }))
        .await
        .expect("ok");
    structured(&r)["memory_id"].as_i64().expect("id")
}

/// Age a memory: pretend it was created/last seen `days` ago.
async fn age(s: &MemoryServer, id: i64, days: i64) {
    let db = s.db_handle();
    let db = db.lock().await;
    db.conn()
        .execute(
            "UPDATE memories
             SET valid_at = strftime('%Y-%m-%dT%H:%M:%fZ','now', ?1 || ' days'),
                 last_retrieved_at = NULL
             WHERE id = ?2",
            rusqlite::params![format!("-{days}"), id],
        )
        .expect("age");
}

async fn review(s: &MemoryServer, project: Option<&str>, confirm: Option<Vec<i64>>) -> serde_json::Value {
    let r = s
        .review_memories(Parameters(ReviewMemoriesRequest {
            project: project.map(String::from),
            limit: None,
            confirm,
        }))
        .await
        .expect("ok");
    structured(&r)
}

fn stale_ids(v: &serde_json::Value) -> Vec<i64> {
    v["stale"].as_array().expect("stale")
        .iter().map(|m| m["memory_id"].as_i64().expect("id")).collect()
}

#[tokio::test]
async fn stale_filter_returns_only_valid_old_low_importance_rows() {
    let s = server();
    let stale = save(&s, Some("p"), "old low-importance fact", "fact", 2).await;
    let fresh = save(&s, Some("p"), "recent fact", "fact", 2).await;
    let important = save(&s, Some("p"), "old but critical fact", "fact", 5).await;
    let superseded = save(&s, Some("p"), "old fact later replaced", "fact", 2).await;
    let global_pref = save(&s, None, "user prefers tabs", "preference", 2).await;
    let global_fact = save(&s, None, "user machine is arm64", "fact", 2).await;

    for id in [stale, important, superseded, global_pref, global_fact] {
        age(&s, id, 200).await;
    }
    // Supersede one of the aged rows.
    s.save_memory(Parameters(SaveMemoryRequest {
        project: Some("p".into()),
        content: "the replacement fact".into(),
        kind: None,
        importance: None,
        supersedes: Some(superseded),
    }))
    .await
    .expect("ok");

    let result = review(&s, Some("p"), None).await;
    let ids = stale_ids(&result);
    assert!(ids.contains(&stale), "aged low-importance row must show: {ids:?}");
    assert!(ids.contains(&global_fact), "aged global FACT must show: {ids:?}");
    assert!(!ids.contains(&fresh), "fresh row must not show");
    assert!(!ids.contains(&important), "importance 5 must not show");
    assert!(!ids.contains(&superseded), "superseded row must not show");
    assert!(!ids.contains(&global_pref), "global preference is evergreen");
}

#[tokio::test]
async fn confirm_touches_usage_and_removes_from_stale_list() {
    let s = server();
    let id = save(&s, Some("p"), "fact to confirm", "fact", 2).await;
    age(&s, id, 120).await;

    let first = review(&s, Some("p"), None).await;
    assert!(stale_ids(&first).contains(&id));

    // Confirm it: same call both confirms and re-lists.
    let second = review(&s, Some("p"), Some(vec![id])).await;
    assert_eq!(second["confirmed"], 1);
    assert!(!stale_ids(&second).contains(&id), "confirmed row must leave the list");

    // The touch went through the usage columns only.
    let db = s.db_handle();
    let db = db.lock().await;
    let (last, count, content): (Option<String>, i64, String) = db
        .conn()
        .query_row(
            "SELECT last_retrieved_at, retrieval_count, content FROM memories WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert!(last.is_some());
    assert!(count >= 1);
    assert_eq!(content, "fact to confirm", "content untouched");
}

#[tokio::test]
async fn unknown_project_is_actionable_error() {
    let s = server();
    let r = s
        .review_memories(Parameters(ReviewMemoriesRequest {
            project: Some("nope".into()),
            limit: None,
            confirm: None,
        }))
        .await
        .expect("no protocol error");
    assert_eq!(r.is_error, Some(true));
}
