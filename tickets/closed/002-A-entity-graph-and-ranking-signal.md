# 002 · [A] Entity graph: extraction, storage, and third ranking signal

**Priority:** A · **Status:** done

## Context
Mem0's v3 algorithm fuses semantic + BM25 + entity matching. We already
have the first two; entities close the retrieval gap AND unlock the
knowledge-graph features (ticket 006, `get_related`).

## Scope
- New tables (append-only, consistent with the memories invariant):
  - `entities(id, name, norm_name, kind, project_id, created_at)` — kind in
    (technology, person, project, concept, other); `norm_name` lowercased
    for matching.
  - `memory_entities(memory_id, entity_id)` — mention edges.
  - `relations(id, source_entity_id, predicate, target_entity_id,
    evidence_memory_id)` — validity is derived from the evidence memory's
    `invalid_at`, so superseding a memory expires its edges automatically.
- Extraction: extend the fact-extraction prompt in `core/extraction.rs` to
  also return `entities` and `triples` per fact; upsert entities by
  `(norm_name, project_id)`, insert edges in the same transaction as the
  memory insert (`db/queries.rs::insert_memory` grows an optional
  entity payload).
- Retrieval: third leg in `core/retrieval.rs` — entities extracted from the
  query text (simple normalized-token match against `entities.norm_name`;
  no LLM call on the search path), ranked by number of shared entities,
  fused into the existing RRF with the same 1/(60+rank) formula.
- New MCP tool `get_related(entity, hops<=2)` returning the neighborhood
  with evidence memory ids.

## Technical details
- Migration: bump `schema_version` in `db/mod.rs`; add idempotent
  `CREATE TABLE IF NOT EXISTS` migration step for existing DBs.
- Keep the search path LLM-free: entity extraction happens only at write
  time (end_session / save_memory may skip entities — cheap fallback: none).
- RRF over 3 lists must degrade gracefully when the entity leg is empty
  (same pattern as the FTS-less case today).

## Acceptance criteria
- [x] Facts ingested via end_session produce entities and relations.
- [x] Superseding a memory removes its edges from current-time graph reads.
- [x] A query naming an entity ranks entity-linked memories above
      vector/lexical-only matches (test with pinned mock vectors).
- [x] `get_related` returns 1-hop and 2-hop neighborhoods with evidence.

## Out of scope
Graph visualization/export (ticket 006); entity dedup via embeddings.
