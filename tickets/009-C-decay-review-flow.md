# 009 · [C] Decay/review: surface stale memories (never delete)

**Priority:** C · **Status:** pending
**Depends on:** 003 (usage tracking)

## Context
Memory quality degrades as noise accumulates. The fix is a review flow —
marking, never deleting, in keeping with the append-only invariant.

## Scope
- `search_memory` gains an optional `stale: true` filter (or a dedicated
  `review_memories` tool): currently-valid memories with
  `last_retrieved_at` older than 90 days (configurable) and importance <= 3.
- Review verdicts map to existing primitives: still true -> touch
  `last_retrieved_at`; outdated -> supersede with the corrected fact;
  noise -> supersede with kind downgrade or importance 1 (soft-archive) —
  no new mutation paths.
- SKILL.md template gains a short "periodic review" section.

## Technical details
- Pure query-layer + tool-layer change; no schema change beyond 003.
- Exclude global preferences from staleness by default (they're evergreen).

## Acceptance criteria
- [ ] Stale filter returns only valid, old-untouched, low-importance rows.
- [ ] No new UPDATE/DELETE surface on memories content.

## Out of scope
Automatic archiving; LLM-driven bulk review.
