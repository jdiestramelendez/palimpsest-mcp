---
name: memory
description: Use the memory MCP server to stay continuous across sessions - load project state at session start, search past decisions when context is missing, save durable facts, and close sessions so memory stays current.
---

# Project memory

This project uses the `palimpsest-mcp` server. Follow this protocol in every
session:

## At session start

Call `get_project_state` with this project's name before doing anything
else. Read the returned `state_md` — it is the project's current truth
(what's in flight, standing decisions, conventions, open items). If the
project is unknown, continue without state; it will be created on the
first `save_memory` or `end_session`.

## During the session

- **Missing context?** Before asking the user or guessing, call
  `search_memory` with a natural-language query (scope it to this
  project; global user preferences are included automatically). Use
  `as_of` only when the user asks about the past.
- **User says "remember …" or a durable decision is made?** Call
  `save_memory` with one self-contained sentence. Pick `kind`
  (fact | decision | preference | todo | insight) and `importance` (1–5).
  If it replaces a fact you just retrieved, pass that memory's id as
  `supersedes` instead of saving a contradicting duplicate.

## At session end

When the user wraps up (or asks to close the session), call `end_session`
with the session transcript (a faithful condensed version is fine) and
this project's name. This extracts durable facts and rewrites the state
document that the next session will start from. Report the returned counts
to the user in one line.

## Periodic review

Every couple of weeks (or when the user asks to "clean up memory"), call
`review_memories` for this project. For each stale memory listed:
- still true -> include its id in `confirm` on the next call;
- outdated -> `save_memory` with `supersedes` and the corrected fact;
- noise -> `save_memory` with `supersedes` and importance 1.
Report a one-line summary of what was confirmed/corrected.

## Rules

- Never store secrets (API keys, tokens, passwords) in memories.
- Prefer one precise memory over several vague ones.
- Old facts are never deleted — supersede them; history stays queryable.
