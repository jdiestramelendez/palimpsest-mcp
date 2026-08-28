-- palimpsest-mcp schema. {DIMS} is substituted with the configured embedding
-- dimensionality at first creation; the value is then pinned in `meta`.
--
-- Invariants encoded here (never violate):
--   * memories rows are append-only: supersession sets invalid_at +
--     superseded_by, nothing is ever hard-deleted or content-updated.
--   * memories_vec / memories_fts mirror memories via same-transaction
--     inserts (vec) and triggers (fts).

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS projects (
    id               TEXT PRIMARY KEY,          -- slug, e.g. "palimpsest-mcp"
    name             TEXT NOT NULL,
    aliases          TEXT NOT NULL DEFAULT '[]',-- JSON array of strings
    state_md         TEXT NOT NULL DEFAULT '',
    state_updated_at TEXT,
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS sessions (
    id              INTEGER PRIMARY KEY,
    project_id      TEXT REFERENCES projects(id),
    client          TEXT,                       -- e.g. "claude-code", "cursor"
    transcript      TEXT NOT NULL,
    transcript_hash TEXT,                       -- sha256 hex; ingest dedup key
    summary         TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
CREATE INDEX IF NOT EXISTS idx_sessions_hash ON sessions(transcript_hash);

CREATE TABLE IF NOT EXISTS memories (
    id                INTEGER PRIMARY KEY,
    project_id        TEXT REFERENCES projects(id),  -- NULL = global memory
    content           TEXT NOT NULL,
    kind              TEXT NOT NULL DEFAULT 'fact'
                      CHECK (kind IN ('fact','decision','preference','todo','insight')),
    importance        INTEGER NOT NULL DEFAULT 3 CHECK (importance BETWEEN 1 AND 5),
    source_session_id INTEGER REFERENCES sessions(id),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    valid_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    invalid_at        TEXT,                          -- NULL = currently valid
    superseded_by     INTEGER REFERENCES memories(id)
);

-- Partial index: the hot path only ever scans currently-valid memories.
CREATE INDEX IF NOT EXISTS idx_memories_valid
    ON memories(project_id, kind) WHERE invalid_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_memories_session ON memories(source_session_id);

-- Vector index (sqlite-vec). memory_id mirrors memories.id.
CREATE VIRTUAL TABLE IF NOT EXISTS memories_vec USING vec0(
    memory_id INTEGER PRIMARY KEY,
    embedding float[{DIMS}]
);

-- Full-text index over memory content. remove_diacritics 2 folds accented
-- characters, so "configuración" matches "configuracion".
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    content,
    content='memories',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

-- FTS sync triggers. The DELETE/UPDATE variants exist for index hygiene
-- (external-content FTS requires them to stay consistent); application code
-- never deletes memories or updates their content.
CREATE TRIGGER IF NOT EXISTS memories_fts_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS memories_fts_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
END;

CREATE TRIGGER IF NOT EXISTS memories_fts_au AFTER UPDATE OF content ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
    INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
END;

-- ── Entity graph (schema v3) ─────────────────────────────────────────────
-- Entities are upserted by (norm_name, scope); edges carry the memory that
-- evidences them, so superseding a memory expires its edges automatically
-- (current-time graph reads filter on the evidence memory's validity).

CREATE TABLE IF NOT EXISTS entities (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,                  -- display form, first seen
    norm_name  TEXT NOT NULL,                  -- lowercased/normalized match key
    kind       TEXT NOT NULL DEFAULT 'other'
               CHECK (kind IN ('technology','person','project','concept','other')),
    project_id TEXT REFERENCES projects(id),   -- NULL = global scope
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_norm
    ON entities(norm_name, ifnull(project_id, ''));

CREATE TABLE IF NOT EXISTS memory_entities (
    memory_id INTEGER NOT NULL REFERENCES memories(id),
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    PRIMARY KEY (memory_id, entity_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_memory_entities_entity
    ON memory_entities(entity_id);

CREATE TABLE IF NOT EXISTS relations (
    id                 INTEGER PRIMARY KEY,
    source_entity_id   INTEGER NOT NULL REFERENCES entities(id),
    predicate          TEXT NOT NULL,
    target_entity_id   INTEGER NOT NULL REFERENCES entities(id),
    evidence_memory_id INTEGER NOT NULL REFERENCES memories(id),
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_entity_id);
CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_entity_id);
