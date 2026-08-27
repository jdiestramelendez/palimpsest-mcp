//! Crate-wide error type.

use thiserror::Error;

/// Unified error type for memory-mcp.
#[derive(Debug, Error)]
pub enum Error {
    /// Underlying SQLite failure.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// Configuration file could not be read or parsed.
    #[error("config error: {0}")]
    Config(String),

    /// The configured embedding dimensionality does not match the database.
    #[error(
        "embedding dims mismatch: config says {config_dims}, database was created with {db_dims}. \
         Changing embedders requires re-embedding every memory; either restore the previous \
         embedder config or create a new database (--db-path) and re-embed."
    )]
    DimsMismatch {
        /// Dims from config.toml.
        config_dims: usize,
        /// Dims pinned in the database meta table.
        db_dims: usize,
    },

    /// Filesystem-level failure (creating the data dir, reading config).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A provider (embedder/extractor) HTTP call failed.
    #[error("provider error: {0}")]
    Provider(String),

    /// Input validation failure surfaced to the MCP caller.
    #[error("{0}")]
    InvalidInput(String),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
