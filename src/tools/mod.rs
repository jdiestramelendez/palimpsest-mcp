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

use crate::db::queries::{self, NewMemory, MEMORY_KINDS};
use crate::db::Db;
use crate::error::Error;
use crate::providers::{Embedder, Extractor};

/// Instructions surfaced to MCP clients at initialize time.
const INSTRUCTIONS: &str = "\
memory-mcp gives you persistent memory across sessions. Call get_project_state \
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
    #[allow(dead_code)] // wired into end_session in Step 5
    extractor: Arc<dyn Extractor>,
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
        match queries::insert_memory(db.conn_mut(), &new, &embedding, req.supersedes) {
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
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("memory-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
