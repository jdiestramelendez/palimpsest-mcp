//! The MCP server and its tool handlers.
//!
//! Handlers validate input, call typed queries / core logic, and translate
//! outcomes into MCP results. Recoverable, agent-visible failures return
//! `CallToolResult::error` (the agent sees the message and can adapt);
//! `Err(ErrorData)` is reserved for malformed protocol input and internal
//! server failures.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ErrorData, Implementation, ServerCapabilities, ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::core::ingest::{self, IngestOutcome};
use crate::config::RetrievalConfig;
use crate::core::retrieval::{self, RecencyBoost, SearchParams};
use crate::db::queries::{self, NewMemory, MEMORY_KINDS};
use crate::db::Db;
use crate::error::Error;
use crate::providers::{Embedder, Extractor};

/// Instructions surfaced to MCP clients at initialize time.
const INSTRUCTIONS: &str = "\
palimpsest-mcp gives you persistent memory across sessions. Call get_project_state \
with the project name at the START of every session to load its living state \
document and counts. Use search_memory whenever you are missing context \
(decisions, preferences, past facts). Use save_memory when the user asks you \
to remember something or an important decision is made. Call end_session with \
the conversation transcript when a session wraps up so durable facts are \
extracted and the project state is updated.";

/// The memory MCP server: shared DB plus provider handles.
#[derive(Clone)]
pub struct MemoryServer {
    db: Arc<Mutex<Db>>,
    embedder: Arc<dyn Embedder>,
    extractor: Arc<dyn Extractor>,
    retrieval_config: RetrievalConfig,
    tool_router: ToolRouter<Self>,
}

/// Parameters for `get_project_state`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetProjectStateRequest {
    /// Project name, id, or alias (case-insensitive).
    pub project: String,
}

/// Parameters for `save_memory`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SaveMemoryRequest {
    /// Project name, id, or alias. Omit for a GLOBAL memory (visible from
    /// every project). Unknown projects are created automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The fact to remember, as one self-contained sentence.
    pub content: String,
    /// Memory kind: fact | decision | preference | todo | insight. Default: fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Importance 1 (trivia) to 5 (critical). Default: 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<i64>,
    /// Id of an existing memory this one replaces. The old memory stays in
    /// history but stops being returned by current-time searches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<i64>,
}

/// Parameters for `search_memory`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchMemoryRequest {
    /// What to look for, in natural language. Both semantic similarity and
    /// exact terms are used.
    pub query: String,
    /// Project name, id, or alias to scope the search. Global memories are
    /// always included. Omit to search across all projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Restrict to these kinds (fact | decision | preference | todo | insight).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Maximum results (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// ISO 8601 instant for time-travel: return what was believed true at
    /// that moment (e.g. "2026-08-01T12:00:00Z"). Omit for current facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
}

/// Parameters for `end_session`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EndSessionRequest {
    /// Project name, id, or alias this session was about. Omit for
    /// project-less sessions (facts are stored as global).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The session transcript (or a faithful condensed version of it).
    pub transcript: String,
    /// Client that ran the session, e.g. "claude-code" or "cursor".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

/// Parameters for `get_related`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetRelatedRequest {
    /// Entity name to explore, e.g. "PostgreSQL" or "deploy pipeline".
    pub entity: String,
    /// Neighborhood radius: 1 (direct relations) or 2. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hops: Option<i64>,
    /// Project name, id, or alias to scope the lookup (global entities are
    /// always visible). Omit to search across all projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Build a tool-level error result with an agent-actionable message.
fn tool_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

/// Map an internal error to the right MCP layer: recoverable/domain errors
/// become tool-level errors the agent can read; everything else is a
/// protocol-level internal error.
fn map_err(e: Error) -> std::result::Result<CallToolResult, ErrorData> {
    match e {
        Error::InvalidInput(msg) => Ok(tool_error(msg)),
        Error::Provider(msg) => Ok(tool_error(format!(
            "provider call failed: {msg}. The operation was not applied; retry once the provider is reachable."
        ))),
        other => Err(ErrorData::internal_error(other.to_string(), None)),
    }
}

impl MemoryServer {
    /// Create a server over an opened database and provider pair.
    pub fn new(db: Db, embedder: Arc<dyn Embedder>, extractor: Arc<dyn Extractor>) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            embedder,
            extractor,
            retrieval_config: RetrievalConfig::default(),
            tool_router: Self::tool_router(),
        }
    }

    /// Set the retrieval scoring knobs (builder style).
    pub fn with_retrieval_config(mut self, config: RetrievalConfig) -> Self {
        self.retrieval_config = config;
        self
    }

    /// Create a server sharing an existing database handle (tests use this
    /// to swap providers mid-scenario).
    pub fn from_shared(
        db: Arc<Mutex<Db>>,
        embedder: Arc<dyn Embedder>,
        extractor: Arc<dyn Extractor>,
    ) -> Self {
        Self {
            db,
            embedder,
            extractor,
            retrieval_config: RetrievalConfig::default(),
            tool_router: Self::tool_router(),
        }
    }

    /// Shared handle to the database (used by tests and diagnostics).
    pub fn db_handle(&self) -> Arc<Mutex<Db>> {
        self.db.clone()
    }
}

#[tool_router]
impl MemoryServer {
    /// Load a project's living state document and memory counts.
    #[tool(
        name = "get_project_state",
        description = "Load a project's living state document (STATE.md), aliases and memory counts. Call this at the start of every session. Accepts the project id, name, or any alias, case-insensitively."
    )]
    pub async fn get_project_state(
        &self,
        params: Parameters<GetProjectStateRequest>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let project = params.0.project;
        tracing::info!(%project, "get_project_state");
        let db = self.db.lock().await;
        let conn = db.conn();

        let resolved = match queries::resolve_project(conn, &project) {
            Ok(r) => r,
            Err(e) => return map_err(e),
        };
        let Some(p) = resolved else {
            let existing = queries::list_project_ids(conn).unwrap_or_default();
            let listing = if existing.is_empty() {
                "No projects exist yet".to_string()
            } else {
                format!("Existing projects: {}", existing.join(", "))
            };
            return Ok(tool_error(format!(
                "project '{project}' not found. {listing}. \
                 Call save_memory with this project name to create it, or retry with one of the existing ids."
            )));
        };

        let active = match queries::active_memory_count(conn, Some(&p.id)) {
            Ok(n) => n,
            Err(e) => return map_err(e),
        };
        let global = match queries::active_memory_count(conn, None) {
            Ok(n) => n,
            Err(e) => return map_err(e),
        };

        Ok(CallToolResult::structured(serde_json::json!({
            "project_id": p.id,
            "name": p.name,
            "aliases": p.aliases,
            "state_md": p.state_md,
            "state_updated_at": p.state_updated_at,
            "active_memory_count": active,
            "global_memory_count": global,
        })))
    }

    /// Persist one memory (embedding + row in one transaction), optionally
    /// superseding an older memory.
    #[tool(
        name = "save_memory",
        description = "Save a durable memory. Provide `project` to scope it (auto-created if unknown) or omit it for a global memory. Set `supersedes` to the id of an outdated memory this fact replaces; the old memory is kept in history, never deleted."
    )]
    pub async fn save_memory(
        &self,
        params: Parameters<SaveMemoryRequest>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let req = params.0;
        tracing::info!(project = ?req.project, supersedes = ?req.supersedes, "save_memory");

        let content = req.content.trim().to_string();
        if content.is_empty() {
            return Ok(tool_error(
                "content is empty. Pass the fact to remember as a self-contained sentence.",
            ));
        }
        let kind = req.kind.unwrap_or_else(|| "fact".to_string());
        if !MEMORY_KINDS.contains(&kind.as_str()) {
            return Ok(tool_error(format!(
                "unknown kind '{kind}'. Use one of: {}.",
                MEMORY_KINDS.join(", ")
            )));
        }
        let importance = req.importance.unwrap_or(3);
        if !(1..=5).contains(&importance) {
            return Ok(tool_error(
                "importance must be between 1 (trivia) and 5 (critical).",
            ));
        }

        // Embed BEFORE opening the transaction; a provider failure must not
        // leave partial state.
        let embedding = match self.embedder.embed(std::slice::from_ref(&content)).await {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            Ok(_) => return Ok(tool_error("embedder returned no vector; retry.")),
            Err(e) => return map_err(e),
        };

        let mut db = self.db.lock().await;
        if embedding.len() != db.dims() {
            return Err(ErrorData::internal_error(
                format!(
                    "embedder produced {} dims but the database expects {}",
                    embedding.len(),
                    db.dims()
                ),
                None,
            ));
        }

        let project_id = match &req.project {
            Some(name) => match queries::ensure_project(db.conn(), name) {
                Ok(p) => Some(p.id),
                Err(e) => return map_err(e),
            },
            None => None,
        };

        let new = NewMemory {
            project_id: project_id.clone(),
            content,
            kind,
            importance,
            source_session_id: None,
        };
        match queries::insert_memory(db.conn_mut(), &new, &embedding, req.supersedes, None) {
            Ok(memory_id) => Ok(CallToolResult::structured(serde_json::json!({
                "memory_id": memory_id,
                "project_id": project_id,
                "kind": new.kind,
                "importance": new.importance,
                "superseded": req.supersedes,
            }))),
            Err(e) => map_err(e),
        }
    }

    /// Hybrid semantic + lexical search over memories.
    #[tool(
        name = "search_memory",
        description = "Search memories with hybrid retrieval (semantic vectors + exact terms, rank-fused). Scope with `project` (global memories are always included), filter by `kinds`, and pass `as_of` to see what was believed true at a past instant. Returns matches ranked by relevance."
    )]
    pub async fn search_memory(
        &self,
        params: Parameters<SearchMemoryRequest>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let req = params.0;
        tracing::info!(project = ?req.project, as_of = ?req.as_of, "search_memory");
        let query = req.query.trim().to_string();
        if query.is_empty() {
            return Ok(tool_error("query is empty. Describe what you are looking for."));
        }
        if let Some(kinds) = &req.kinds {
            if let Some(bad) = kinds.iter().find(|k| !MEMORY_KINDS.contains(&k.as_str())) {
                return Ok(tool_error(format!(
                    "unknown kind '{bad}'. Use any of: {}.",
                    MEMORY_KINDS.join(", ")
                )));
            }
        }
        // Malformed as_of is a protocol-level invalid_params per contract.
        let as_of = match &req.as_of {
            Some(raw) => Some(
                retrieval::normalize_as_of(raw)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
            ),
            None => None,
        };

        let embedding = match self.embedder.embed(std::slice::from_ref(&query)).await {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            Ok(_) => return Ok(tool_error("embedder returned no vector; retry.")),
            Err(e) => return map_err(e),
        };

        let db = self.db.lock().await;
        let conn = db.conn();

        // Resolve project scoping; unknown project -> same guidance as
        // get_project_state.
        let project_id = match &req.project {
            Some(name) => match queries::resolve_project(conn, name) {
                Ok(Some(p)) => Some(p.id),
                Ok(None) => {
                    let existing = queries::list_project_ids(conn).unwrap_or_default();
                    return Ok(tool_error(format!(
                        "project '{name}' not found. Existing projects: {}. \
                         Retry with one of these, or omit `project` to search everywhere.",
                        if existing.is_empty() { "(none)".to_string() } else { existing.join(", ") }
                    )));
                }
                Err(e) => return map_err(e),
            },
            None => None,
        };

        let is_historical = as_of.is_some();
        let search_params = SearchParams {
            project_id,
            kinds: req.kinds.clone(),
            limit: req.limit.unwrap_or(8).clamp(1, 50),
            as_of,
            recency: (!is_historical).then_some(RecencyBoost {
                weight: self.retrieval_config.recency_weight,
                half_life_days: self.retrieval_config.recency_half_life_days,
            }),
        };
        match retrieval::search(conn, &query, &embedding, &search_params) {
            Ok(hits) => {
                // Usage tracking: current-time hits only; historical
                // queries must not disturb the usage signal.
                if !is_historical {
                    let ids: Vec<i64> = hits.iter().map(|h| h.memory.id).collect();
                    if let Err(e) = queries::bump_retrieval(conn, &ids) {
                        tracing::warn!(error = %e, "usage bump failed (results unaffected)");
                    }
                }
                let results: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|h| serde_json::json!({
                        "memory_id": h.memory.id,
                        "project_id": h.memory.project_id,
                        "content": h.memory.content,
                        "kind": h.memory.kind,
                        "importance": h.memory.importance,
                        "created_at": h.memory.created_at,
                        "invalid_at": h.memory.invalid_at,
                        "superseded_by": h.memory.superseded_by,
                        "score": h.score,
                    }))
                    .collect();
                Ok(CallToolResult::structured(serde_json::json!({
                    "results": results,
                    "count": results.len(),
                })))
            }
            Err(e) => map_err(e),
        }
    }

    /// Persist the session and distill it into durable memories + STATE.md.
    #[tool(
        name = "end_session",
        description = "Close a session: saves the transcript, extracts durable facts (deduplicated and conflict-resolved against existing memories), and rewrites the project's state document. Call this once when a session wraps up."
    )]
    pub async fn end_session(
        &self,
        params: Parameters<EndSessionRequest>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let req = params.0;
        tracing::info!(
            project = ?req.project,
            transcript_len = req.transcript.len(),
            "end_session"
        );
        if req.transcript.trim().is_empty() {
            return Ok(tool_error(
                "transcript is empty. Pass the session conversation (or a faithful summary of it).",
            ));
        }

        let mut db = self.db.lock().await;

        let project_id = match &req.project {
            Some(name) => match queries::ensure_project(db.conn(), name) {
                Ok(p) => Some(p.id),
                Err(e) => return map_err(e),
            },
            None => None,
        };

        // Shared ingest path: persists the transcript first and dedups by
        // content hash, so a hook or retry re-sending the same transcript
        // is a no-op.
        match ingest::ingest_transcript(
            &mut db,
            self.embedder.as_ref(),
            self.extractor.as_ref(),
            project_id.as_deref(),
            req.client.as_deref(),
            &req.transcript,
        )
        .await
        {
            Ok(IngestOutcome::Completed(result)) => Ok(CallToolResult::structured(
                serde_json::to_value(&result).unwrap_or_default(),
            )),
            Ok(IngestOutcome::AlreadyIngested { session_id }) => {
                Ok(CallToolResult::structured(serde_json::json!({
                    "session_id": session_id,
                    "already_ingested": true,
                    "memories_created": 0,
                })))
            }
            Ok(IngestOutcome::ExtractionFailed { session_id, error }) => Ok(tool_error(format!(
                "session transcript was saved as session {session_id}, but fact extraction \
                 failed: {error}. Nothing was lost — call end_session again later with the \
                 same transcript to retry extraction."
            ))),
            Err(e) => map_err(e),
        }
    }

    /// Explore the knowledge graph around one entity.
    #[tool(
        name = "get_related",
        description = "Explore the knowledge graph around an entity: returns the entities related to it (1 or 2 hops), the relations connecting them, and the memories that evidence each relation. Only currently-valid knowledge is returned (superseded facts drop out automatically)."
    )]
    pub async fn get_related(
        &self,
        params: Parameters<GetRelatedRequest>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let req = params.0;
        tracing::info!(entity = %req.entity, "get_related");
        let hops = req.hops.unwrap_or(1);
        if !(1..=2).contains(&hops) {
            return Ok(tool_error("hops must be 1 or 2."));
        }

        let db = self.db.lock().await;
        let conn = db.conn();

        let project_id = match &req.project {
            Some(name) => match queries::resolve_project(conn, name) {
                Ok(Some(p)) => Some(p.id),
                Ok(None) => {
                    return Ok(tool_error(format!(
                        "project '{name}' not found. Omit `project` to search everywhere."
                    )))
                }
                Err(e) => return map_err(e),
            },
            None => None,
        };

        let entity = match queries::resolve_entity(conn, &req.entity, project_id.as_deref()) {
            Ok(Some(e)) => e,
            Ok(None) => {
                return Ok(tool_error(format!(
                    "no entity matches '{}'. Entities are created when sessions are ingested; \
                     try search_memory for free-text lookup instead.",
                    req.entity
                )))
            }
            Err(e) => return map_err(e),
        };

        let (entity_id, name, kind) = entity;
        match queries::entity_neighborhood(conn, entity_id, hops) {
            Ok((nodes, edges)) => Ok(CallToolResult::structured(serde_json::json!({
                "entity": { "id": entity_id, "name": name, "kind": kind },
                "nodes": nodes,
                "edges": edges,
            }))),
            Err(e) => map_err(e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("palimpsest-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
