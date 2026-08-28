//! Ticket 001 tests: shared ingestion (hash idempotency), Claude Code
//! JSONL parsing, project derivation, and the v1->v2 migration.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;

use palimpsest_mcp::core::ingest::{
    project_from_cwd, tail_transcript, transcript_from_claude_jsonl, transcript_hash,
};
use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::{EndSessionRequest, MemoryServer};

const DIMS: usize = 16;

const EXTRACTION_ONE_FACT: &str =
    r#"{"summary": "s", "facts": [{"content": "acme uses SQLite", "kind": "fact", "importance": 3, "scope": "project"}]}"#;

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

#[tokio::test]
async fn same_transcript_is_never_ingested_twice() {
    // Enough scripted responses for TWO full runs — the second must not
    // consume any because the hash check short-circuits.
    let s = server_with(vec![
        EXTRACTION_ONE_FACT, "# state", EXTRACTION_ONE_FACT, "# state",
    ]);
    let req = || EndSessionRequest {
        project: Some("acme".into()),
        transcript: "[user] we use SQLite\n[assistant] noted".into(),
        client: Some("test".into()),
    };

    let first = s.end_session(Parameters(req())).await.expect("ok");
    let first_json = first.structured_content.expect("structured");
    assert_eq!(first_json["memories_created"], 1);
    let session_id = first_json["session_id"].as_i64().expect("id");

    let second = s.end_session(Parameters(req())).await.expect("ok");
    let second_json = second.structured_content.expect("structured");
    assert_eq!(second_json["already_ingested"], true);
    assert_eq!(second_json["session_id"], session_id);

    let db = s.db_handle();
    let db = db.lock().await;
    let (sessions, memories): (i64, i64) = (
        db.conn().query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).expect("s"),
        db.conn().query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0)).expect("m"),
    );
    assert_eq!((sessions, memories), (1, 1), "second run must be a full no-op");
}

#[test]
fn transcript_hash_is_stable_and_content_sensitive() {
    let a = transcript_hash("hello");
    assert_eq!(a, transcript_hash("hello"));
    assert_ne!(a, transcript_hash("hello!"));
    assert_eq!(a.len(), 64, "sha256 hex");
}

#[test]
fn jsonl_parser_handles_both_line_shapes_and_skips_noise() {
    let raw = r#"
{"role":"user","content":"flat form question"}
{"role":"assistant","content":"flat form answer"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"nested form question"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"nested answer"},{"type":"tool_use","name":"Bash"}]}}
{"type":"tool_result","tool_use_id":"t1","content":"noise"}
{"type":"summary","summary":"noise"}
not even json
"#;
    let text = transcript_from_claude_jsonl(raw);
    assert!(text.contains("[user] flat form question"));
    assert!(text.contains("[assistant] flat form answer"));
    assert!(text.contains("[user] nested form question"));
    assert!(text.contains("[assistant] nested answer"));
    assert!(!text.contains("noise"), "tool results/summaries must be skipped: {text}");
    assert_eq!(text.lines().count(), 4);
}

#[test]
fn tail_transcript_cuts_on_line_boundary_and_utf8_safely() {
    let t = "línea uno ñ\nlínea dos é\nlínea tres ü\n";
    let tail = tail_transcript(t, 16);
    assert!(tail.len() <= 16);
    assert!(tail.starts_with("línea tres"), "cut on line boundary: {tail:?}");
    // Degenerate case: no newline in window, mid-char start.
    let no_nl = "ññññññññ";
    let cut = tail_transcript(no_nl, 3);
    assert!(cut.chars().all(|c| c == 'ñ'), "must not split a char: {cut:?}");
}

#[test]
fn project_derives_from_git_root_or_dirname() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("my-repo");
    let nested = repo.join("src").join("deep");
    std::fs::create_dir_all(&nested).expect("mkdirs");
    std::fs::create_dir_all(repo.join(".git")).expect("git dir");
    assert_eq!(project_from_cwd(&nested).as_deref(), Some("my-repo"));

    let plain = dir.path().join("loose-dir");
    std::fs::create_dir_all(&plain).expect("mkdir");
    assert_eq!(project_from_cwd(&plain).as_deref(), Some("loose-dir"));
}

#[test]
fn v1_database_is_migrated_to_current_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("old.db");
    // Create a fresh DB, then rewind it to schema v1 (drop the hash column
    // artifacts and the version marker) to simulate a pre-migration file.
    {
        let db = palimpsest_mcp::db::Db::open(&path, DIMS).expect("create");
        db.conn()
            .execute_batch(
                "DROP INDEX idx_sessions_hash;
                 ALTER TABLE sessions DROP COLUMN transcript_hash;
                 UPDATE meta SET value = '1' WHERE key = 'schema_version';",
            )
            .expect("rewind to v1");
    }
    let db = palimpsest_mcp::db::Db::open(&path, DIMS).expect("re-open migrates");
    let cols: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'transcript_hash'",
            [],
            |r| r.get(0),
        )
        .expect("pragma");
    assert_eq!(cols, 1, "migration must add transcript_hash");
    let version: String = db
        .conn()
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, "3");
    let graph_tables: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'
             AND name IN ('entities','memory_entities','relations')",
            [],
            |r| r.get(0),
        )
        .expect("graph tables");
    assert_eq!(graph_tables, 3, "v3 migration must create the graph tables");
}
