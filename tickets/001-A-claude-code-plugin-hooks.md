# 001 · [A] Claude Code plugin with lifecycle hooks

**Priority:** A · **Status:** pending

## Context
Today the server depends on the agent *choosing* to call the tools.
claude-mem wins adoption because its hooks fire automatically. This is the
single biggest gap for real-world use.

## Scope
A Claude Code plugin (marketplace-installable folder) that wires
palimpsest-mcp into the session lifecycle:
- `SessionStart` hook: run `get_project_state` for the current project
  (derive the project id from the repo/directory name) and inject
  `state_md` into context via hook output.
- `SessionEnd` (and `PreCompact`) hook: feed the transcript to the
  extraction pipeline automatically.
- Plugin also registers the MCP server itself so install is one step.

## Technical details
- Add a CLI subcommand `palimpsest-mcp ingest --project <id> --transcript
  <path> [--client claude-code]` that runs the same pipeline as the
  `end_session` tool (reuse `core::extraction::run_pipeline`; no MCP round
  trip needed from a hook). Subcommand shares `--db-path` handling in
  `src/main.rs` (switch to a clap subcommand enum; default = serve).
- Hooks receive session info as JSON on stdin (transcript path included for
  SessionEnd); parse defensively.
- Plugin layout: `plugin/` dir with `.claude-plugin/plugin.json`, `hooks/`
  scripts calling the binary, MCP server registration.
- Idempotency: a SessionEnd firing twice must not double-ingest — store a
  transcript content hash in `sessions` and skip if already ingested.

## Acceptance criteria
- [ ] `palimpsest-mcp ingest` runs the full pipeline from a transcript file.
- [ ] Duplicate ingest of the same transcript is a no-op (hash check).
- [ ] Fresh Claude Code session in a repo shows injected state without any
      manual tool call.
- [ ] Closing a session creates memories automatically.

## Out of scope
Cursor/other-client automation; UserPromptSubmit/PostToolUse observation
(claude-mem-style granular capture).
