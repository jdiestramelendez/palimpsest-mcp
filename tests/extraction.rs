//! Extraction pipeline tests (Step 5). MockExtractor with scripted
//! responses; fixture transcript in tests/fixtures/.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::{EndSessionRequest, MemoryServer, SaveMemoryRequest};

const DIMS: usize = 16;
const TRANSCRIPT: &str = include_str!("fixtures/transcript.txt");

fn server_with(responses: Vec<&str>) -> MemoryServer {
    let db = palimpsest_mcp::db::Db::open_in_memory(DIMS).expect("db");
    MemoryServer::new(
        db,
        Arc::new(MockEmbedder::new(DIMS)),
        Arc::new(MockExtractor::new(
            responses.into_iter().map(String::from).collect(),
        )),
    )
}

async fn end_session(s: &MemoryServer, project: Option<&str>) -> CallToolResult {
    s.end_session(Parameters(EndSessionRequest {
        project: project.map(String::from),
        transcript: TRANSCRIPT.into(),
        client: Some("test-client".into()),
    }))
    .await
    .expect("no protocol error")
}

fn structured(result: &CallToolResult) -> serde_json::Value {
    assert_ne!(result.is_error, Some(true), "expected success: {result:?}");
    result.structured_content.clone().expect("structured")
}

async fn seed_memory(s: &MemoryServer, project: &str, content: &str) -> i64 {
    let r = s
        .save_memory(Parameters(SaveMemoryRequest {
            project: Some(project.into()),
            content: content.into(),
            kind: None,
            importance: None,
            supersedes: None,
        }))
        .await
        .expect("no protocol error");
    structured(&r)["memory_id"].as_i64().expect("id")
}

const EXTRACTION_TWO_FACTS: &str = r#"{
  "summary": "Swapped acme-shop storage from JSON prototype to SQLite with litestream.",
  "facts": [
    {"content": "acme-shop uses SQLite with litestream backups as its primary store", "kind": "decision", "importance": 5, "scope": "project"},
    {"content": "migration files in acme-shop start with a UTC timestamp prefix", "kind": "preference", "importance": 3, "scope": "project"}
  ]
}"#;

#[tokio::test]
async fn happy_path_counts_supersession_and_state() {
    let s = server_with(vec![]);
    let old_id = seed_memory(&s, "acme-shop", "acme-shop stores orders in a JSON file").await;

    // Script: extraction, conflict for fact 1 (supersedes), conflict for
    // fact 2 (new), state rewrite.
    let conflict1 = format!(r#"{{"verdict": "supersedes", "supersedes": {old_id}}}"#);
    {
        let extractor = MockExtractor::new(vec![
            EXTRACTION_TWO_FACTS.into(),
            conflict1,
            r#"{"verdict": "new"}"#.into(),
            "# acme-shop — State\n\n## Now\n- storage migration done\n".into(),
        ]);
        // Rebuild the server with the scripted extractor but the SAME db.
        let db = s.db_handle();
        let s2 = MemoryServer::from_shared(db, Arc::new(MockEmbedder::new(DIMS)), Arc::new(extractor));

        let json = structured(&end_session(&s2, Some("acme-shop")).await);
        assert_eq!(json["facts_extracted"], 2);
        assert_eq!(json["memories_created"], 2);
        assert_eq!(json["memories_superseded"], 1);
        assert_eq!(json["duplicates_skipped"], 0);
        assert_eq!(json["state_updated"], true);
        assert!(json["summary"].as_str().expect("summary").contains("SQLite"));
        let session_id = json["session_id"].as_i64().expect("session_id");

        let db = s2.db_handle();
        let db = db.lock().await;
        // Old memory invalidated, never deleted.
        let (total, invalid): (i64, Option<String>) = (
            db.conn().query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0)).expect("count"),
            db.conn()
                .query_row("SELECT invalid_at FROM memories WHERE id = ?1", [old_id], |r| r.get(0))
                .expect("old row"),
        );
        assert_eq!(total, 3);
        assert!(invalid.is_some());
        // STATE.md rewritten, session summary saved.
        let state: String = db
            .conn()
            .query_row("SELECT state_md FROM projects WHERE id = 'acme-shop'", [], |r| r.get(0))
            .expect("state");
        assert!(state.contains("storage migration done"));
        let summary: Option<String> = db
            .conn()
            .query_row("SELECT summary FROM sessions WHERE id = ?1", [session_id], |r| r.get(0))
            .expect("session");
        assert!(summary.expect("summary set").contains("SQLite"));
    }
}

#[tokio::test]
async fn duplicate_verdict_creates_nothing() {
    let s = server_with(vec![]);
    let dup_target =
        seed_memory(&s, "acme-shop", "acme-shop uses SQLite with litestream backups").await;

    let extraction = r#"{
      "summary": "Confirmed storage decision.",
      "facts": [
        {"content": "acme-shop uses SQLite with litestream backups as its primary store", "kind": "decision", "importance": 5, "scope": "project"}
      ]
    }"#;
    let conflict = format!(r#"{{"verdict": "duplicate", "of": {dup_target}}}"#);
    let s2 = MemoryServer::from_shared(
        s.db_handle(),
        Arc::new(MockEmbedder::new(DIMS)),
        Arc::new(MockExtractor::new(vec![
            extraction.into(),
            conflict,
            "# acme-shop — State\n\n## Now\n- steady\n".into(),
        ])),
    );

    let json = structured(&end_session(&s2, Some("acme-shop")).await);
    assert_eq!(json["memories_created"], 0);
    assert_eq!(json["duplicates_skipped"], 1);

    let db = s2.db_handle();
    let db = db.lock().await;
    let total: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .expect("count");
    assert_eq!(total, 1, "duplicate must not create a row");
}

#[tokio::test]
async fn malformed_json_then_valid_retry_succeeds() {
    let s = server_with(vec![
        "Sure! Here are the facts you asked for, hope this helps!",
        r#"{"summary": "One fact.", "facts": [{"content": "acme-shop pins Rust 1.94", "kind": "fact", "importance": 3, "scope": "project"}]}"#,
        "# acme-shop — State\n\n## Conventions\n- Rust 1.94 pinned\n",
    ]);
    let json = structured(&end_session(&s, Some("acme-shop")).await);
    assert_eq!(json["facts_extracted"], 1);
    assert_eq!(json["memories_created"], 1);
}

#[tokio::test]
async fn extractor_hard_failure_saves_session_and_reports() {
    let s = server_with(vec![]); // failing extractor: script exhausted
    let result = end_session(&s, Some("acme-shop")).await;
    assert_eq!(result.is_error, Some(true), "must be a tool-level error");
    let msg = serde_json::to_string(&result.content).expect("content");
    assert!(msg.contains("saved") && msg.contains("retry"), "actionable msg: {msg}");

    let db = s.db_handle();
    let db = db.lock().await;
    let (sessions, transcript): (i64, String) = (
        db.conn().query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).expect("count"),
        db.conn()
            .query_row("SELECT transcript FROM sessions LIMIT 1", [], |r| r.get(0))
            .expect("transcript"),
    );
    assert_eq!(sessions, 1, "transcript must be persisted despite failure");
    assert!(transcript.contains("litestream"), "full transcript saved");
    let memories: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .expect("memories");
    assert_eq!(memories, 0);
}
