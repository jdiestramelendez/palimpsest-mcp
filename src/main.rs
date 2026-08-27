//! memory-mcp binary entry point. Loads config, opens the database, and
//! (from Step 3 onwards) serves the MCP tool set over stdio.

use std::path::PathBuf;

use clap::Parser;
use memory_mcp::config::Config;
use memory_mcp::db::Db;

/// CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "memory-mcp", version, about = "Persistent memory MCP server")]
struct Cli {
    /// Override the database path (default: ~/.memory-mcp/memory.db).
    #[arg(long)]
    db_path: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = Config::load()?;
    let db_path = config.resolve_db_path(cli.db_path.as_deref());
    let _db = Db::open(&db_path, config.embedder.dims)?;
    // MCP server bootstrap arrives in Step 3.
    eprintln!("memory-mcp: database ready at {}", db_path.display());
    Ok(())
}
