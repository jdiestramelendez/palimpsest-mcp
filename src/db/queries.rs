//! Typed query functions. Tool handlers never contain raw SQL; it all
//! lives here (and in `core::retrieval` for the ranked search).
//!
//! Append-only invariant: nothing in this module (or anywhere else) deletes
//! a memory row or updates its content. Supersession stamps `invalid_at` +
//! `superseded_by` on the old row inside the same transaction that inserts
//! the replacement.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A project row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// Slug identifier, e.g. `palimpsest-mcp`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Alternative names that resolve to this project.
    pub aliases: Vec<String>,
    /// The living STATE.md document.
    pub state_md: String,
    /// When STATE.md was last rewritten (ISO 8601), if ever.
    pub state_updated_at: Option<String>,
    /// Creation timestamp (ISO 8601).
    pub created_at: String,
}

/// A memory row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Row id.
    pub id: i64,
    /// Owning project; `None` = global memory.
    pub project_id: Option<String>,
    /// The remembered fact.
    pub content: String,
    /// One of `fact`, `decision`, `preference`, `todo`, `insight`.
    pub kind: String,
    /// 1 (trivia) .. 5 (critical).
    pub importance: i64,
    /// Session that produced this memory, if any.
    pub source_session_id: Option<i64>,
    /// Creation timestamp (ISO 8601).
    pub created_at: String,
    /// When the fact became true (ISO 8601).
    pub valid_at: String,
    /// When the fact stopped being current; `None` = still valid.
    pub invalid_at: Option<String>,
    /// The memory that replaced this one, if superseded.
    pub superseded_by: Option<i64>,
}

/// Fields for inserting a new memory.
#[derive(Debug, Clone)]
pub struct NewMemory {
    /// Owning project (`None` = global). Must already exist.
    pub project_id: Option<String>,
    /// The remembered fact.
    pub content: String,
    /// One of `fact`, `decision`, `preference`, `todo`, `insight`.
    pub kind: String,
    /// 1..=5.
    pub importance: i64,
    /// Session that produced this memory, if any.
    pub source_session_id: Option<i64>,
}

/// Memory kinds accepted by the schema.
pub const MEMORY_KINDS: &[&str] = &["fact", "decision", "preference", "todo", "insight"];

fn row_to_project(row: &Row<'_>) -> rusqlite::Result<Project> {
    let aliases_json: String = row.get("aliases")?;
    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
        state_md: row.get("state_md")?,
        state_updated_at: row.get("state_updated_at")?,
        created_at: row.get("created_at")?,
    })
}

fn row_to_memory(row: &Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        content: row.get("content")?,
        kind: row.get("kind")?,
        importance: row.get("importance")?,
        source_session_id: row.get("source_session_id")?,
        created_at: row.get("created_at")?,
        valid_at: row.get("valid_at")?,
        invalid_at: row.get("invalid_at")?,
        superseded_by: row.get("superseded_by")?,
    })
}

/// Serialize an embedding as the little-endian float32 blob sqlite-vec expects.
pub fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Turn free-form project input into a slug id: lowercase, `[a-z0-9]`
/// runs joined by single dashes.
pub fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut prev_dash = true; // suppress leading dash
    for c in input.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Look up a project by exact id.
pub fn get_project(conn: &Connection, id: &str) -> Result<Option<Project>> {
    conn.query_row("SELECT * FROM projects WHERE id = ?1", [id], row_to_project)
        .optional()
        .map_err(Error::from)
}

/// Resolve a project by id, name, or alias — all case-insensitive.
pub fn resolve_project(conn: &Connection, name_or_alias: &str) -> Result<Option<Project>> {
    conn.query_row(
        "SELECT * FROM projects
         WHERE lower(id) = lower(?1)
            OR lower(name) = lower(?1)
            OR EXISTS (SELECT 1 FROM json_each(projects.aliases)
                       WHERE lower(json_each.value) = lower(?1))
         LIMIT 1",
        [name_or_alias],
        row_to_project,
    )
    .optional()
    .map_err(Error::from)
}

/// All project ids, alphabetically (used by not-found error messages).
pub fn list_project_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM projects ORDER BY id")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// Resolve a project or create it on the fly from free-form input
/// (slugged id, name = the input as given).
pub fn ensure_project(conn: &Connection, name_or_alias: &str) -> Result<Project> {
    if let Some(p) = resolve_project(conn, name_or_alias)? {
        return Ok(p);
    }
    let id = slugify(name_or_alias);
    if id.is_empty() {
        return Err(Error::InvalidInput(format!(
            "project name '{name_or_alias}' contains no usable characters"
        )));
    }
    conn.execute(
        "INSERT INTO projects (id, name) VALUES (?1, ?2)",
        params![id, name_or_alias],
    )?;
    get_project(conn, &id)?.ok_or_else(|| Error::InvalidInput("project vanished after insert".into()))
}

/// Count currently-valid memories scoped to `project_id` (`None` = global).
pub fn active_memory_count(conn: &Connection, project_id: Option<&str>) -> Result<i64> {
    let count = match project_id {
        Some(pid) => conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id = ?1 AND invalid_at IS NULL",
            [pid],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id IS NULL AND invalid_at IS NULL",
            [],
            |r| r.get(0),
        )?,
    };
    Ok(count)
}

/// Fetch a memory by id.
pub fn get_memory(conn: &Connection, id: i64) -> Result<Option<Memory>> {
    conn.query_row("SELECT * FROM memories WHERE id = ?1", [id], row_to_memory)
        .optional()
        .map_err(Error::from)
}

/// Insert a memory and its embedding in ONE transaction; when `supersedes`
/// is given, the same transaction stamps the old row's `invalid_at` and
/// `superseded_by`. The superseded row must exist and be currently valid.
///
/// Returns the new memory id.
pub fn insert_memory(
    conn: &mut Connection,
    new: &NewMemory,
    embedding: &[f32],
    supersedes: Option<i64>,
) -> Result<i64> {
    let tx = conn.transaction()?;

    if let Some(old_id) = supersedes {
        let old: Option<(Option<String>,)> = tx
            .query_row(
                "SELECT invalid_at FROM memories WHERE id = ?1",
                [old_id],
                |r| Ok((r.get(0)?,)),
            )
            .optional()?;
        match old {
            None => {
                return Err(Error::InvalidInput(format!(
                    "cannot supersede memory {old_id}: no such memory. \
                     Use search_memory to find the right id."
                )))
            }
            Some((Some(_),)) => {
                return Err(Error::InvalidInput(format!(
                    "cannot supersede memory {old_id}: it was already superseded. \
                     Use search_memory to find the currently valid version."
                )))
            }
            Some((None,)) => {}
        }
    }

    tx.execute(
        "INSERT INTO memories (project_id, content, kind, importance, source_session_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            new.project_id,
            new.content,
            new.kind,
            new.importance,
            new.source_session_id
        ],
    )?;
    let new_id = tx.last_insert_rowid();

    tx.execute(
        "INSERT INTO memories_vec (memory_id, embedding) VALUES (?1, ?2)",
        params![new_id, embedding_to_blob(embedding)],
    )?;

    if let Some(old_id) = supersedes {
        tx.execute(
            "UPDATE memories
             SET invalid_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), superseded_by = ?1
             WHERE id = ?2",
            params![new_id, old_id],
        )?;
    }

    tx.commit()?;
    Ok(new_id)
}

/// Persist a session transcript. Returns the session id.
pub fn insert_session(
    conn: &Connection,
    project_id: Option<&str>,
    client: Option<&str>,
    transcript: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO sessions (project_id, client, transcript) VALUES (?1, ?2, ?3)",
        params![project_id, client, transcript],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Store the session's one-line summary once extraction produced it.
pub fn set_session_summary(conn: &Connection, session_id: i64, summary: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET summary = ?1 WHERE id = ?2",
        params![summary, session_id],
    )?;
    Ok(())
}

/// Overwrite a project's STATE.md and bump `state_updated_at`.
pub fn update_project_state(conn: &Connection, project_id: &str, state_md: &str) -> Result<()> {
    conn.execute(
        "UPDATE projects
         SET state_md = ?1, state_updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?2",
        params![state_md, project_id],
    )?;
    Ok(())
}
