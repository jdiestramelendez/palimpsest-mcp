//! Shared transcript ingestion: the one entry point used by both the
//! `end_session` MCP tool and the CLI (`ingest` / `hook session-end`).
//!
//! Guarantees: the transcript is persisted before extraction runs, and the
//! same transcript is never ingested twice (sha256 dedup) — a lifecycle
//! hook firing twice is a no-op.

use sha2::{Digest, Sha256};

use crate::core::extraction::{self, EndSessionResult};
use crate::db::{queries, Db};
use crate::error::Result;
use crate::providers::{Embedder, Extractor};

/// Outcome of an ingestion attempt.
#[derive(Debug)]
pub enum IngestOutcome {
    /// This exact transcript was ingested before; nothing was done.
    AlreadyIngested {
        /// The session that already holds this transcript.
        session_id: i64,
    },
    /// Pipeline ran to completion.
    Completed(EndSessionResult),
    /// Transcript was persisted but extraction failed; retry is safe
    /// (retry re-runs extraction only after the duplicate row is ignored).
    ExtractionFailed {
        /// The persisted session row.
        session_id: i64,
        /// Why extraction failed.
        error: crate::error::Error,
    },
}

/// Hex sha256 of a transcript, the dedup key.
pub fn transcript_hash(transcript: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(transcript.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Persist and extract one session transcript. `project_id` must already
/// be resolved/created by the caller (or `None` for a global session).
pub async fn ingest_transcript(
    db: &mut Db,
    embedder: &dyn Embedder,
    extractor: &dyn Extractor,
    project_id: Option<&str>,
    client: Option<&str>,
    transcript: &str,
) -> Result<IngestOutcome> {
    let hash = transcript_hash(transcript);
    if let Some(session_id) = queries::find_session_by_hash(db.conn(), &hash)? {
        tracing::info!(session_id, "transcript already ingested; skipping");
        return Ok(IngestOutcome::AlreadyIngested { session_id });
    }

    let session_id = queries::insert_session(db.conn(), project_id, client, transcript, &hash)?;

    match extraction::run_pipeline(db, embedder, extractor, session_id, project_id, transcript)
        .await
    {
        Ok(result) => Ok(IngestOutcome::Completed(result)),
        Err(error) => Ok(IngestOutcome::ExtractionFailed { session_id, error }),
    }
}

/// Extract a plain-text transcript from a Claude Code session JSONL file.
///
/// Handles both documented line shapes: flat `{"role": "...", "content":
/// ...}` and the nested `{"type": "user", "message": {"content": ...}}`
/// form; `content` may be a string or an array of typed blocks. Tool
/// calls/results and metadata lines are skipped. Output lines look like
/// `[user] …` / `[assistant] …`.
pub fn transcript_from_claude_jsonl(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let role_field = v
            .get("role")
            .or_else(|| v.get("type"))
            .and_then(|t| t.as_str());
        let role = match role_field {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        let Some(content) = v
            .get("content")
            .or_else(|| v.get("message").and_then(|m| m.get("content")))
        else {
            continue;
        };
        let text = match content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(blocks) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        out.push_str(&format!("[{role}] {text}\n"));
    }
    out
}

/// Cap a transcript to its final `max_bytes` (the wrap-up carries the most
/// durable signal), cutting on a line boundary.
pub fn tail_transcript(transcript: &str, max_bytes: usize) -> &str {
    if transcript.len() <= max_bytes {
        return transcript;
    }
    let mut start = transcript.len() - max_bytes;
    while start < transcript.len() && !transcript.is_char_boundary(start) {
        start += 1;
    }
    match transcript[start..].find('\n') {
        Some(nl) => &transcript[start + nl + 1..],
        None => &transcript[start..],
    }
}

/// Derive a project name from a working directory: the git repository
/// root's directory name when inside a repo, else the directory's own name.
pub fn project_from_cwd(cwd: &std::path::Path) -> Option<String> {
    let mut dir = cwd;
    loop {
        if dir.join(".git").exists() {
            return dir.file_name().map(|n| n.to_string_lossy().into_owned());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    cwd.file_name().map(|n| n.to_string_lossy().into_owned())
}
