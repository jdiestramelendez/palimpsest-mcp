//! palimpsest-mcp binary entry point.
//!
//! - No subcommand: serve the MCP tool set over stdio.
//! - `ingest`: run the extraction pipeline on a transcript file (idempotent).
//! - `hook session-start` / `hook session-end`: Claude Code lifecycle hook
//!   entry points — they read the hook's JSON payload from stdin.
//!
//! All logging goes to stderr — stdout belongs to the MCP transport (and,
//! for `hook session-start`, to the injected context).

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use palimpsest_mcp::config::Config;
use palimpsest_mcp::core::ingest::{self, IngestOutcome};
use palimpsest_mcp::db::{queries, Db};
use palimpsest_mcp::providers;
use palimpsest_mcp::tools::MemoryServer;
use rmcp::ServiceExt;
use serde::Deserialize;

/// Keep hook-driven extraction prompts bounded: ingest at most this much
/// of the transcript tail (the wrap-up carries the most durable signal).
const HOOK_TRANSCRIPT_MAX_BYTES: usize = 120_000;

/// CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "palimpsest-mcp", version, about = "Persistent memory MCP server")]
struct Cli {
    /// Override the database path (default: ~/.palimpsest-mcp/palimpsest.db).
    #[arg(long, global = true)]
    db_path: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands; absent = serve MCP over stdio.
#[derive(Debug, Subcommand)]
enum Command {
    /// Ingest a transcript file: persist it and extract durable memories.
    /// Re-running on the same content is a no-op.
    Ingest {
        /// Path to the transcript file.
        #[arg(long)]
        transcript: PathBuf,
        /// Project name/id/alias (auto-created). Omit for global scope.
        #[arg(long)]
        project: Option<String>,
        /// Client label recorded on the session.
        #[arg(long, default_value = "cli")]
        client: String,
        /// Transcript format: plain text, or Claude Code session JSONL.
        #[arg(long, value_parser = ["text", "claude-jsonl"], default_value = "text")]
        format: String,
    },
    /// Claude Code lifecycle hook entry points (hook JSON on stdin).
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
}

/// Supported hook events.
#[derive(Debug, Subcommand)]
enum HookEvent {
    /// SessionStart: print the project's state document to stdout so
    /// Claude Code injects it as context.
    SessionStart,
    /// SessionEnd / PreCompact: ingest the session transcript.
    SessionEnd,
}

/// The subset of Claude Code's hook payload we consume.
#[derive(Debug, Deserialize)]
struct HookPayload {
    #[serde(default)]
    transcript_path: Option<PathBuf>,
    #[serde(default)]
    cwd: Option<PathBuf>,
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

    match cli.command {
        None => serve(&config, &db_path).await,
        Some(Command::Ingest {
            transcript,
            project,
            client,
            format,
        }) => {
            let raw = std::fs::read_to_string(&transcript)?;
            let text = match format.as_str() {
                "claude-jsonl" => ingest::transcript_from_claude_jsonl(&raw),
                _ => raw,
            };
            run_ingest(&config, &db_path, project, &client, &text).await
        }
        Some(Command::Hook { event }) => {
            let payload: HookPayload = serde_json::from_reader(std::io::stdin())
                .unwrap_or(HookPayload {
                    transcript_path: None,
                    cwd: None,
                });
            let project = payload
                .cwd
                .as_deref()
                .and_then(ingest::project_from_cwd);
            match event {
                HookEvent::SessionStart => hook_session_start(&config, &db_path, project),
                HookEvent::SessionEnd => {
                    let Some(path) = payload.transcript_path else {
                        tracing::warn!("hook payload has no transcript_path; nothing to ingest");
                        return Ok(());
                    };
                    let raw = match std::fs::read_to_string(&path) {
                        Ok(raw) => raw,
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "cannot read transcript");
                            return Ok(());
                        }
                    };
                    let text = ingest::transcript_from_claude_jsonl(&raw);
                    let text = ingest::tail_transcript(&text, HOOK_TRANSCRIPT_MAX_BYTES);
                    if text.trim().is_empty() {
                        tracing::info!("empty conversation; nothing to ingest");
                        return Ok(());
                    }
                    run_ingest(&config, &db_path, project, "claude-code", text).await
                }
            }
        }
    }
}

/// Serve the MCP tool set over stdio.
async fn serve(config: &Config, db_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::open(db_path, config.embedder.dims)?;
    tracing::info!(db = %db_path.display(), "database ready");
    let (embedder, extractor) = providers::from_config(config)?;
    let server = MemoryServer::new(db, embedder, extractor);
    let service = server.serve(rmcp::transport::stdio()).await?;
    tracing::info!("palimpsest-mcp serving over stdio");
    service.waiting().await?;
    Ok(())
}

/// Run the ingestion pipeline for a plain-text transcript.
async fn run_ingest(
    config: &Config,
    db_path: &std::path::Path,
    project: Option<String>,
    client: &str,
    transcript: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut db = Db::open(db_path, config.embedder.dims)?;
    let (embedder, extractor) = providers::from_config(config)?;

    let project_id = match &project {
        Some(name) => Some(queries::ensure_project(db.conn(), name)?.id),
        None => None,
    };

    let outcome = ingest::ingest_transcript(
        &mut db,
        embedder.as_ref(),
        extractor.as_ref(),
        project_id.as_deref(),
        Some(client),
        transcript,
    )
    .await?;

    match outcome {
        IngestOutcome::AlreadyIngested { session_id } => {
            tracing::info!(session_id, "transcript already ingested; nothing to do");
        }
        IngestOutcome::Completed(result) => {
            tracing::info!(
                session_id = result.session_id,
                facts = result.facts_extracted,
                created = result.memories_created,
                superseded = result.memories_superseded,
                duplicates = result.duplicates_skipped,
                state_updated = result.state_updated,
                "ingestion complete"
            );
        }
        IngestOutcome::ExtractionFailed { session_id, error } => {
            tracing::error!(session_id, error = %error,
                "transcript saved but extraction failed; re-run ingest to retry");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// SessionStart hook: emit the project's state to stdout for injection.
/// Unknown project or empty state prints nothing (a hook must never fail
/// the session).
fn hook_session_start(
    config: &Config,
    db_path: &std::path::Path,
    project: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(name) = project else { return Ok(()) };
    let db = match Db::open(db_path, config.embedder.dims) {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "cannot open database; skipping state injection");
            return Ok(());
        }
    };
    let Ok(Some(p)) = queries::resolve_project(db.conn(), &name) else {
        return Ok(());
    };
    if p.state_md.trim().is_empty() {
        return Ok(());
    }
    println!(
        "Persistent project state for '{}' (from palimpsest-mcp; use its search_memory \
         tool for anything not covered here):\n\n{}",
        p.id, p.state_md
    );
    Ok(())
}
