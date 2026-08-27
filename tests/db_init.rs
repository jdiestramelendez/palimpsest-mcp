//! Database initialization tests (Step 1).

use std::collections::HashSet;

use palimpsest_mcp::db::Db;

/// All tables and virtual tables the schema must create.
const EXPECTED_TABLES: &[&str] = &[
    "meta",
    "projects",
    "sessions",
    "memories",
    "memories_vec",
    "memories_fts",
];

#[test]
fn fresh_init_creates_all_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("palimpsest.db");
    let db = Db::open(&path, 768).expect("open");

    let mut stmt = db
        .conn()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .expect("prepare");
    let names: HashSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("collect");

    for t in EXPECTED_TABLES {
        assert!(names.contains(*t), "missing table {t}; got {names:?}");
    }

    // Partial index + FTS triggers exist.
    let idx: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_memories_valid'",
            [],
            |r| r.get(0),
        )
        .expect("index query");
    assert_eq!(idx, 1, "partial validity index missing");

    let triggers: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'memories_fts_%'",
            [],
            |r| r.get(0),
        )
        .expect("trigger query");
    assert_eq!(triggers, 3, "expected the three FTS sync triggers");
}

#[test]
fn reopen_preserves_data_and_skips_migrations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("palimpsest.db");

    {
        let db = Db::open(&path, 768).expect("first open");
        db.conn()
            .execute(
                "INSERT INTO projects (id, name) VALUES ('proj-a', 'Project A')",
                [],
            )
            .expect("insert");
    }

    let db = Db::open(&path, 768).expect("re-open");
    let name: String = db
        .conn()
        .query_row("SELECT name FROM projects WHERE id = 'proj-a'", [], |r| r.get(0))
        .expect("row survived re-open");
    assert_eq!(name, "Project A");

    // Migrations did not duplicate meta rows.
    let dims_rows: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM meta WHERE key = 'embedding_dims'",
            [],
            |r| r.get(0),
        )
        .expect("meta query");
    assert_eq!(dims_rows, 1);
}

#[test]
fn mismatched_dims_is_rejected_with_explanation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("palimpsest.db");
    Db::open(&path, 768).expect("create at 768");

    let err = match Db::open(&path, 1536) {
        Err(e) => e,
        Ok(_) => panic!("opening with mismatched dims must fail"),
    };
    let msg = err.to_string();
    assert!(msg.contains("768") && msg.contains("1536"), "msg: {msg}");
    assert!(msg.to_lowercase().contains("re-embed"), "msg must explain re-embedding: {msg}");
}
