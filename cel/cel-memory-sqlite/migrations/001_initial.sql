-- Cellar memory storage schema — initial migration.
-- See /Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md §6.2.
--
-- This migration assumes the sqlite-vec extension is already loaded into
-- the connection (the SqliteMemoryProvider does that at open time, before
-- running migrations).

-- Primary content table. One row per chunk of memory.
CREATE TABLE memory_chunks (
    id              TEXT PRIMARY KEY,         -- uuid v7
    created_at      INTEGER NOT NULL,         -- unix ms
    kind            TEXT NOT NULL,            -- see ChunkKind
    tier            TEXT NOT NULL,            -- session | long_term
    source          TEXT NOT NULL,            -- see ChunkSource
    session_id      TEXT,                     -- nullable
    project_root    TEXT,                     -- nullable
    caller_id       TEXT NOT NULL,
    content         TEXT NOT NULL,
    metadata        TEXT NOT NULL,            -- JSON
    importance      REAL NOT NULL DEFAULT 0.5,
    pinned          INTEGER NOT NULL DEFAULT 0,
    shareable       INTEGER NOT NULL DEFAULT 0,
    superseded_by   TEXT,
    embedding_model TEXT NOT NULL,
    embedding_dim   INTEGER NOT NULL
);

CREATE INDEX idx_memory_kind_tier ON memory_chunks(kind, tier);
CREATE INDEX idx_memory_session   ON memory_chunks(session_id);
CREATE INDEX idx_memory_caller    ON memory_chunks(caller_id);
CREATE INDEX idx_memory_created   ON memory_chunks(created_at DESC);
CREATE INDEX idx_memory_project   ON memory_chunks(project_root);

-- Vector index (sqlite-vec virtual table). One row per chunk that has an
-- embedding. The dim must match the embedder; we use bge-small-en-v1.5 by
-- default (384). The provider rejects writes whose embedding_dim doesn't
-- match this fixed schema dim.
CREATE VIRTUAL TABLE memory_vec USING vec0(
    chunk_id  TEXT PRIMARY KEY,
    embedding FLOAT[384]
);

-- Full-text index. Maintained by triggers below.
CREATE VIRTUAL TABLE memory_fts USING fts5(
    chunk_id UNINDEXED,
    content,
    tokenize = 'porter unicode61'
);

CREATE TRIGGER memory_chunks_ai_fts AFTER INSERT ON memory_chunks BEGIN
    INSERT INTO memory_fts(chunk_id, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER memory_chunks_au_fts AFTER UPDATE OF content ON memory_chunks BEGIN
    UPDATE memory_fts SET content = new.content WHERE chunk_id = new.id;
END;

CREATE TRIGGER memory_chunks_ad_fts AFTER DELETE ON memory_chunks BEGIN
    DELETE FROM memory_fts WHERE chunk_id = old.id;
END;

-- Sessions: groups of chunks that belong to one conversation or one
-- delegated job.
CREATE TABLE memory_sessions (
    id          TEXT PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER,
    caller_id   TEXT NOT NULL,
    title       TEXT,
    summary     TEXT,
    outcome     TEXT NOT NULL,                -- open | success | failure | aborted
    metadata    TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_memory_sessions_caller ON memory_sessions(caller_id);
CREATE INDEX idx_memory_sessions_outcome ON memory_sessions(outcome);
CREATE INDEX idx_memory_sessions_started ON memory_sessions(started_at DESC);

-- Summary -> constituent chunks join table.
CREATE TABLE memory_summary_members (
    rollup_id  TEXT NOT NULL,
    member_id  TEXT NOT NULL,
    PRIMARY KEY (rollup_id, member_id)
);

-- Access log: every retrieval. Drives relevance feedback and the recall@k
-- benchmark (Phase 5 of the Memory subsystem).
CREATE TABLE memory_access_log (
    ts            INTEGER NOT NULL,
    chunk_id      TEXT NOT NULL,
    retrieved_by  TEXT NOT NULL,
    query_hash    TEXT NOT NULL,
    rank          INTEGER NOT NULL,
    used          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_memory_access_ts    ON memory_access_log(ts DESC);
CREATE INDEX idx_memory_access_query ON memory_access_log(query_hash);

-- Eviction log. Audit trail for every delete.
CREATE TABLE memory_eviction_log (
    ts        INTEGER NOT NULL,
    chunk_id  TEXT NOT NULL,
    reason    TEXT NOT NULL,
    metadata  TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_memory_eviction_ts ON memory_eviction_log(ts DESC);
