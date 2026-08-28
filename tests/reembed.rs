//! Ticket 008 tests: re-embedding into a new database with a verbatim
//! copy of everything except the vectors.

use palimpsest_mcp::db::admin::{reembed_database, META_EMBEDDER_MODEL};
use palimpsest_mcp::db::queries::{self, EntityRef, GraphPayload, NewMemory, TripleRef};
use palimpsest_mcp::db::Db;
use palimpsest_mcp::providers::mock::MockEmbedder;

/// Seed: project, session, supersession chain, graph rows.
fn seed(db_path: &std::path::Path, dims: usize) -> (i64, i64) {
    let mut db = Db::open(db_path, dims).expect("open");
    queries::ensure_project(db.conn(), "p").expect("project");
    let session = queries::insert_session(db.conn(), Some("p"), Some("test"), "transcript", "hash1")
        .expect("session");
    let old = queries::insert_memory(
        db.conn_mut(),
        &NewMemory {
            project_id: Some("p".into()),
            content: "we deploy on fridays".into(),
            kind: "fact".into(),
            importance: 3,
            source_session_id: Some(session),
        },
        &vec![0.1; dims],
        None,
        None,
    )
    .expect("old");
    let new = queries::insert_memory(
        db.conn_mut(),
        &NewMemory {
            project_id: Some("p".into()),
            content: "we deploy on mondays".into(),
            kind: "fact".into(),
            importance: 3,
            source_session_id: Some(session),
        },
        &vec![0.2; dims],
        Some(old),
        Some(&GraphPayload {
            entities: vec![EntityRef { name: "deploys".into(), kind: "concept".into() }],
            triples: vec![TripleRef {
                source: "p".into(), predicate: "does".into(), target: "deploys".into(),
            }],
        }),
    )
    .expect("new");
    (old, new)
}

#[tokio::test]
async fn reembed_preserves_everything_but_vectors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("live.db");
    let (old_id, new_id) = seed(&db_path, 8);

    // New embedder at DIFFERENT dims.
    let embedder = MockEmbedder::new(16);
    let stats = reembed_database(&db_path, &embedder, "new-model", 4)
        .await
        .expect("reembed");
    assert_eq!(stats.memories, 2);
    assert!(stats.previous.exists(), "old file kept: {:?}", stats.previous);
    assert!(stats.previous.to_string_lossy().contains("pre-reembed"));

    // Opens at the NEW dims (old dims must now be refused).
    assert!(Db::open(&db_path, 8).is_err(), "old dims refused after reembed");
    let db = Db::open(&db_path, 16).expect("opens at new dims");
    assert_eq!(db.get_meta(META_EMBEDDER_MODEL).expect("meta"), Some("new-model".into()));

    // Rows identical: ids, supersession links, sessions, graph.
    let (superseded_by, invalid): (Option<i64>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT superseded_by, invalid_at FROM memories WHERE id = ?1",
            [old_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("old row");
    assert_eq!(superseded_by, Some(new_id));
    assert!(invalid.is_some());
    for (table, expected) in [("memories", 2i64), ("sessions", 1), ("relations", 1), ("entities", 2)] {
        let n: i64 = db
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .expect(table);
        assert_eq!(n, expected, "{table}");
    }
    // Vectors regenerated at the new dimensionality and searchable.
    let (vecs, fts): (i64, i64) = (
        db.conn().query_row("SELECT COUNT(*) FROM memories_vec", [], |r| r.get(0)).expect("vec"),
        db.conn()
            .query_row("SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH 'deploy'", [], |r| r.get(0))
            .expect("fts"),
    );
    assert_eq!((vecs, fts), (2, 2));
    let embedding = {
        use palimpsest_mcp::providers::Embedder as _;
        embedder
            .embed(&["we deploy on mondays".to_string()])
            .await
            .expect("embed")
            .remove(0)
    };
    let hits = palimpsest_mcp::core::retrieval::search(
        db.conn(),
        "we deploy on mondays",
        &embedding,
        &palimpsest_mcp::core::retrieval::SearchParams {
            project_id: Some("p".into()),
            kinds: None,
            limit: 5,
            as_of: None,
            recency: None,
        },
    )
    .expect("search works");
    assert!(hits.iter().any(|h| h.memory.id == new_id), "search finds the valid fact");

    // The kept-aside file still opens at the OLD dims with its data.
    let prev = Db::open(&stats.previous, 8).expect("aside opens at old dims");
    let n: i64 = prev
        .conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 2);
}

#[tokio::test]
async fn noop_reembed_is_refused_and_failures_leave_source_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("live.db");
    seed(&db_path, 8);

    // First reembed records the model (same dims: allowed because the
    // stored model is unknown).
    let embedder = MockEmbedder::new(8);
    reembed_database(&db_path, &embedder, "model-a", 32).await.expect("first");

    // Same dims AND same model -> guaranteed no-op, refused.
    let err = reembed_database(&db_path, &embedder, "model-a", 32)
        .await
        .expect_err("noop refused");
    assert!(err.to_string().contains("nothing to do"), "{err}");

    // Model change with same dims IS allowed.
    reembed_database(&db_path, &embedder, "model-b", 32).await.expect("model change");

    // A failing embedder aborts and leaves the live DB untouched.
    let failing = palimpsest_mcp::providers::mock::MockEmbedder::new(4);
    // (dims 4 differs so the guard passes; embed succeeds but this checks
    // the tmp-cleanup path via a poisoned run: use a provider that errors)
    struct Exploding;
    #[async_trait::async_trait]
    impl palimpsest_mcp::providers::Embedder for Exploding {
        async fn embed(&self, _t: &[String]) -> palimpsest_mcp::error::Result<Vec<Vec<f32>>> {
            Err(palimpsest_mcp::error::Error::Provider("boom".into()))
        }
        fn dims(&self) -> usize { 4 }
    }
    let _ = failing;
    reembed_database(&db_path, &Exploding, "model-c", 32)
        .await
        .expect_err("provider failure aborts");
    let db = Db::open(&db_path, 8).expect("live db intact at old dims");
    assert_eq!(db.get_meta(META_EMBEDDER_MODEL).expect("meta"), Some("model-b".into()));
    assert!(!db_path.with_extension("reembed-tmp").exists(), "tmp cleaned up");
}
