//! End-of-session extraction pipeline:
//! 1. persist the session transcript (never lost, even when extraction fails),
//! 2. extract up to [`MAX_FACTS`] durable facts via the Extractor,
//! 3. resolve each fact against similar existing memories (new / duplicate /
//!    supersedes) and apply the verdict transactionally,
//! 4. rewrite the project's STATE.md.

use serde::Deserialize;

use crate::core::retrieval::{self, SearchParams};
use crate::db::queries::{self, EntityRef, GraphPayload, NewMemory, TripleRef, MEMORY_KINDS};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::providers::{Embedder, Extractor};

/// Hard cap on facts taken from one session.
pub const MAX_FACTS: usize = 15;

/// Candidate memories considered when resolving conflicts for one fact.
const CONFLICT_CANDIDATES: usize = 5;

/// System prompt for fact extraction.
pub const FACT_EXTRACTION_PROMPT: &str = r#"You extract durable memories from a coding-session transcript.

Return ONLY a JSON object, no prose, with this exact shape:
{
  "summary": "one sentence describing what happened this session",
  "facts": [
    {
      "content": "one self-contained factual sentence",
      "kind": "fact | decision | preference | todo | insight",
      "importance": 1-5,
      "scope": "project | global",
      "entities": [
        {"name": "PostgreSQL", "kind": "technology | person | project | concept | other"}
      ],
      "triples": [
        {"source": "acme-shop", "predicate": "uses", "target": "PostgreSQL"}
      ]
    }
  ]
}

Rules:
- Extract only facts worth remembering WEEKS from now: decisions and their
  reasons, project conventions, user preferences, environment quirks,
  unresolved todos. Skip transient debugging noise and anything obvious
  from the code itself.
- Each content must stand alone without the transcript (name the subject,
  never "it" or "this").
- scope is "global" only for facts about the user or their machine that
  hold across every project; otherwise "project".
- At most 15 facts. Fewer, well-chosen facts beat many noisy ones.
- importance 5 = losing this fact breaks future work; 1 = nice to know.
- entities: the concrete things the fact is about (tools, services, people,
  projects, key concepts). 0-4 per fact; omit generic words.
- triples: only relations the fact directly states, with source/target
  drawn from (or added to) that fact's entities. 0-2 per fact."#;

/// System prompt for conflict classification.
pub const CONFLICT_PROMPT: &str = r#"You compare one NEW fact against a numbered list of EXISTING memories.

Return ONLY a JSON object, no prose:
  {"verdict": "new"}                          if the fact adds information no existing memory covers
  {"verdict": "duplicate", "of": <id>}        if an existing memory already states the same thing
  {"verdict": "supersedes", "supersedes": <id>} if the fact REPLACES an existing memory that is now outdated

"supersedes" is only for direct contradiction or replacement (a changed
decision, a corrected value). Related-but-compatible facts are "new"."#;

/// System prompt for the STATE.md rewrite.
pub const STATE_REWRITE_PROMPT: &str = r###"You maintain STATE.md: the single living status document for a software project. An agent reads it at session start to get productive immediately.

You receive the current STATE.md, a one-line summary of the session that
just ended, and the durable facts extracted from it. Rewrite the WHOLE
document, merging the new information in place.

Rules:
- Return ONLY the markdown document. No fences, no commentary.
- Keep this structure: # <project> — State, then sections
  "## Now" (what is actively being worked on),
  "## Decisions" (standing decisions with one-line rationale),
  "## Conventions" (how things are done here),
  "## Open items" (todos, known issues).
- Merge, don't append: replace outdated statements instead of stacking
  history. The memory database keeps history; this document is the present.
- Be dense and concrete. Prefer short bullets over sentences. Keep the
  document under ~80 lines; drop the least important detail first."###;

/// A fact as returned by the extraction model.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedFact {
    /// Self-contained factual sentence.
    pub content: String,
    /// Memory kind; sanitized against [`MEMORY_KINDS`].
    #[serde(default)]
    pub kind: Option<String>,
    /// 1..=5; clamped.
    #[serde(default)]
    pub importance: Option<i64>,
    /// `project` or `global`.
    #[serde(default)]
    pub scope: Option<String>,
    /// Entities this fact mentions.
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    /// Relations this fact states.
    #[serde(default)]
    pub triples: Vec<ExtractedTriple>,
}

/// An entity as returned by the extraction model.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedEntity {
    /// Display name.
    pub name: String,
    /// Entity kind; sanitized against `ENTITY_KINDS` at insert.
    #[serde(default)]
    pub kind: Option<String>,
}

/// A relation triple as returned by the extraction model.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedTriple {
    /// Source entity name.
    pub source: String,
    /// Relation label.
    pub predicate: String,
    /// Target entity name.
    pub target: String,
}

/// Parsed output of the extraction call.
#[derive(Debug, Deserialize)]
pub struct ExtractionOutput {
    /// One-line session summary.
    #[serde(default)]
    pub summary: String,
    /// Extracted facts (capped at [`MAX_FACTS`] by the pipeline).
    #[serde(default)]
    pub facts: Vec<ExtractedFact>,
}

/// Conflict verdict for one fact.
#[derive(Debug, Deserialize)]
struct ConflictVerdict {
    verdict: String,
    #[serde(default, alias = "of")]
    supersedes: Option<i64>,
}

/// Result of the pipeline, mirrored into the tool's response.
#[derive(Debug, Default, serde::Serialize)]
pub struct EndSessionResult {
    /// Persisted session row id.
    pub session_id: i64,
    /// One-line session summary from the extraction model.
    pub summary: String,
    /// Facts the model produced (after capping).
    pub facts_extracted: usize,
    /// Memories inserted (new + superseding).
    pub memories_created: usize,
    /// Facts skipped as duplicates.
    pub duplicates_skipped: usize,
    /// Old memories invalidated by supersession.
    pub memories_superseded: usize,
    /// Whether STATE.md was rewritten.
    pub state_updated: bool,
}

/// Pull a JSON object out of model text: strips markdown fences and trims
/// to the outermost `{ … }` span.
pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then(|| &text[start..=end])
}

/// Call the extractor and parse typed JSON, retrying once with an explicit
/// "Return ONLY valid JSON" nudge on parse failure.
async fn complete_json<T: serde::de::DeserializeOwned>(
    extractor: &dyn Extractor,
    system: &str,
    user: &str,
) -> Result<T> {
    let first = extractor.complete(system, user).await?;
    if let Some(json) = extract_json(&first) {
        if let Ok(parsed) = serde_json::from_str::<T>(json) {
            return Ok(parsed);
        }
    }
    let retry_user = format!("{user}\n\nReturn ONLY valid JSON.");
    let second = extractor.complete(system, &retry_user).await?;
    let json = extract_json(&second)
        .ok_or_else(|| Error::Provider("extractor returned no JSON object".into()))?;
    serde_json::from_str::<T>(json)
        .map_err(|e| Error::Provider(format!("extractor JSON did not match schema: {e}")))
}

/// Run extraction + conflict resolution + state rewrite for an
/// already-persisted session. The session row is untouched on failure —
/// callers persist it first so the transcript survives any provider outage.
pub async fn run_pipeline(
    db: &mut Db,
    embedder: &dyn Embedder,
    extractor: &dyn Extractor,
    session_id: i64,
    project_id: Option<&str>,
    transcript: &str,
) -> Result<EndSessionResult> {
    let mut result = EndSessionResult {
        session_id,
        ..EndSessionResult::default()
    };

    // 1. Extract facts.
    let mut extraction: ExtractionOutput =
        complete_json(extractor, FACT_EXTRACTION_PROMPT, transcript).await?;
    extraction.facts.truncate(MAX_FACTS);
    result.summary = extraction.summary.clone();
    result.facts_extracted = extraction.facts.len();

    // 2. Resolve and apply each fact.
    for fact in &extraction.facts {
        let content = fact.content.trim();
        if content.is_empty() {
            continue;
        }
        let kind = fact
            .kind
            .as_deref()
            .filter(|k| MEMORY_KINDS.contains(k))
            .unwrap_or("fact")
            .to_string();
        let importance = fact.importance.unwrap_or(3).clamp(1, 5);
        let scope_project: Option<&str> = match fact.scope.as_deref() {
            Some("global") => None,
            _ => project_id,
        };

        let embedding = embedder
            .embed(std::slice::from_ref(&content.to_string()))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Provider("embedder returned no vector".into()))?;

        // Top-5 similar valid memories in the same scope.
        let candidates = retrieval::search(
            db.conn(),
            content,
            &embedding,
            &SearchParams {
                project_id: scope_project.map(String::from),
                kinds: None,
                limit: CONFLICT_CANDIDATES,
                as_of: None,
                recency: None,
            },
        )?;

        let verdict = if candidates.is_empty() {
            ConflictVerdict {
                verdict: "new".into(),
                supersedes: None,
            }
        } else {
            let listing: String = candidates
                .iter()
                .map(|c| format!("[{}] {}\n", c.memory.id, c.memory.content))
                .collect();
            let user = format!("NEW FACT: {content}\n\nEXISTING MEMORIES:\n{listing}");
            match complete_json::<ConflictVerdict>(extractor, CONFLICT_PROMPT, &user).await {
                Ok(v) => v,
                Err(e) => {
                    // A broken verdict must not lose the fact: insert as new.
                    tracing::warn!(error = %e, "conflict classification failed; treating fact as new");
                    ConflictVerdict {
                        verdict: "new".into(),
                        supersedes: None,
                    }
                }
            }
        };

        let new = NewMemory {
            project_id: scope_project.map(String::from),
            content: content.to_string(),
            kind,
            importance,
            source_session_id: Some(session_id),
        };
        let graph = GraphPayload {
            entities: fact
                .entities
                .iter()
                .map(|e| EntityRef {
                    name: e.name.clone(),
                    kind: e.kind.clone().unwrap_or_else(|| "other".into()),
                })
                .collect(),
            triples: fact
                .triples
                .iter()
                .map(|t| TripleRef {
                    source: t.source.clone(),
                    predicate: t.predicate.clone(),
                    target: t.target.clone(),
                })
                .collect(),
        };
        match verdict.verdict.as_str() {
            "duplicate" => {
                result.duplicates_skipped += 1;
            }
            "supersedes" => {
                // Only ids the model was actually shown are honored.
                let target = verdict
                    .supersedes
                    .filter(|id| candidates.iter().any(|c| c.memory.id == *id));
                match target {
                    Some(old_id) => {
                        queries::insert_memory(db.conn_mut(), &new, &embedding, Some(old_id), Some(&graph))?;
                        result.memories_created += 1;
                        result.memories_superseded += 1;
                    }
                    None => {
                        tracing::warn!("supersedes verdict named an unknown id; inserting as new");
                        queries::insert_memory(db.conn_mut(), &new, &embedding, None, Some(&graph))?;
                        result.memories_created += 1;
                    }
                }
            }
            _ => {
                queries::insert_memory(db.conn_mut(), &new, &embedding, None, Some(&graph))?;
                result.memories_created += 1;
            }
        }
    }

    // 3. Store the summary on the session row.
    if !result.summary.is_empty() {
        queries::set_session_summary(db.conn(), session_id, &result.summary)?;
    }

    // 4. Rewrite STATE.md (project sessions only). A rewrite failure is
    // logged but does not fail the pipeline — memories are already saved.
    if let Some(pid) = project_id {
        match rewrite_state(db, extractor, pid, &result.summary, &extraction.facts).await {
            Ok(()) => result.state_updated = true,
            Err(e) => tracing::warn!(error = %e, "STATE.md rewrite failed; memories were saved"),
        }
    }

    Ok(result)
}

/// Run the state-rewrite prompt and overwrite the project's STATE.md.
async fn rewrite_state(
    db: &mut Db,
    extractor: &dyn Extractor,
    project_id: &str,
    summary: &str,
    facts: &[ExtractedFact],
) -> Result<()> {
    let project = queries::get_project(db.conn(), project_id)?
        .ok_or_else(|| Error::InvalidInput(format!("project {project_id} vanished")))?;
    let facts_md: String = facts
        .iter()
        .map(|f| format!("- {}\n", f.content.trim()))
        .collect();
    let old_state = if project.state_md.trim().is_empty() {
        "(empty — this is the first session)".to_string()
    } else {
        project.state_md.clone()
    };
    let user = format!(
        "PROJECT: {}\n\nCURRENT STATE.md:\n{}\n\nSESSION SUMMARY:\n{}\n\nNEW FACTS APPLIED THIS SESSION:\n{}",
        project.name, old_state, summary, facts_md
    );
    let mut new_state = extractor.complete(STATE_REWRITE_PROMPT, &user).await?;
    // Strip accidental markdown fences defensively.
    let trimmed = new_state.trim();
    if trimmed.starts_with("```") {
        new_state = trimmed
            .trim_start_matches("```markdown")
            .trim_start_matches("```md")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
            .to_string();
    }
    if new_state.trim().is_empty() {
        return Err(Error::Provider("state rewrite returned an empty document".into()));
    }
    queries::update_project_state(db.conn(), project_id, &new_state)?;
    Ok(())
}
