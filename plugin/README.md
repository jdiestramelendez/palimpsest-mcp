# palimpsest Claude Code plugin

Wires [palimpsest-mcp](https://github.com/jdiestramelendez/palimpsest-mcp)
into Claude Code's session lifecycle:

- **SessionStart** → injects the current project's living state document
  (STATE.md) as context, automatically.
- **SessionEnd / PreCompact** → feeds the session transcript to the
  extraction pipeline: durable facts are saved (deduplicated by content
  hash, so repeated firings are no-ops) and the state document is rewritten.
- Registers the `palimpsest` MCP server (its 4 tools) for explicit
  `save_memory` / `search_memory` calls mid-session.

## Requirements

The `palimpsest-mcp` binary must be on your PATH:

```sh
cargo install --path ..   # from a clone, or: cargo install palimpsest-mcp
```

## Install

```sh
claude plugin marketplace add jdiestramelendez/palimpsest-mcp
claude plugin install palimpsest@palimpsest-mcp
```

The project name is derived from the git repository (or directory) name of
the session's working directory.
