//! memory-mcp library: persistent memory for coding agents over MCP.
//!
//! Modules:
//! - [`config`]: `~/.memory-mcp/config.toml` loading with Ollama defaults.
//! - [`db`]: SQLite (WAL, sqlite-vec, FTS5) open/migrate + typed queries.
//! - [`providers`]: `Embedder` / `Extractor` traits and implementations.
//! - [`core`]: hybrid RRF retrieval and the end-of-session extraction pipeline.
//! - [`tools`]: the MCP server and its four tools.

pub mod config;
pub mod core;
pub mod db;
pub mod error;
pub mod providers;
pub mod tools;
