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
    /// Export the knowledge graph: interactive HTML (default), Mermaid
    /// (.mmd) or Graphviz DOT (.dot), chosen by the --out extension.
    Graph {
        /// Scope to one project (global entities always included).
        #[arg(long)]
        project: Option<String>,
        /// Render the graph as it was at this ISO 8601 instant.
        #[arg(long)]
        as_of: Option<String>,
        /// Output file: .html, .mmd or .dot.
        #[arg(long, default_value = "graph.html")]
        out: PathBuf,
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
        Some(Command::Graph { project, as_of, out }) => {
            export_graph(&config, &db_path, project, as_of, &out)
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
    let server = MemoryServer::new(db, embedder, extractor).with_retrieval_config(config.retrieval)
        .with_review_config(config.review);
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

/// Export the knowledge graph to HTML / Mermaid / DOT.
fn export_graph(
    config: &Config,
    db_path: &std::path::Path,
    project: Option<String>,
    as_of: Option<String>,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use palimpsest_mcp::core::{graphviz, retrieval};

    let db = Db::open(db_path, config.embedder.dims)?;
    let project_id = match &project {
        Some(name) => Some(
            queries::resolve_project(db.conn(), name)?
                .ok_or_else(|| format!("project '{name}' not found"))?
                .id,
        ),
        None => None,
    };
    let as_of_norm = as_of.as_deref().map(retrieval::normalize_as_of).transpose()?;
    let (nodes, edges) = queries::graph_export(db.conn(), project_id.as_deref())?;
    if nodes.is_empty() {
        eprintln!("the graph is empty — entities appear once sessions are ingested");
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let t = as_of_norm.clone().unwrap_or(now);
    let title = match &project_id {
        Some(p) => format!("{p} — knowledge graph"),
        None => "knowledge graph — all projects".to_string(),
    };
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("html");
    let rendered = match ext {
        "mmd" => graphviz::render_mermaid(&nodes, &edges, &t),
        "dot" => graphviz::render_dot(&nodes, &edges, &t),
        "html" => graphviz::render_html(&nodes, &edges, &title, as_of_norm.as_deref()),
        other => return Err(format!("unsupported extension '.{other}': use .html, .mmd or .dot").into()),
    };
    std::fs::write(out, rendered)?;
    println!("{} nodes, {} edges -> {}", nodes.len(), edges.len(), out.display());
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
