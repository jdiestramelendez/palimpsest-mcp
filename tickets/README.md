# Tickets

Lightweight file-based ticket tracking for palimpsest-mcp.

## Conventions

- **File name**: `NNN-P-slug.md` — 3-digit number first (so listings sort
  naturally), then the priority tag, then a descriptive slug.
- **Priorities**: `A` = high (moves the needle now), `M` = medium (next
  wave), `C` = backlog (whenever it fits). To reprioritize, rename the
  file — the number never changes; it is the ticket's identity.
- **Flow**: active tickets live in `tickets/`; when finished, move the file
  as-is into `tickets/closed/` (it keeps its number and name — history is
  never deleted, in keeping with the project's philosophy).
- **New tickets**: take the next free number, counting `closed/` too.

## Ticket template

```markdown
# NNN · [P] Title

**Priority:** P · **Status:** pending

## Context
Why this ticket exists (1-3 lines).

## Scope
Exactly what is included.

## Technical details
Files/modules to touch, decisions already made, known gotchas.

## Acceptance criteria
- [ ] Verifiable, checklist style.

## Out of scope
What does NOT belong here (to stop scope creep).
```
