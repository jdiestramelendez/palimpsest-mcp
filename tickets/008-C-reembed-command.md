# 008 · [C] reembed command (switch embedders safely)

**Priority:** C · **Status:** pending

## Context
Dims are pinned at DB creation and mismatches refuse to open (by design).
`reembed` is the sanctioned escape hatch the error message promises.

## Scope
- `palimpsest-mcp reembed [--batch-size N]`: re-embed every memory's
  content with the embedder from the CURRENT config into a NEW database
  file, then swap (old file kept as `.pre-reembed`, never deleted).

## Technical details
- Build the new DB with `Db::open` at the new dims; stream memories in
  id order, batch-embed (`Embedder::embed` already takes a batch), and
  copy ALL columns verbatim (ids, validity, supersession links, sessions,
  projects, entities when 002 lands) — only `memories_vec` differs.
- Progress to stderr (tracing); resumable is overkill, but make the whole
  thing transactional per batch so a crash leaves the new file discardable.
- Update the meta table dims in the new file; refuse to run if source and
  target dims are equal AND the model name is unchanged (no-op guard).

## Acceptance criteria
- [ ] After reembed, the DB opens with the new config and search works.
- [ ] Row counts and supersession links identical pre/post.
- [ ] Old file preserved.

## Out of scope
Incremental/partial re-embedding; embedding-version tracking per row.
