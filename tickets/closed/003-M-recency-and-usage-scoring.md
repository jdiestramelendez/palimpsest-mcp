# 003 · [M] Recency and usage signals in scoring

**Priority:** M · **Status:** done

## Context
Ranking today is RRF + importance tie-break. Recency and access frequency
matter (claude-mem weighs recency 30%); usage data also feeds ticket 009
(decay/review).

## Scope
- Track usage: add `last_retrieved_at` and `retrieval_count` columns to
  `memories`; bump them (single UPDATE of tracking columns only — content
  stays immutable) for every row returned by `search_memory`.
- Scoring: multiply the RRF score by a recency boost, e.g.
  `1 + w * exp(-age_days / half_life)` with `w` and `half_life`
  configurable in `config.toml` (`[retrieval]` section, defaults ~0.25 /
  30 days). Age from `valid_at`.
- `as_of` searches must NOT apply the recency boost (historical queries
  rank by historical relevance) and must not bump usage counters.

## Technical details
- `core/retrieval.rs`: fold the boost into the final ORDER BY expression;
  keep the tie-break chain (score DESC, importance DESC, id DESC).
- The usage UPDATE is the one new sanctioned UPDATE on `memories` — it
  touches ONLY tracking columns. Extend the Step-6 invariant audit grep in
  CI/docs to assert no UPDATE ever touches `content`.
- Migration: idempotent ALTER TABLE ADD COLUMN guarded by schema_version.

## Acceptance criteria
- [x] Fresh memory outranks an equally-scored stale one; boost is bounded
      (multiplier capped at 1 + weight).
- [x] Usage counters increment on search hits; `as_of` searches don't.
- [x] Config knobs respected; defaults sensible with no config file.

## Out of scope
Automatic pruning or archiving (ticket 009).
