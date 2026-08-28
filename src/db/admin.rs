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
