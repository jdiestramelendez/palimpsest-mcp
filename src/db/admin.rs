//! Database administration: online backup and verified restore.
//!
//! Backups use SQLite's `.backup` API (safe while the server runs under
//! WAL). Restores never destroy data: the incoming file is verified on a
//! temporary copy first, and the current database is renamed aside
//! (`.pre-restore-<timestamp>`), not deleted.

use std::path::{Path, PathBuf};

use rusqlite::backup::Backup;

use crate::db::Db;
use crate::error::{Error, Result};

/// Pages copied per backup step (with a tiny pause between steps so an
/// active writer is never starved).
const PAGES_PER_STEP: std::num::NonZeroU32 = std::num::NonZeroU32::new(256).unwrap();

fn timestamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Default backup directory: `<data dir>/backups`.
pub fn default_backup_dir() -> PathBuf {
    crate::config::Config::data_dir().join("backups")
}

/// Take an online backup of the database at `db_path` into `out`
/// (default: `backups/palimpsest-<timestamp>.db` next to the config).
/// With `keep = Some(n)`, older `palimpsest-*.db` files in the default
/// directory are rotated away, keeping the newest `n` (rotation only
/// applies to the default directory — explicit `--out` paths are never
/// touched). Returns the written path.
pub fn backup_database(db_path: &Path, out: Option<PathBuf>, keep: Option<usize>) -> Result<PathBuf> {
    if !db_path.exists() {
        return Err(Error::InvalidInput(format!(
            "no database at {}; nothing to back up",
            db_path.display()
        )));
    }
    let (dest, rotate_dir) = match out {
        Some(p) => (p, None),
        None => {
            let dir = default_backup_dir();
            std::fs::create_dir_all(&dir)?;
            (dir.join(format!("palimpsest-{}.db", timestamp())), Some(dir))
        }
    };
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    if dest == db_path {
        return Err(Error::InvalidInput(
            "backup destination equals the live database path".into(),
        ));
    }

    let src = rusqlite::Connection::open(db_path)?;
    let mut dst = rusqlite::Connection::open(&dest)?;
    {
        let backup = Backup::new(&src, &mut dst)?;
        backup.run_to_completion(
            PAGES_PER_STEP.get() as std::os::raw::c_int,
            std::time::Duration::from_millis(10),
            None,
        )?;
    }

    if let (Some(dir), Some(n)) = (rotate_dir, keep) {
        rotate_backups(&dir, n)?;
    }
    Ok(dest)
}

/// Keep the newest `n` `palimpsest-*.db` files in `dir`; remove the rest.
/// (This deletes backup FILES only — memories are never deleted.)
fn rotate_backups(dir: &Path, n: usize) -> Result<()> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .map(|f| f.starts_with("palimpsest-") && f.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();
    backups.sort(); // timestamp names sort chronologically
    while backups.len() > n.max(1) {
        let victim = backups.remove(0);
        std::fs::remove_file(&victim)?;
        tracing::info!(file = %victim.display(), "rotated old backup");
    }
    Ok(())
}

/// Restore the database at `db_path` from `source`.
///
/// The source is copied to a temporary file and verified there (it must
/// open, pass `PRAGMA integrity_check`, and match the configured `dims` —
/// migrations run on the copy, never on the original backup). Only then is
/// the current database renamed aside (`.pre-restore-<timestamp>`, along
/// with its `-wal`/`-shm` siblings) and the verified copy moved into place.
pub fn restore_database(db_path: &Path, source: &Path, dims: usize) -> Result<PathBuf> {
    if !source.exists() {
        return Err(Error::InvalidInput(format!(
            "backup file {} does not exist",
            source.display()
        )));
    }
    if source == db_path {
        return Err(Error::InvalidInput(
            "source equals the live database path; nothing to restore".into(),
        ));
    }
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // 1. Verify on a scratch copy (never mutate the backup itself).
    let tmp = db_path.with_extension("restore-tmp");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(source, &tmp)?;
    let verify = (|| -> Result<()> {
        let db = Db::open(&tmp, dims)?; // enforces dims + runs migrations
        let verdict: String = db
            .conn()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if verdict != "ok" {
            return Err(Error::InvalidInput(format!(
                "backup failed integrity_check: {verdict}"
            )));
        }
        Ok(())
    })();
    if let Err(e) = verify {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // 2. Move the current database aside (never delete).
    let mut kept_aside = None;
    if db_path.exists() {
        let aside = db_path.with_extension(format!("pre-restore-{}", timestamp()));
        std::fs::rename(db_path, &aside)?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
            if side.exists() {
                let _ = std::fs::rename(&side, PathBuf::from(format!("{}{suffix}", aside.display())));
            }
        }
        kept_aside = Some(aside);
    }

    // 3. Swap the verified copy in.
    std::fs::rename(&tmp, db_path)?;
    Ok(kept_aside.unwrap_or_else(|| db_path.to_path_buf()))
}

/// Meta key holding the embedder model the vectors were produced with.
pub const META_EMBEDDER_MODEL: &str = "embedder_model";

/// Statistics from a completed reembed.
#[derive(Debug)]
pub struct ReembedStats {
    /// Memories re-embedded.
    pub memories: usize,
    /// Where the previous database was kept.
    pub previous: PathBuf,
}

/// Re-embed every memory with the CURRENT config's embedder into a fresh
/// database, then swap it in. All rows (ids, validity, supersession links,
/// sessions, projects, graph) are copied verbatim — only `memories_vec`
/// (and the pinned dims/model in `meta`) change. The old database is kept
/// as `.pre-reembed-<ts>`, never deleted.
///
/// Refuses to run when the stored dims AND recorded model already match
/// the config (a guaranteed no-op).
pub async fn reembed_database(
    db_path: &Path,
    embedder: &dyn crate::providers::Embedder,
    new_model: &str,
    batch_size: usize,
) -> Result<ReembedStats> {
    if !db_path.exists() {
        return Err(Error::InvalidInput(format!(
            "no database at {}; nothing to re-embed",
            db_path.display()
        )));
    }
    let new_dims = embedder.dims();
    let batch_size = batch_size.clamp(1, 512);

    // Inspect the source WITHOUT enforcing config dims (the mismatch is
    // exactly why the user is here).
    let src = rusqlite::Connection::open(db_path)?;
    let stored_dims: usize = src
        .query_row("SELECT value FROM meta WHERE key = 'embedding_dims'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::Config("source database has no pinned dims".into()))?;
    let stored_model: Option<String> = src
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [META_EMBEDDER_MODEL],
            |r| r.get(0),
        )
        .ok();
    if stored_dims == new_dims && stored_model.as_deref() == Some(new_model) {
        return Err(Error::InvalidInput(format!(
            "database is already embedded with '{new_model}' at {new_dims} dims; nothing to do"
        )));
    }
    drop(src);

    // Fresh database at the new dims; copy every non-vector table verbatim
    // via ATTACH (FTS refills itself through the insert triggers).
    let tmp = db_path.with_extension("reembed-tmp");
    let _ = std::fs::remove_file(&tmp);
    let result = async {
        let mut new_db = Db::open(&tmp, new_dims)?;
        new_db.conn().execute(
            "ATTACH DATABASE ?1 AS src",
            [db_path.to_string_lossy().as_ref()],
        )?;
        new_db.conn().execute_batch(
            "BEGIN;
             INSERT INTO projects SELECT * FROM src.projects;
             INSERT INTO sessions SELECT * FROM src.sessions;
             INSERT INTO memories SELECT * FROM src.memories;
             INSERT INTO entities SELECT * FROM src.entities;
             INSERT INTO memory_entities SELECT * FROM src.memory_entities;
             INSERT INTO relations SELECT * FROM src.relations;
             COMMIT;",
        )?;
        new_db.conn().execute_batch("DETACH DATABASE src")?;

        // Batch re-embedding, one transaction per batch.
        let rows: Vec<(i64, String)> = {
            let mut stmt = new_db
                .conn()
                .prepare("SELECT id, content FROM memories ORDER BY id")?;
            let collected = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            collected
        };
        let total = rows.len();
        for (done, batch) in rows.chunks(batch_size).enumerate() {
            let texts: Vec<String> = batch.iter().map(|(_, c)| c.clone()).collect();
            let vectors = embedder.embed(&texts).await?;
            if vectors.len() != batch.len() {
                return Err(Error::Provider(format!(
                    "embedder returned {} vectors for {} inputs",
                    vectors.len(),
                    batch.len()
                )));
            }
            let tx = new_db.conn_mut().transaction()?;
            for ((id, _), vector) in batch.iter().zip(&vectors) {
                if vector.len() != new_dims {
                    return Err(Error::Provider(format!(
                        "embedder produced {} dims, expected {new_dims}",
                        vector.len()
                    )));
                }
                tx.execute(
                    "INSERT INTO memories_vec (memory_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![id, crate::db::queries::embedding_to_blob(vector)],
                )?;
            }
            tx.commit()?;
            tracing::info!(
                embedded = (done * batch_size + batch.len()).min(total),
                total,
                "reembed progress"
            );
        }

        // Sanity: row counts must match the source exactly.
        new_db.conn().execute(
            "ATTACH DATABASE ?1 AS src",
            [db_path.to_string_lossy().as_ref()],
        )?;
        for table in ["projects", "sessions", "memories", "entities", "memory_entities", "relations"] {
            let (a, b): (i64, i64) = new_db.conn().query_row(
                &format!("SELECT (SELECT COUNT(*) FROM {table}), (SELECT COUNT(*) FROM src.{table})"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if a != b {
                return Err(Error::Config(format!(
                    "count mismatch in {table}: {a} vs {b}; aborting swap"
                )));
            }
        }
        let vec_count: i64 = new_db
            .conn()
            .query_row("SELECT COUNT(*) FROM memories_vec", [], |r| r.get(0))?;
        if vec_count as usize != total {
            return Err(Error::Config(format!(
                "vector count {vec_count} != memory count {total}; aborting swap"
            )));
        }
        new_db.conn().execute_batch("DETACH DATABASE src")?;
        new_db.set_meta(META_EMBEDDER_MODEL, new_model)?;
        Ok(total)
    }
    .await;

    let total = match result {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };

    // Swap: keep the old database aside, move the new one into place.
    let aside = db_path.with_extension(format!("pre-reembed-{}", timestamp()));
    std::fs::rename(db_path, &aside)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if side.exists() {
            let _ = std::fs::rename(&side, PathBuf::from(format!("{}{suffix}", aside.display())));
        }
    }
    std::fs::rename(&tmp, db_path)?;
    // The fresh file's own -wal/-shm (if any) follow the rename by name.
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", tmp.display()));
        if side.exists() {
            let _ = std::fs::rename(&side, PathBuf::from(format!("{}{suffix}", db_path.display())));
        }
    }

    Ok(ReembedStats {
        memories: total,
        previous: aside,
    })
}
