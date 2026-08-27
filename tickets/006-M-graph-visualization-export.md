# 006 · [M] Knowledge graph visualization and export

**Priority:** M · **Status:** pending
**Depends on:** 002

## Context
Seeing your accumulated knowledge as a graph is a headline feature — and
with append-only validity metadata we can show how it evolved over time.

## Scope
- CLI subcommand: `palimpsest-mcp graph [--project X] [--as-of TS]
  [--out graph.html|graph.mmd|graph.dot]`.
- HTML output: single self-contained file (inline JS/CSS, no CDN) with a
  force-directed layout — nodes = entities (size = mention count,
  color = kind), edges = relations (tooltip shows the evidence memory),
  plus a time slider that re-filters edges using the same `as_of`
  validity predicate as retrieval.
- Mermaid (`.mmd`) and DOT (`.dot`) exports for embedding in docs.

## Technical details
- Data comes from the ticket-002 tables via one query in `db/queries.rs`;
  serialize to a JSON blob embedded in the HTML template
  (`include_str!` the template, substitute a `/*DATA*/` marker).
- No server, no ports: write file, print the path (open manually).
  Keep the inline layout lib tiny (a ~200-line custom force sim is fine;
  avoid vendoring a huge bundle).
- Respect scope rules: `--project` includes global entities, same as search.

## Acceptance criteria
- [ ] `graph --out g.html` opens offline in a browser and renders nodes,
      edges and tooltips.
- [ ] Time slider (or `--as-of`) hides edges whose evidence memory was
      superseded after/before the chosen instant.
- [ ] Mermaid/DOT exports are syntactically valid.

## Out of scope
Live web UI / served dashboard; graph editing.
