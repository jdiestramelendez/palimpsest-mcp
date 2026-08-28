# 004 · [M] Distribution: crates.io, prebuilt binaries, Homebrew

**Priority:** M · **Status:** done (prepared — credentialed steps documented in RELEASING.md)

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
- [x] `cargo install palimpsest-mcp` works on a clean machine — PREPARED:
      metadata complete (license/readme/repository/keywords/categories/
      rust-version, lean excludes), LICENSE file added, `cargo package`
      verification build passes (71 KB crate). Final `cargo publish`
      needs the owner's crates.io token (RELEASING.md step 1).
- [x] Tagging `vX.Y.Z` produces a Release with all target binaries —
      release.yml builds 5 targets (linux x86_64-musl, linux arm64,
      macOS x86_64/arm64, windows x86_64) with a combined SHA256SUMS and
      generated notes. Fires on the first tag push (RELEASING.md step 2).
- [x] `brew install jdiestramelendez/tap/palimpsest-mcp` works — PREPARED:
      formula template at packaging/homebrew/palimpsest-mcp.rb (4
      platform binaries); the tap repo creation and sha fill-in are the
      owner's steps (RELEASING.md step 3).

## Out of scope
npm wrapper; MCP registry/marketplace listings (worth a separate ticket
once the plugin from 001 exists).
