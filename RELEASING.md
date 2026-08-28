# Releasing palimpsest-mcp

Everything below is prepared; the three credentialed steps are yours.

## 0. One-time setup

- crates.io: `cargo login` with your API token (crates.io → Account
  Settings → API Tokens).
- Homebrew tap: create the repository `jdiestramelendez/homebrew-tap`
  with a `Formula/` directory.

## 1. Publish to crates.io

```sh
cargo publish        # metadata + packaging already verified (cargo package passes)
```

The crate excludes tickets/evals/tests/plugin — it ships only the source
needed to build the binary (~71 KB).

## 2. Cut a GitHub release

```sh
git tag v0.1.0
git push origin v0.1.0
```

The `Release` workflow builds five targets (linux x86_64-musl,
linux aarch64, macOS x86_64, macOS arm64, windows x86_64), attaches the
archives plus a combined `SHA256SUMS`, and generates release notes.

## 3. Update the Homebrew formula

Copy `packaging/homebrew/palimpsest-mcp.rb` into the tap as
`Formula/palimpsest-mcp.rb`, set `version`, and replace the four
`REPLACE_WITH_*` placeholders with the matching lines from the release's
`SHA256SUMS`. Then:

```sh
brew install jdiestramelendez/tap/palimpsest-mcp
```

## Version bumps

Bump `version` in Cargo.toml, commit, repeat steps 1–3 with the new tag.
crates.io versions are immutable — never reuse a number.
