//! Database layer: opening, migrations, and typed queries.
//!
//! A single SQLite file holds everything: projects, sessions, memories, the
//! sqlite-vec vector index and the FTS5 lexical index. The embedding
//! dimensionality is pinned in the `meta` table at first creation and the DB
//! refuses to open when the configured dims differ.

pub mod queries;

use std::path::Path;
use std::sync::Once;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Embedded schema; `{DIMS}` is substituted at first creation.
const SCHEMA_TEMPLATE: &str = include_str!("schema.sql");

/// Schema version recorded in `meta` (bump on migration-worthy changes).
const SCHEMA_VERSION: &str = "1";

static VEC_INIT: Once = Once::new();

/// Register sqlite-vec as an auto-extension for every future connection.
/// Safe to call multiple times; the registration happens once per process.
fn register_sqlite_vec() {
    VEC_INIT.call_once(|| {
        // SAFETY: sqlite3_vec_init is the canonical sqlite-vec entry point,
        // transmuted to the auto-extension signature exactly as the
        // sqlite-vec crate documents.
        unsafe {
            type AutoExtensionFn = unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::os::raw::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::os::raw::c_int;
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), AutoExtensionFn>(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// Handle to the memory database.
pub struct Db {
    conn: Connection,
    dims: usize,
}

impl Db {
    /// Open (creating if necessary) the database at `path`, enable WAL and
    /// foreign keys, and run migrations. `dims` is the configured embedding
    /// dimensionality; on an existing database it must match the pinned
    /// value or an explanatory [`Error::DimsMismatch`] is returned.
    pub fn open(path: &Path, dims: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        register_sqlite_vec();
        let conn = Connection::open(path)?;
        Self::init(conn, dims)
    }

    /// Open an in-memory database (tests only, but harmless elsewhere).
    pub fn open_in_memory(dims: usize) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        Self::init(conn, dims)
    }

    fn init(conn: Connection, dims: usize) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let migrated: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get(0),
        )?;

        if migrated {
            let stored: Option<String> = conn
                .query_row("SELECT value FROM meta WHERE key = 'embedding_dims'", [], |r| {
                    r.get(0)
                })
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            let db_dims: usize = stored
                .as_deref()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| Error::Config("meta.embedding_dims missing or corrupt".into()))?;
            if db_dims != dims {
                return Err(Error::DimsMismatch {
                    config_dims: dims,
                    db_dims,
                });
            }
        } else {
            let schema = SCHEMA_TEMPLATE.replace("{DIMS}", &dims.to_string());
            conn.execute_batch(&schema)?;
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('embedding_dims', ?1), ('schema_version', ?2)",
                rusqlite::params![dims.to_string(), SCHEMA_VERSION],
            )?;
        }

        Ok(Self { conn, dims })
    }

    /// The embedding dimensionality this database was created with.
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// Borrow the underlying connection (query layer only).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutably borrow the underlying connection (for transactions).
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}
