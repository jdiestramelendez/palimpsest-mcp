//! palimpsest-mcp binary entry point: load config, open the database, build
//! providers, and serve the MCP tool set over stdio.
//!
//! All logging goes to stderr — stdout belongs to the MCP transport.

use std::path::PathBuf;

use clap::Parser;
use palimpsest_mcp::config::Config;
use palimpsest_mcp::db::Db;
use palimpsest_mcp::providers;
use palimpsest_mcp::tools::MemoryServer;
use rmcp::ServiceExt;

/// CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "palimpsest-mcp", version, about = "Persistent memory MCP server")]
struct Cli {
    /// Override the database path (default: ~/.palimpsest-mcp/palimpsest.db).
    #[arg(long)]
    db_path: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = Config::load()?;
    let db_path = config.resolve_db_path(cli.db_path.as_deref());
    let db = Db::open(&db_path, config.embedder.dims)?;
    tracing::info!(db = %db_path.display(), "database ready");

    let (embedder, extractor) = providers::from_config(&config)?;
    let server = MemoryServer::new(db, embedder, extractor);

    let service = server.serve(rmcp::transport::stdio()).await?;
    tracing::info!("palimpsest-mcp serving over stdio");
    service.waiting().await?;
    Ok(())
}
