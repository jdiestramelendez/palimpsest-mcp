//! Tests for get_project_state + save_memory (Step 3).
//! In-memory DB + MockEmbedder; tools invoked directly on the server.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use memory_mcp::db::Db;
use memory_mcp::providers::mock::{MockEmbedder, MockExtractor};
use memory_mcp::tools::{GetProjectStateRequest, MemoryServer, SaveMemoryRequest};

const DIMS: usize = 16;

fn server() -> MemoryServer {
    let db = Db::open_in_memory(DIMS).expect("in-memory db");
    MemoryServer::new(
        db,
        Arc::new(MockEmbedder::new(DIMS)),
        Arc::new(MockExtractor::failing()),
    )
}

/// Extract the structured JSON of a successful tool result.
fn structured(result: &CallToolResult) -> serde_json::Value {
    assert_ne!(result.is_error, Some(true), "expected success: {result:?}");
    result.structured_content.clone().expect("structured content")
}

/// Assert a tool-level error and return its message text.
fn error_text(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "expected tool-level error: {result:?}");
    serde_json::to_string(&result.content).expect("content serializes")
}

async fn save(server: &MemoryServer, req: SaveMemoryRequest) -> CallToolResult {
    server.save_memory(Parameters(req)).await.expect("no protocol error")
}

fn save_req(project: Option<&str>, content: &str) -> SaveMemoryRequest {
    SaveMemoryRequest {
        project: project.map(String::from),
        content: content.into(),
        kind: None,
        importance: None,
        supersedes: None,
    }
}

#[tokio::test]
async fn save_memory_creates_project_and_lands_in_all_indexes() {
    let s = server();
    let result = save(&s, save_req(Some("My Cool Project"), "the API uses OAuth2")).await;
    let json = structured(&result);
    assert_eq!(json["project_id"], "my-cool-project", "slugged project id");
    let memory_id = json["memory_id"].as_i64().expect("memory_id");

    // Row visible in memories, memories_vec and memories_fts.
    let db = s.db_handle();
    let db = db.lock().await;
    let (mem, vec, fts): (i64, i64, i64) = (
        db.conn()
            .query_row("SELECT COUNT(*) FROM memories WHERE id = ?1", [memory_id], |r| r.get(0))
            .expect("memories"),
        db.conn()
            .query_row("SELECT COUNT(*) FROM memories_vec WHERE memory_id = ?1", [memory_id], |r| r.get(0))
            .expect("vec"),
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH 'oauth2'",
                [],
                |r| r.get(0),
            )
            .expect("fts"),
    );
    assert_eq!((mem, vec, fts), (1, 1, 1));
}

#[tokio::test]
async fn supersession_invalidates_old_and_keeps_both_rows() {
    let s = server();
    let first = structured(&save(&s, save_req(Some("proj"), "we deploy on Fridays")).await);
    let old_id = first["memory_id"].as_i64().expect("id");

    let mut req = save_req(Some("proj"), "we deploy on Mondays now");
    req.supersedes = Some(old_id);
    let second = structured(&save(&s, req).await);
    let new_id = second["memory_id"].as_i64().expect("id");

    let db = s.db_handle();
    let db = db.lock().await;
    let total: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .expect("count");
    assert_eq!(total, 2, "nothing may ever be deleted");

    let (invalid_at, superseded_by): (Option<String>, Option<i64>) = db
        .conn()
        .query_row(
            "SELECT invalid_at, superseded_by FROM memories WHERE id = ?1",
            [old_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("old row");
    assert!(invalid_at.is_some(), "old row must be invalidated");
    assert_eq!(superseded_by, Some(new_id));

    let new_invalid: Option<String> = db
        .conn()
        .query_row("SELECT invalid_at FROM memories WHERE id = ?1", [new_id], |r| r.get(0))
        .expect("new row");
    assert!(new_invalid.is_none(), "new row must be valid");
}

#[tokio::test]
async fn superseding_missing_or_stale_memory_is_a_tool_error() {
    let s = server();
    // Missing id.
    let mut req = save_req(Some("proj"), "some fact");
    req.supersedes = Some(9999);
    let msg = error_text(&save(&s, req).await);
    assert!(msg.contains("9999"), "must name the bad id: {msg}");

    // Already-superseded id.
    let first = structured(&save(&s, save_req(Some("proj"), "v1 fact")).await);
    let old_id = first["memory_id"].as_i64().expect("id");
    let mut req = save_req(Some("proj"), "v2 fact");
    req.supersedes = Some(old_id);
    save(&s, req).await;
    let mut req = save_req(Some("proj"), "v3 fact");
    req.supersedes = Some(old_id); // old_id is stale now
    let msg = error_text(&save(&s, req).await);
    assert!(msg.contains("already superseded"), "msg: {msg}");

    // A tool-level error must not have inserted anything.
    let db = s.db_handle();
    let db = db.lock().await;
    let total: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .expect("count");
    assert_eq!(total, 2, "failed saves must not leave rows");
}

#[tokio::test]
async fn get_project_state_resolves_alias_and_counts() {
    let s = server();
    save(&s, save_req(Some("frontend-app"), "uses Vite")).await;
    save(&s, save_req(Some("frontend-app"), "state lives in zustand")).await;
    save(&s, save_req(None, "user prefers tabs over spaces")).await; // global

    // Add an alias directly (alias management is not a tool).
    {
        let db = s.db_handle();
        let db = db.lock().await;
        db.conn()
            .execute(
                "UPDATE projects SET aliases = '[\"the app\", \"FE\"]' WHERE id = 'frontend-app'",
                [],
            )
            .expect("set aliases");
    }

    let result = s
        .get_project_state(Parameters(GetProjectStateRequest { project: "fe".into() }))
        .await
        .expect("no protocol error");
    let json = structured(&result);
    assert_eq!(json["project_id"], "frontend-app", "alias resolved case-insensitively");
    assert_eq!(json["active_memory_count"], 2);
    assert_eq!(json["global_memory_count"], 1);
}

#[tokio::test]
async fn get_project_state_unknown_project_lists_existing() {
    let s = server();
    save(&s, save_req(Some("alpha"), "a fact")).await;
    save(&s, save_req(Some("beta"), "b fact")).await;

    let result = s
        .get_project_state(Parameters(GetProjectStateRequest { project: "gamma".into() }))
        .await
        .expect("no protocol error");
    let msg = error_text(&result);
    assert!(msg.contains("gamma") && msg.contains("alpha") && msg.contains("beta"), "msg: {msg}");
}
