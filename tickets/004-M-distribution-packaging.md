# 004 · [M] Distribution: crates.io, prebuilt binaries, Homebrew

**Priority:** M · **Status:** pending

## Context
"Clone and compile Rust" loses against `npx`/plugin installs every time.
Lower the install friction to one command.

## Scope
- Publish `palimpsest-mcp` to crates.io (`cargo install palimpsest-mcp`).
- GitHub Actions release workflow: on tag push, build binaries for
  macOS (arm64/x86_64), Linux (x86_64/arm64, musl for portability) and
  Windows; attach to a GitHub Release with checksums.
- Homebrew tap (`jdiestramelendez/homebrew-tap`) with a formula pointing
  at the release binaries.

## Technical details
- Cargo.toml already has name/description/license; add `repository`,
  `keywords`, `categories`, and a `readme` field before publishing.
- The crates.io name `palimpsest-mcp` was verified free (2026-08-27).
- sqlite-vec builds C code: confirm cross-compilation targets in CI
  (musl needs a C toolchain; use `cross` or zig cc if plain cargo fails).
- Follow-up niceties: `cargo-dist` can generate most of this workflow.

## Acceptance criteria
- [ ] `cargo install palimpsest-mcp` works on a clean machine.
- [ ] Tagging `vX.Y.Z` produces a Release with all target binaries.
- [ ] `brew install jdiestramelendez/tap/palimpsest-mcp` works on macOS.

## Out of scope
npm wrapper; MCP registry/marketplace listings (worth a separate ticket
once the plugin from 001 exists).
