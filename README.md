# palimpsest-mcp

Persistent memory for coding agents, served over MCP. A *palimpsest* is a
manuscript written over an older text whose earlier layers remain visible
underneath — which is exactly how this store treats facts: corrections
layer on top, nothing underneath is ever erased. A single SQLite file
stores project facts with full history (nothing is ever deleted — outdated
facts are *superseded* and remain queryable via time-travel), retrieval is
hybrid semantic + lexical (sqlite-vec KNN fused with FTS5 BM25 via
Reciprocal Rank Fusion), and every project carries a living `STATE.md`
document that is rewritten at the end of each session.

## Tools

| Tool | Purpose |
|---|---|
| `get_project_state` | Load a project's STATE.md, aliases and memory counts. Call at session start. |
| `save_memory` | Save one durable fact (optionally superseding an outdated one). |
| `search_memory` | Hybrid search (vector + lexical + entity signals, recency-boosted) with project scoping, kind filters and `as_of` time-travel. |
| `get_related` | Explore the knowledge graph around an entity (1-2 hops, with evidence). |
| `end_session` | Persist the transcript, extract durable facts, rewrite STATE.md. |

## Install

```sh
cargo build --release
# binary at target/release/palimpsest-mcp
```

Data lives in `~/.palimpsest-mcp/` (`palimpsest.db`, `config.toml`). Override the
database location with `--db-path /path/to.db`.

## Configuration — `~/.palimpsest-mcp/config.toml`

Everything is optional; the defaults use a local Ollama with no API keys.

```toml
# db_path = "/custom/path/palimpsest.db"   # optional; --db-path wins over this

[embedder]
provider = "ollama"          # "ollama" | "openai"
model = "nomic-embed-text"
dims = 768                   # pinned into the DB at first creation;
                             # changing it later requires re-embedding
# base_url = "http://localhost:11434"
# api_key_env = "OPENAI_API_KEY"      # env var read for cloud providers

[retrieval]
recency_weight = 0.25        # score multiplier: 1 + w * exp(-age/half_life)
recency_half_life_days = 30  # 0 weight disables the boost; as_of searches
                             # never apply it

[extractor]
provider = "ollama"          # "ollama" | "anthropic" | "openai"
model = "qwen2.5:7b"         # e.g. "claude-haiku-4-5" / "gpt-4o-mini"
# api_key_env = "ANTHROPIC_API_KEY"
# base_url = ...
```

API keys are **never** stored in the file: `api_key_env` names an
environment variable and the key is read from the process environment.

Logs go to stderr (stdout is the MCP transport); set `RUST_LOG=debug` for
verbose logging.

## Register the server

**Claude Code — plugin (recommended)**

The plugin wires everything automatically: project state is injected at
session start and durable facts are extracted at session end, no manual
tool calls needed.

```sh
cargo install --path .        # puts palimpsest-mcp on your PATH
claude plugin marketplace add jdiestramelendez/palimpsest-mcp
claude plugin install palimpsest@palimpsest-mcp
```

**Claude Code — MCP server only**

```sh
claude mcp add --scope user palimpsest -- /absolute/path/to/target/release/palimpsest-mcp
```

**Claude Desktop** — add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "palimpsest": {
      "command": "/absolute/path/to/target/release/palimpsest-mcp"
    }
  }
}
```

**Cursor** — add to `~/.cursor/mcp.json` (or project `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "palimpsest": {
      "command": "/absolute/path/to/target/release/palimpsest-mcp"
    }
  }
}
```

## First 5 minutes

1. **Save a memory** — in a session, say
   *"Remember that this project deploys from the `release` branch only."*
   The agent calls `save_memory`; you get back a `memory_id`.
2. **Close the session** — say *"We're done, close out this session."*
   The agent calls `end_session` with the transcript: facts are extracted,
   deduplicated against what's already known, and STATE.md is rewritten.
3. **Reopen** — start a fresh session and ask *"Where were we on
   \<project\>?"* The agent calls `get_project_state` and answers from
   STATE.md — including the deploy-branch fact from step 1.
4. **Time-travel (optional)** — *"What did we believe about deployment last
   month?"* → `search_memory` with `as_of` returns the facts that were
   valid then, even if they've since been superseded.

To make agents do this reliably, drop the skill template from
`docs/skill-template/SKILL.md` into your project (e.g.
`.claude/skills/memory/SKILL.md`).

## CLI

```sh
palimpsest-mcp                                   # serve MCP over stdio (default)
palimpsest-mcp ingest --transcript notes.txt \
    --project my-app [--format claude-jsonl]     # manual ingestion (idempotent)
palimpsest-mcp hook session-start                # Claude Code hook entry points
palimpsest-mcp hook session-end                  #   (hook JSON on stdin)
```

Ingestion is idempotent: the same transcript content (sha256) is never
processed twice, so lifecycle hooks firing repeatedly are no-ops.

## Retrieval quality

`evals/` holds a deterministic eval harness: `cargo run --bin eval` scores
hit@1/3/8 and MRR over a seeded dataset (lexical, semantic, diacritics,
entity, as_of and global cases); `--check` enforces committed floors in CI
and `--provider ollama` runs it against real embeddings. Baseline lives in
`evals/RESULTS.md`.

## Guarantees

- **Append-only history**: no memory row is ever hard-deleted and content
  is never edited; corrections insert a replacement and stamp
  `invalid_at`/`superseded_by` on the old row in the same transaction.
  (The only other mutable fields are the usage-tracking columns
  `last_retrieved_at`/`retrieval_count`, bumped when a current-time search
  returns a memory.)
- **Atomic writes**: the vector index row is inserted in the same
  transaction as the memory row; FTS stays in sync via triggers.
- **Validity-filtered retrieval**: searches only return currently-valid
  facts (or the facts valid at `as_of`), and global memories
  (`project_id IS NULL`) are always included alongside project scope.
- **Knowledge graph**: sessions also extract entities and relations;
  search fuses a third, entity-match signal into the ranking, and
  `get_related` walks the graph. Edges are evidenced by memories, so
  superseding a fact expires its edges automatically.
