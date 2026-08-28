# 007 · [C] backup / restore subcommands

**Priority:** C · **Status:** done

## Context
A single SQLite file holds a user's entire accumulated memory. One corrupt
disk without a backup story is fatal trust damage.

## Scope
- `palimpsest-mcp backup [--out path]`: online backup via SQLite's
  `.backup` API (rusqlite `backup` feature) — safe while the server runs
  (WAL). Default output: `~/.palimpsest-mcp/backups/palimpsest-YYYYMMDD-HHMMSS.db`.
- `palimpsest-mcp restore <file>`: verify the file opens and passes
  `PRAGMA integrity_check` and the dims/meta check, then swap it in
  (previous db renamed `.pre-restore`, never deleted).

## Technical details
- Enable rusqlite's `backup` feature in Cargo.toml.
- Restore must refuse while dims mismatch the config (reuse
  `Error::DimsMismatch` path in `db/mod.rs`).
- Consider a `--keep N` rotation flag for backups.

## Acceptance criteria
- [x] Backup taken during active writes opens clean and passes
      integrity_check (taken with a live WAL connection open; FTS intact).
- [x] Restore swaps atomically and keeps the previous file
      (.pre-restore-<ts>, with -wal/-shm siblings); verification runs on a
      scratch copy so the backup source is never mutated; dims mismatch
      and corrupt files are refused with the live DB untouched. --keep N
      rotation implemented for the default backups directory.

## Out of scope
Scheduled/automatic backups; cloud replication (litestream is the DIY
answer — document it in the README instead).
