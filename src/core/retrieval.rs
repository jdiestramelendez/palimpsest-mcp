//! Hybrid retrieval: sqlite-vec KNN + FTS5 BM25, fused with Reciprocal
//! Rank Fusion (k = 60), importance as tie-break.
//!
//! Every search filters by validity (current-time by default, or an
//! `as_of` point in history) and always includes global memories
//! (`project_id IS NULL`) alongside the chosen project's.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::types::Value;
use rusqlite::Connection;

use crate::db::queries::{embedding_to_blob, normalize_entity, Memory};
use crate::error::{Error, Result};

/// RRF constant: score contribution is 1 / (K + rank).
const RRF_K: f64 = 60.0;

/// Candidates taken from each signal before fusion.
const PER_SIGNAL_TOP: usize = 20;

/// KNN over-fetch: the vector index cannot pre-filter by validity/scope,
/// so we pull extra neighbours and rank the survivors.
const KNN_OVERFETCH: usize = 200;

/// Recency boost parameters (from `[retrieval]` config). `None` disables
/// the boost — always the case for `as_of` searches, which rank by
/// historical relevance only.
#[derive(Debug, Clone, Copy)]
pub struct RecencyBoost {
    /// Boost weight; multiplier is `1 + weight * exp(-age/half_life)`.
    pub weight: f64,
    /// Half-life in days.
    pub half_life_days: f64,
}

/// Search parameters (already validated by the tool layer).
#[derive(Debug, Clone)]
pub struct SearchParams {
    /// Scope to this project id (global memories are always included).
    /// `None` searches across all projects.
    pub project_id: Option<String>,
    /// Restrict to these kinds; `None` = all kinds.
    pub kinds: Option<Vec<String>>,
    /// Maximum results to return.
    pub limit: usize,
    /// Normalized ISO 8601 UTC instant for time-travel; `None` = now.
    pub as_of: Option<String>,
    /// Recency boost; ignored (treat as disabled) when `as_of` is set.
    pub recency: Option<RecencyBoost>,
}

/// One search hit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoredMemory {
    /// The memory row.
    #[serde(flatten)]
    pub memory: Memory,
    /// RRF fusion score (higher is better).
    pub score: f64,
}

/// Parse a user-supplied `as_of` into the storage timestamp format
/// (`%Y-%m-%dT%H:%M:%S.%3fZ`, UTC). Accepts RFC 3339 with offset,
/// a naive datetime (treated as UTC), or a bare date (midnight UTC).
pub fn normalize_as_of(input: &str) -> Result<String> {
    const STORE_FMT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc).format(STORE_FMT).to_string());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(input, fmt) {
            return Ok(naive.and_utc().format(STORE_FMT).to_string());
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        if let Some(naive) = date.and_hms_opt(0, 0, 0) {
            return Ok(naive.and_utc().format(STORE_FMT).to_string());
        }
    }
    Err(Error::InvalidInput(format!(
        "as_of '{input}' is not a valid ISO 8601 timestamp; use e.g. 2026-08-27T12:00:00Z or 2026-08-27"
    )))
}

/// Quote user text as literal FTS5 terms: each whitespace-separated token
/// becomes a quoted phrase (internal quotes doubled), joined with OR so any
/// overlapping term contributes to BM25 ranking. Returns `None` when no
/// usable term remains (pure-punctuation or empty queries).
pub fn fts_query_from(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

/// Run the hybrid search. `query_embedding` is the embedded query text;
/// `query_text` feeds the lexical leg (skipped when it has no usable terms,
/// leaving RRF over the vector list alone).
pub fn search(
    conn: &Connection,
    query_text: &str,
    query_embedding: &[f32],
    params: &SearchParams,
) -> Result<Vec<ScoredMemory>> {
    // Validity predicate: current time, or the historical as_of instant.
    // String comparison is safe: all timestamps share the same UTC format.
    let validity = match params.as_of {
        None => "m.invalid_at IS NULL".to_string(),
        Some(_) => "m.valid_at <= :as_of AND (m.invalid_at IS NULL OR m.invalid_at > :as_of)"
            .to_string(),
    };

    // Project scope: the chosen project OR global; no project = everything.
    let scope = match params.project_id {
        Some(_) => "(m.project_id = :project_id OR m.project_id IS NULL)",
        None => "1=1",
    };

    // Kind filter, inlined as quoted literals (values are validated against
    // MEMORY_KINDS by the tool layer, but quote defensively anyway).
    let kind_filter = match &params.kinds {
        Some(kinds) if !kinds.is_empty() => {
            let quoted: Vec<String> = kinds
                .iter()
                .map(|k| format!("'{}'", k.replace('\'', "''")))
                .collect();
            format!("AND m.kind IN ({})", quoted.join(", "))
        }
        _ => String::new(),
    };

    let fts_query = fts_query_from(query_text);

    // Entity leg: entities whose normalized name appears inside the
    // normalized query text (substring match handles multi-word names,
    // LLM-free on the search path). Length guard avoids 1-2 char noise.
    let norm_query = normalize_entity(query_text);
    let use_entities = norm_query.len() >= 3;

    // The two ranked candidate lists. The vector leg over-fetches because
    // vec0 KNN cannot pre-filter by validity/scope; survivors are re-ranked.
    let vec_cte = format!(
        "vec_ranked AS (
            SELECT v.memory_id, ROW_NUMBER() OVER (ORDER BY v.distance) AS rnk
            FROM (SELECT memory_id, distance FROM memories_vec
                  WHERE embedding MATCH :embedding AND k = :knn) v
            JOIN memories m ON m.id = v.memory_id
            WHERE {validity} AND {scope} {kind_filter}
            ORDER BY rnk LIMIT {PER_SIGNAL_TOP}
        )"
    );
    // bm25() cannot appear inside a window function, so the FTS scan
    // computes it in an inner subquery and ROW_NUMBER ranks over the alias.
    let fts_cte = format!(
        "fts_ranked AS (
            SELECT f.memory_id, ROW_NUMBER() OVER (ORDER BY f.bscore) AS rnk
            FROM (SELECT rowid AS memory_id, bm25(memories_fts) AS bscore
                  FROM memories_fts WHERE memories_fts MATCH :fts) f
            JOIN memories m ON m.id = f.memory_id
            WHERE {validity} AND {scope} {kind_filter}
            ORDER BY rnk LIMIT {PER_SIGNAL_TOP}
        )"
    );

    // Entity candidates ranked by how many query entities each memory
    // shares (aggregate computed in an inner subquery; ROW_NUMBER ranks
    // over the alias, same pattern as the bm25 leg).
    let ent_cte = format!(
        "ent_ranked AS (
            SELECT e.memory_id, ROW_NUMBER() OVER (ORDER BY e.n DESC, e.memory_id DESC) AS rnk
            FROM (SELECT me.memory_id, COUNT(*) AS n
                  FROM entities en
                  JOIN memory_entities me ON me.entity_id = en.id
                  WHERE length(en.norm_name) >= 3
                    AND instr(' ' || :norm_query || ' ', ' ' || en.norm_name || ' ') > 0
                  GROUP BY me.memory_id) e
            JOIN memories m ON m.id = e.memory_id
            WHERE {validity} AND {scope} {kind_filter}
            ORDER BY rnk LIMIT {PER_SIGNAL_TOP}
        )"
    );

    // RRF fusion over the active legs (vector always; lexical and entity
    // when usable). Absent legs simply drop out of the union and sum.
    let mut ctes = vec![vec_cte];
    let mut unions = vec!["SELECT memory_id FROM vec_ranked".to_string()];
    let mut joins = vec!["LEFT JOIN vec_ranked v ON v.memory_id = c.memory_id".to_string()];
    let mut score_terms = vec![format!("COALESCE(1.0 / ({RRF_K} + v.rnk), 0)")];
    if fts_query.is_some() {
        ctes.push(fts_cte);
        unions.push("SELECT memory_id FROM fts_ranked".to_string());
        joins.push("LEFT JOIN fts_ranked f ON f.memory_id = c.memory_id".to_string());
        score_terms.push(format!("COALESCE(1.0 / ({RRF_K} + f.rnk), 0)"));
    }
    if use_entities {
        ctes.push(ent_cte);
        unions.push("SELECT memory_id FROM ent_ranked".to_string());
        joins.push("LEFT JOIN ent_ranked en ON en.memory_id = c.memory_id".to_string());
        score_terms.push(format!("COALESCE(1.0 / ({RRF_K} + en.rnk), 0)"));
    }
    let sql = format!(
        "WITH {ctes}
         SELECT m.*, {score} AS score
         FROM ({unions}) c
         JOIN memories m ON m.id = c.memory_id
         {joins}
         ORDER BY score DESC, m.importance DESC, m.id DESC",
        ctes = ctes.join(", "),
        score = score_terms.join(" + "),
        unions = unions.join(" UNION "),
        joins = joins.join("
         "),
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut named: Vec<(&str, Value)> = vec![
        (":embedding", Value::Blob(embedding_to_blob(query_embedding))),
        (":knn", Value::Integer(KNN_OVERFETCH as i64)),
    ];
    if let Some(fts) = &fts_query {
        named.push((":fts", Value::Text(fts.clone())));
    }
    if use_entities {
        named.push((":norm_query", Value::Text(norm_query.clone())));
    }
    if let Some(pid) = &params.project_id {
        named.push((":project_id", Value::Text(pid.clone())));
    }
    if let Some(as_of) = &params.as_of {
        named.push((":as_of", Value::Text(as_of.clone())));
    }
    let named_refs: Vec<(&str, &dyn rusqlite::types::ToSql)> = named
        .iter()
        .map(|(k, v)| (*k, v as &dyn rusqlite::types::ToSql))
        .collect();

    let rows = stmt.query_map(named_refs.as_slice(), |row| {
        Ok(ScoredMemory {
            memory: Memory {
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
            },
            score: row.get("score")?,
        })
    })?;
    let mut hits = rows.collect::<rusqlite::Result<Vec<_>>>()?;

    // Recency boost (current-time searches only): multiply the RRF score
    // by 1 + w * exp(-age_days / half_life), computed here rather than in
    // SQL so no math-function build flags are required. The fused pool is
    // small (<= 3 legs x top-20), so re-ranking in memory is free.
    if params.as_of.is_none() {
        if let Some(recency) = params.recency.filter(|r| r.weight > 0.0) {
            let now = chrono::Utc::now();
            for hit in &mut hits {
                let age_days = DateTime::parse_from_rfc3339(&hit.memory.valid_at)
                    .map(|t| (now - t.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0)
                    .unwrap_or(f64::MAX)
                    .max(0.0);
                let boost = 1.0
                    + recency.weight * (-age_days / recency.half_life_days.max(0.001)).exp();
                hit.score *= boost;
            }
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.memory.importance.cmp(&a.memory.importance))
                    .then(b.memory.id.cmp(&a.memory.id))
            });
        }
    }
    hits.truncate(params.limit);
    Ok(hits)
}
