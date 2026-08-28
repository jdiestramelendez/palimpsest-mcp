# 010 · [C] Streamable HTTP transport (shared memory across machines)

**Priority:** C · **Status:** done

## Context
stdio covers the single-machine case. Sharing one memory across machines
(desktop + laptop) needs rmcp's streamable HTTP server transport.

## Scope
- `palimpsest-mcp serve --http 127.0.0.1:PORT` flag; stdio stays the
  default when the flag is absent.
- Bearer-token auth REQUIRED for any non-localhost bind (token from env
  var; refuse to start unauthenticated on 0.0.0.0 — learn from
  claude-mem's unauthenticated-port audit finding).

## Technical details
- Enable rmcp's `transport-streamable-http-server` feature; the
  `MemoryServer` handler is transport-agnostic already.
- SQLite stays single-writer: the tokio::Mutex already serializes writes;
  document that multi-client concurrency is fine but throughput-limited.
- TLS is out of scope — document reverse-proxy (caddy/nginx) as the
  supported path.

## Acceptance criteria
- [x] Same tool set reachable over HTTP; stdio unchanged by default
      (initialize verified over real HTTP: 200 + serverInfo; stdio path
      untouched).
- [x] Non-localhost bind without a token refuses to start (blank tokens
      count as missing; IPv6 loopback recognized as local); with a token,
      missing/wrong Authorization gets 401, correct token 200 — verified
      in tests and against the real binary.

## Out of scope
Multi-tenant auth/scoping; sync/replication between two DB files.
