//! Ticket 007 tests: online backup and verified restore.

use palimpsest_mcp::db::admin::{backup_database, restore_database};
use palimpsest_mcp::db::{queries, Db};

const DIMS: usize = 8;

fn save(db: &mut Db, content: &str) -> i64 {
    queries::ensure_project(db.conn(), "p").expect("project");
    queries::insert_memory(
        db.conn_mut(),
        &queries::NewMemory {
            project_id: Some("p".into()),
            content: content.into(),
            kind: "fact".into(),
            importance: 3,
            source_session_id: None,
        },
        &[0.5; DIMS],
        None,
        None,
    )
    .expect("insert")
}

fn count(db: &Db) -> i64 {
    db.conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .expect("count")
}

#[test]
fn backup_taken_with_open_connection_is_consistent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("live.db");
    let mut db = Db::open(&db_path, DIMS).expect("open");
    save(&mut db, "fact one");
    save(&mut db, "fact two");

    // Connection stays open (WAL) while the backup runs.
    let out = dir.path().join("snap.db");
    let written = backup_database(&db_path, Some(out.clone()), None).expect("backup");
    assert_eq!(written, out);

    let back = Db::open(&out, DIMS).expect("backup opens");
    assert_eq!(count(&back), 2);
    let verdict: String = back
        .conn()
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity");
    assert_eq!(verdict, "ok");
    // FTS came along too.
    let fts: i64 = back
        .conn()
        .query_row("SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH 'fact'", [], |r| r.get(0))
        .expect("fts");
    assert_eq!(fts, 2);
}

#[test]
fn restore_swaps_atomically_and_keeps_previous_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("live.db");

    // State A: one memory; snapshot it.
    {
        let mut db = Db::open(&db_path, DIMS).expect("open");
        save(&mut db, "state A fact");
    }
    let snap = dir.path().join("snap.db");
    backup_database(&db_path, Some(snap.clone()), None).expect("backup");

    // State B: extra memory after the snapshot.
    {
        let mut db = Db::open(&db_path, DIMS).expect("reopen");
        save(&mut db, "state B fact");
        assert_eq!(count(&db), 2);
    }

    // Restore the snapshot: back to state A, state B preserved aside.
    let aside = restore_database(&db_path, &snap, DIMS).expect("restore");
    let db = Db::open(&db_path, DIMS).expect("restored opens");
    assert_eq!(count(&db), 1, "restored database is state A");
    assert!(aside.exists(), "previous database must be kept: {aside:?}");
    assert!(aside.to_string_lossy().contains("pre-restore"));
    let prev = Db::open(&aside, DIMS).expect("aside opens");
    assert_eq!(count(&prev), 2, "aside file is state B, nothing lost");
    // The backup source itself was not mutated by verification.
    let snap_db = Db::open(&snap, DIMS).expect("snapshot still opens");
    assert_eq!(count(&snap_db), 1);
}

#[test]
fn restore_refuses_dims_mismatch_and_garbage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("live.db");
    {
        let mut db = Db::open(&db_path, DIMS).expect("open");
        save(&mut db, "current fact");
    }

    // Backup created at other dims.
    let other = dir.path().join("other.db");
    Db::open(&other, 16).expect("other dims db");
    let err = restore_database(&db_path, &other, DIMS).expect_err("dims mismatch");
    assert!(err.to_string().contains("dims"), "{err}");

    // Garbage file.
    let junk = dir.path().join("junk.db");
    std::fs::write(&junk, b"not a sqlite file at all").expect("write junk");
    restore_database(&db_path, &junk, DIMS).expect_err("junk refused");

    // Live database untouched by failed restores.
    let db = Db::open(&db_path, DIMS).expect("live db intact");
    assert_eq!(count(&db), 1);
    assert!(!db_path.with_extension("restore-tmp").exists(), "tmp cleaned up");
}

#[test]
fn missing_source_and_self_paths_are_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("live.db");
    Db::open(&db_path, DIMS).expect("open");

    restore_database(&db_path, &dir.path().join("nope.db"), DIMS).expect_err("missing source");
    restore_database(&db_path, &db_path, DIMS).expect_err("self restore");
    backup_database(&db_path, Some(db_path.clone()), None).expect_err("self backup");
    backup_database(&dir.path().join("ghost.db"), None, None).expect_err("missing db");
}
