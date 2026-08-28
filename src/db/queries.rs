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
    /// When this memory last matched a search (usage tracking).
    pub last_retrieved_at: Option<String>,
    /// How many searches this memory has matched (usage tracking).
    pub retrieval_count: i64,
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

/// Entity kinds accepted by the schema.
pub const ENTITY_KINDS: &[&str] = &["technology", "person", "project", "concept", "other"];

/// One entity mentioned by a fact.
#[derive(Debug, Clone)]
pub struct EntityRef {
    /// Display name as extracted.
    pub name: String,
    /// One of [`ENTITY_KINDS`]; anything else is stored as `other`.
    pub kind: String,
}

/// One (source —predicate→ target) triple; names refer to entities.
#[derive(Debug, Clone)]
pub struct TripleRef {
    /// Source entity name.
    pub source: String,
    /// Relation label, e.g. "uses", "deploys-to".
    pub predicate: String,
    /// Target entity name.
    pub target: String,
}

/// Entity/triple payload attached to a memory insert.
#[derive(Debug, Clone, Default)]
pub struct GraphPayload {
    /// Entities the fact mentions.
    pub entities: Vec<EntityRef>,
    /// Relations the fact evidences.
    pub triples: Vec<TripleRef>,
}

/// Normalize an entity name (or free query text) for matching: lowercase,
/// alphanumeric runs joined by single spaces.
pub fn normalize_entity(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = true;
    for c in input.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
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
        last_retrieved_at: row.get("last_retrieved_at")?,
        retrieval_count: row.get("retrieval_count")?,
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
/// When `graph` is present, its entities are upserted and mention/relation
/// edges inserted in the same transaction (evidence = the new memory).
///
/// Returns the new memory id.
pub fn insert_memory(
    conn: &mut Connection,
    new: &NewMemory,
    embedding: &[f32],
    supersedes: Option<i64>,
    graph: Option<&GraphPayload>,
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

    if let Some(graph) = graph {
        attach_graph(&tx, new_id, new.project_id.as_deref(), graph)?;
    }

    tx.commit()?;
    Ok(new_id)
}

/// Upsert entities and insert mention + relation edges for one memory,
/// inside the caller's transaction. Entities named only in triples are
/// created too (kind `other`). Empty/degenerate names are skipped.
fn attach_graph(
    tx: &rusqlite::Transaction<'_>,
    memory_id: i64,
    project_id: Option<&str>,
    graph: &GraphPayload,
) -> Result<()> {
    let upsert = |name: &str, kind: &str| -> Result<Option<i64>> {
        let norm = normalize_entity(name);
        if norm.is_empty() {
            return Ok(None);
        }
        let kind = if ENTITY_KINDS.contains(&kind) { kind } else { "other" };
        tx.execute(
            "INSERT INTO entities (name, norm_name, kind, project_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(norm_name, ifnull(project_id, '')) DO NOTHING",
            params![name.trim(), norm, kind, project_id],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM entities
             WHERE norm_name = ?1 AND ifnull(project_id,'') = ifnull(?2,'')",
            params![norm, project_id],
            |r| r.get(0),
        )?;
        Ok(Some(id))
    };

    for e in &graph.entities {
        if let Some(entity_id) = upsert(&e.name, &e.kind)? {
            tx.execute(
                "INSERT OR IGNORE INTO memory_entities (memory_id, entity_id) VALUES (?1, ?2)",
                params![memory_id, entity_id],
            )?;
        }
    }
    for t in &graph.triples {
        let (Some(src), Some(dst)) = (upsert(&t.source, "other")?, upsert(&t.target, "other")?)
        else {
            continue;
        };
        let predicate = t.predicate.trim();
        if predicate.is_empty() || src == dst {
            continue;
        }
        // Triple endpoints count as mentions of this memory too.
        for id in [src, dst] {
            tx.execute(
                "INSERT OR IGNORE INTO memory_entities (memory_id, entity_id) VALUES (?1, ?2)",
                params![memory_id, id],
            )?;
        }
        tx.execute(
            "INSERT INTO relations (source_entity_id, predicate, target_entity_id, evidence_memory_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![src, predicate, dst, memory_id],
        )?;
    }
    Ok(())
}

/// A node in a `get_related` neighborhood.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphNode {
    /// Entity id.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// Entity kind.
    pub kind: String,
    /// Hop distance from the queried entity (0 = the entity itself).
    pub hops: i64,
}

/// An edge in a `get_related` neighborhood.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphEdge {
    /// Source entity id.
    pub source: i64,
    /// Relation label.
    pub predicate: String,
    /// Target entity id.
    pub target: i64,
    /// Memory evidencing this relation (currently valid).
    pub evidence_memory_id: i64,
}

/// Resolve an entity by normalized name within a scope (project OR global).
pub fn resolve_entity(
    conn: &Connection,
    name: &str,
    project_id: Option<&str>,
) -> Result<Option<(i64, String, String)>> {
    let norm = normalize_entity(name);
    conn.query_row(
        "SELECT id, name, kind FROM entities
         WHERE norm_name = ?1 AND (project_id IS NULL OR project_id = ifnull(?2, project_id))
         ORDER BY (project_id IS NULL) LIMIT 1",
        params![norm, project_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
    .map_err(Error::from)
}

/// Breadth-first neighborhood of an entity over currently-valid relations
/// (edges whose evidence memory has not been superseded). Returns nodes
/// with hop distance and the edges connecting them.
pub fn entity_neighborhood(
    conn: &Connection,
    entity_id: i64,
    hops: i64,
) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
    let mut stmt = conn.prepare(
        "SELECT r.source_entity_id, r.predicate, r.target_entity_id, r.evidence_memory_id
         FROM relations r
         JOIN memories m ON m.id = r.evidence_memory_id
         WHERE (r.source_entity_id = ?1 OR r.target_entity_id = ?1)
           AND m.invalid_at IS NULL",
    )?;

    let mut frontier = vec![entity_id];
    let mut seen_nodes = std::collections::HashMap::from([(entity_id, 0i64)]);
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut seen_edges = std::collections::HashSet::new();

    for depth in 1..=hops.max(1) {
        let mut next = Vec::new();
        for node in frontier.drain(..) {
            let rows = stmt.query_map([node], |r| {
                Ok(GraphEdge {
                    source: r.get(0)?,
                    predicate: r.get(1)?,
                    target: r.get(2)?,
                    evidence_memory_id: r.get(3)?,
                })
            })?;
            for edge in rows {
                let edge = edge?;
                if seen_edges.insert((edge.source, edge.predicate.clone(), edge.target)) {
                    for other in [edge.source, edge.target] {
                        if let std::collections::hash_map::Entry::Vacant(v) =
                            seen_nodes.entry(other)
                        {
                            v.insert(depth);
                            next.push(other);
                        }
                    }
                    edges.push(edge);
                }
            }
        }
        frontier = next;
    }

    let mut nodes = Vec::with_capacity(seen_nodes.len());
    for (id, hop) in &seen_nodes {
        let (name, kind): (String, String) = conn.query_row(
            "SELECT name, kind FROM entities WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        nodes.push(GraphNode {
            id: *id,
            name,
            kind,
            hops: *hop,
        });
    }
    nodes.sort_by_key(|n| (n.hops, n.id));
    Ok((nodes, edges))
}

/// Record that these memories matched a current-time search. This is the
/// ONLY sanctioned UPDATE on memories besides supersession stamping, and it
/// touches exclusively the usage-tracking columns — never content/validity.
pub fn bump_retrieval(conn: &Connection, memory_ids: &[i64]) -> Result<()> {
    if memory_ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; memory_ids.len()].join(", ");
    let sql = format!(
        "UPDATE memories
         SET last_retrieved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
             retrieval_count = retrieval_count + 1
         WHERE id IN ({placeholders})"
    );
    let params: Vec<&dyn rusqlite::types::ToSql> =
        memory_ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    conn.execute(&sql, params.as_slice())?;
    Ok(())
}

/// Persist a session transcript (with its dedup hash). Returns the session id.
pub fn insert_session(
    conn: &Connection,
    project_id: Option<&str>,
    client: Option<&str>,
    transcript: &str,
    transcript_hash: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO sessions (project_id, client, transcript, transcript_hash)
         VALUES (?1, ?2, ?3, ?4)",
        params![project_id, client, transcript, transcript_hash],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Find a previously ingested session by transcript hash (idempotency guard).
pub fn find_session_by_hash(conn: &Connection, transcript_hash: &str) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM sessions WHERE transcript_hash = ?1 LIMIT 1",
        [transcript_hash],
        |r| r.get(0),
    )
    .optional()
    .map_err(Error::from)
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

/// A node in a full-graph export, with the validity windows of the
/// memories that mention it (drives the time slider in the HTML view).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportNode {
    /// Entity id.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// Entity kind.
    pub kind: String,
    /// Owning project (`None` = global entity).
    pub project_id: Option<String>,
    /// `(valid_at, invalid_at)` windows of mentioning memories.
    pub windows: Vec<(String, Option<String>)>,
}

/// An edge in a full-graph export, carrying its evidence window.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportEdge {
    /// Source entity id.
    pub source: i64,
    /// Relation label.
    pub predicate: String,
    /// Target entity id.
    pub target: i64,
    /// Evidence memory id.
    pub evidence_memory_id: i64,
    /// Evidence memory content (truncated for tooltips).
    pub evidence: String,
    /// When the evidence became true.
    pub valid_at: String,
    /// When it stopped being current (`None` = still valid).
    pub invalid_at: Option<String>,
}

/// Whole-graph export for visualization: every entity and relation in
/// scope (chosen project OR global; `None` = everything), including
/// superseded evidence — validity windows let viewers time-travel.
pub fn graph_export(
    conn: &Connection,
    project_id: Option<&str>,
) -> Result<(Vec<ExportNode>, Vec<ExportEdge>)> {
    let scope = match project_id {
        Some(_) => "AND (e.project_id = :pid OR e.project_id IS NULL)",
        None => "",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT e.id, e.name, e.kind, e.project_id FROM entities e WHERE 1=1 {scope} ORDER BY e.id"
    ))?;
    let bind: Vec<(&str, &dyn rusqlite::types::ToSql)> = match &project_id {
        Some(pid) => vec![(":pid", pid as &dyn rusqlite::types::ToSql)],
        None => vec![],
    };
    let mut nodes = stmt
        .query_map(bind.as_slice(), |r| {
            Ok(ExportNode {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                project_id: r.get(3)?,
                windows: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut win_stmt = conn.prepare(
        "SELECT m.valid_at, m.invalid_at FROM memory_entities me
         JOIN memories m ON m.id = me.memory_id
         WHERE me.entity_id = ?1 ORDER BY m.valid_at",
    )?;
    for node in &mut nodes {
        node.windows = win_stmt
            .query_map([node.id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }

    let node_ids: std::collections::HashSet<i64> = nodes.iter().map(|n| n.id).collect();
    let mut edge_stmt = conn.prepare(
        "SELECT r.source_entity_id, r.predicate, r.target_entity_id,
                r.evidence_memory_id, m.content, m.valid_at, m.invalid_at
         FROM relations r JOIN memories m ON m.id = r.evidence_memory_id
         ORDER BY r.id",
    )?;
    let edges = edge_stmt
        .query_map([], |r| {
            let content: String = r.get(4)?;
            Ok(ExportEdge {
                source: r.get(0)?,
                predicate: r.get(1)?,
                target: r.get(2)?,
                evidence_memory_id: r.get(3)?,
                evidence: content.chars().take(140).collect(),
                valid_at: r.get(5)?,
                invalid_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|e| node_ids.contains(&e.source) && node_ids.contains(&e.target))
        .collect();

    Ok((nodes, edges))
}
