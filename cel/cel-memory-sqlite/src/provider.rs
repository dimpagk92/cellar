//! `SqliteMemoryProvider` — the v1 Phase 0 backing storage.
//!
//! This implementation is the foundation the full Memory & Context Manager
//! subsystem (`cellar-memory-manager.md` Phase 1+) fills in. Phase 0 ships:
//!
//! - Real `open` that loads sqlite-vec, runs migrations, holds the connection.
//! - Real `write` / `get` / `stats` / `purge_all`.
//! - `Err(NotImplemented)` for retrieval, summarization, rollups, re-embed,
//!   maintenance methods — these need the hybrid retrieval pipeline and
//!   the summarizer LLM client which are Phase 1+ work.
//!
//! The provider is `Send + Sync` and behind an `Arc<dyn MemoryProvider>` —
//! identical surface to [`BasicMemoryProvider`] so the daemon's
//! `wire_subsystems()` swap is one line.
//!
//! [`BasicMemoryProvider`]: cel_memory::BasicMemoryProvider

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cel_memory::{
    AccessEntry, AgingReport, CallerScope, ChunkKind, ChunkSource, EvictionEntry, EvictionReason,
    ExportBundle, ExportFilter, MemoryChunk, MemoryError, MemoryPredicate, MemoryProvider,
    MemoryQuery, MemorySession, MemoryStats, MemoryTier, NewMemoryChunk, NewMemorySession,
    PurgeReport, ReEmbedReport, Result as MemoryResult, SessionFilter, SessionOutcome,
};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::embedder::Embedder;
use crate::error::SqliteMemoryError;
use crate::migrations;

/// SQLite-backed [`MemoryProvider`].
pub struct SqliteMemoryProvider {
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<dyn Embedder>,
}

impl SqliteMemoryProvider {
    /// Open or create a memory database at the given path. Loads
    /// `sqlite-vec` into the connection, runs pending migrations, returns
    /// a ready-to-use provider.
    ///
    /// The provided [`Embedder`] determines the dimensionality used at
    /// write time. The migration schema currently hard-codes `FLOAT[384]`
    /// for `memory_vec`; if the embedder's dim is different, writes that
    /// produce embeddings will fail with `DimMismatch`. Future migrations
    /// will make the dim configurable.
    pub async fn open(
        path: impl AsRef<Path>,
        embedder: Arc<dyn Embedder>,
    ) -> Result<Self, SqliteMemoryError> {
        let path = path.as_ref().to_path_buf();
        crate::vec_extension::register();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection, SqliteMemoryError> {
            let mut c = Connection::open(&path)?;
            // WAL mode for concurrent reads while a writer is active.
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "synchronous", "NORMAL")?;
            migrations::run(&mut c)?;
            Ok(c)
        })
        .await
        .map_err(|e| SqliteMemoryError::BlockingJoin(e.to_string()))??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder,
        })
    }

    /// Open an in-memory database for tests.
    pub async fn open_in_memory(embedder: Arc<dyn Embedder>) -> Result<Self, SqliteMemoryError> {
        crate::vec_extension::register();
        let conn = tokio::task::spawn_blocking(|| -> Result<Connection, SqliteMemoryError> {
            let mut c = Connection::open_in_memory()?;
            migrations::run(&mut c)?;
            Ok(c)
        })
        .await
        .map_err(|e| SqliteMemoryError::BlockingJoin(e.to_string()))??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder,
        })
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn dt_to_ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn ms_to_dt(ms: i64) -> DateTime<Utc> {
    chrono::DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp_millis(0).unwrap())
}

fn kind_str(k: ChunkKind) -> &'static str {
    match k {
        ChunkKind::Chat => "chat",
        ChunkKind::Action => "action",
        ChunkKind::Fire => "fire",
        ChunkKind::Observation => "observation",
        ChunkKind::Correction => "correction",
        ChunkKind::JobSummary => "job_summary",
        ChunkKind::Context => "context",
        ChunkKind::Rollup => "rollup",
    }
}

fn str_to_kind(s: &str) -> Result<ChunkKind, MemoryError> {
    Ok(match s {
        "chat" => ChunkKind::Chat,
        "action" => ChunkKind::Action,
        "fire" => ChunkKind::Fire,
        "observation" => ChunkKind::Observation,
        "correction" => ChunkKind::Correction,
        "job_summary" => ChunkKind::JobSummary,
        "context" => ChunkKind::Context,
        "rollup" => ChunkKind::Rollup,
        other => return Err(MemoryError::Storage(format!("unknown kind: {other}"))),
    })
}

fn source_str(s: ChunkSource) -> &'static str {
    match s {
        ChunkSource::Embedded => "embedded",
        ChunkSource::Mcp => "mcp",
        ChunkSource::Gateway => "gateway",
        ChunkSource::Matcher => "matcher",
        ChunkSource::Cortex => "cortex",
        ChunkSource::System => "system",
    }
}

fn str_to_source(s: &str) -> Result<ChunkSource, MemoryError> {
    Ok(match s {
        "embedded" => ChunkSource::Embedded,
        "mcp" => ChunkSource::Mcp,
        "gateway" => ChunkSource::Gateway,
        "matcher" => ChunkSource::Matcher,
        "cortex" => ChunkSource::Cortex,
        "system" => ChunkSource::System,
        other => return Err(MemoryError::Storage(format!("unknown source: {other}"))),
    })
}

fn tier_str(t: MemoryTier) -> &'static str {
    match t {
        MemoryTier::Session => "session",
        MemoryTier::LongTerm => "long_term",
    }
}

fn str_to_tier(s: &str) -> Result<MemoryTier, MemoryError> {
    Ok(match s {
        "session" => MemoryTier::Session,
        "long_term" => MemoryTier::LongTerm,
        other => return Err(MemoryError::Storage(format!("unknown tier: {other}"))),
    })
}

fn outcome_str(o: SessionOutcome) -> &'static str {
    match o {
        SessionOutcome::Open => "open",
        SessionOutcome::Success => "success",
        SessionOutcome::Failure => "failure",
        SessionOutcome::Aborted => "aborted",
    }
}

fn str_to_outcome(s: &str) -> Result<SessionOutcome, MemoryError> {
    Ok(match s {
        "open" => SessionOutcome::Open,
        "success" => SessionOutcome::Success,
        "failure" => SessionOutcome::Failure,
        "aborted" => SessionOutcome::Aborted,
        other => return Err(MemoryError::Storage(format!("unknown outcome: {other}"))),
    })
}

fn row_to_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryChunk> {
    let metadata_str: String = row.get("metadata")?;
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
    Ok(MemoryChunk {
        id: row.get("id")?,
        created_at: ms_to_dt(row.get::<_, i64>("created_at")?),
        kind: str_to_kind(&row.get::<_, String>("kind")?).unwrap_or(ChunkKind::Chat),
        tier: str_to_tier(&row.get::<_, String>("tier")?).unwrap_or(MemoryTier::Session),
        source: str_to_source(&row.get::<_, String>("source")?).unwrap_or(ChunkSource::System),
        session_id: row.get("session_id")?,
        project_root: row.get("project_root")?,
        caller_id: row.get("caller_id")?,
        content: row.get("content")?,
        metadata,
        importance: row.get::<_, f64>("importance")? as f32,
        pinned: row.get::<_, i64>("pinned")? != 0,
        superseded_by: row.get("superseded_by")?,
        embedding_model: row.get("embedding_model")?,
        embedding_dim: row.get::<_, i64>("embedding_dim")? as u32,
    })
}

#[async_trait]
impl MemoryProvider for SqliteMemoryProvider {
    // ───────────── Reads ─────────────

    async fn retrieve(&self, _query: MemoryQuery) -> MemoryResult<Vec<MemoryChunk>> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::retrieve — Phase 1 work",
        ))
    }

    async fn get(&self, chunk_id: &str) -> MemoryResult<Option<MemoryChunk>> {
        let conn = Arc::clone(&self.conn);
        let chunk_id = chunk_id.to_string();
        let res: Result<Option<MemoryChunk>, MemoryError> =
            tokio::task::spawn_blocking(move || -> Result<Option<MemoryChunk>, MemoryError> {
                let guard = conn.blocking_lock();
                let mut stmt = guard
                    .prepare(
                        "SELECT id, created_at, kind, tier, source, session_id,
                                project_root, caller_id, content, metadata,
                                importance, pinned, superseded_by,
                                embedding_model, embedding_dim
                         FROM memory_chunks WHERE id = ?",
                    )
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let row = stmt
                    .query_row(params![chunk_id], row_to_chunk)
                    .optional()
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                Ok(row)
            })
            .await
            .map_err(|e| MemoryError::Internal(format!("join: {e}")))?;
        res
    }

    async fn get_session(&self, session_id: &str) -> MemoryResult<Option<MemorySession>> {
        let conn = Arc::clone(&self.conn);
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<MemorySession>, MemoryError> {
            let guard = conn.blocking_lock();
            let mut stmt = guard
                .prepare(
                    "SELECT id, started_at, ended_at, caller_id, title, summary,
                            outcome, metadata
                     FROM memory_sessions WHERE id = ?",
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let row = stmt
                .query_row(params![session_id], |r| {
                    let metadata_str: String = r.get("metadata")?;
                    let metadata: serde_json::Value =
                        serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
                    Ok(MemorySession {
                        id: r.get("id")?,
                        started_at: ms_to_dt(r.get::<_, i64>("started_at")?),
                        ended_at: r.get::<_, Option<i64>>("ended_at")?.map(ms_to_dt),
                        caller_id: r.get("caller_id")?,
                        title: r.get("title")?,
                        summary: r.get("summary")?,
                        outcome: str_to_outcome(&r.get::<_, String>("outcome")?)
                            .unwrap_or(SessionOutcome::Aborted),
                        metadata,
                    })
                })
                .optional()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(row)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    async fn list_sessions(&self, _filter: SessionFilter) -> MemoryResult<Vec<MemorySession>> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::list_sessions — Phase 1 work",
        ))
    }

    // ───────────── Writes ─────────────

    async fn write(&self, new_chunk: NewMemoryChunk) -> MemoryResult<MemoryChunk> {
        if new_chunk.content.trim().is_empty() {
            return Err(MemoryError::InvalidArgument(
                "content must not be empty".into(),
            ));
        }
        let id = Uuid::now_v7().to_string();
        let created_at_ms = now_ms();
        let importance = new_chunk.importance.unwrap_or(0.5).clamp(0.0, 1.0);
        let embedder_dim = self.embedder.dim();
        let embedder_name = self.embedder.model_name().to_string();
        // Embed the content. If this fails we don't store the chunk —
        // chunks without vectors don't participate in retrieval.
        let embedding = self
            .embedder
            .embed(&new_chunk.content)
            .await
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
        if embedding.len() != embedder_dim {
            return Err(MemoryError::Internal(format!(
                "embedder produced dim {}, declared {}",
                embedding.len(),
                embedder_dim
            )));
        }

        let chunk = MemoryChunk {
            id: id.clone(),
            created_at: ms_to_dt(created_at_ms),
            kind: new_chunk.kind,
            tier: MemoryTier::Session,
            source: new_chunk.source,
            session_id: new_chunk.session_id.clone(),
            project_root: new_chunk.project_root.clone(),
            caller_id: new_chunk.caller_id.clone(),
            content: new_chunk.content.clone(),
            metadata: new_chunk.metadata.clone(),
            importance,
            pinned: new_chunk.pinned,
            superseded_by: None,
            embedding_model: embedder_name.clone(),
            embedding_dim: embedder_dim as u32,
        };

        let conn = Arc::clone(&self.conn);
        let chunk_for_blocking = chunk.clone();
        let embedding_clone = embedding.clone();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO memory_chunks(
                    id, created_at, kind, tier, source, session_id, project_root,
                    caller_id, content, metadata, importance, pinned, shareable,
                    superseded_by, embedding_model, embedding_dim
                ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    chunk_for_blocking.id,
                    created_at_ms,
                    kind_str(chunk_for_blocking.kind),
                    tier_str(chunk_for_blocking.tier),
                    source_str(chunk_for_blocking.source),
                    chunk_for_blocking.session_id,
                    chunk_for_blocking.project_root,
                    chunk_for_blocking.caller_id,
                    chunk_for_blocking.content,
                    serde_json::to_string(&chunk_for_blocking.metadata)
                        .unwrap_or_else(|_| "null".into()),
                    chunk_for_blocking.importance as f64,
                    if chunk_for_blocking.pinned { 1 } else { 0 },
                    if new_chunk.shareable { 1 } else { 0 },
                    Option::<String>::None,
                    chunk_for_blocking.embedding_model,
                    chunk_for_blocking.embedding_dim as i64,
                ],
            )
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
            // memory_vec insert. sqlite-vec accepts vectors as JSON-array
            // text or as a packed BLOB; JSON is simpler and the conversion
            // cost is irrelevant for v1.
            let v_json = serde_json::to_string(&embedding_clone)
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO memory_vec(chunk_id, embedding) VALUES (?, ?)",
                params![chunk_for_blocking.id, v_json],
            )
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))??;

        Ok(chunk)
    }

    async fn write_batch(&self, chunks: Vec<NewMemoryChunk>) -> MemoryResult<Vec<MemoryChunk>> {
        let mut out = Vec::with_capacity(chunks.len());
        for nc in chunks {
            out.push(self.write(nc).await?);
        }
        Ok(out)
    }

    async fn open_session(&self, init: NewMemorySession) -> MemoryResult<MemorySession> {
        let session = MemorySession {
            id: Uuid::now_v7().to_string(),
            started_at: Utc::now(),
            ended_at: None,
            caller_id: init.caller_id.clone(),
            title: init.title.clone(),
            summary: None,
            outcome: SessionOutcome::Open,
            metadata: init.metadata.clone(),
        };
        let conn = Arc::clone(&self.conn);
        let s = session.clone();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            guard
                .execute(
                    "INSERT INTO memory_sessions(
                        id, started_at, ended_at, caller_id, title, summary,
                        outcome, metadata
                    ) VALUES (?,?,?,?,?,?,?,?)",
                    params![
                        s.id,
                        dt_to_ms(s.started_at),
                        Option::<i64>::None,
                        s.caller_id,
                        s.title,
                        Option::<String>::None,
                        outcome_str(s.outcome),
                        serde_json::to_string(&s.metadata).unwrap_or_else(|_| "{}".into()),
                    ],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))??;
        Ok(session)
    }

    async fn close_session(&self, session_id: &str, outcome: SessionOutcome) -> MemoryResult<()> {
        let resolved = match outcome {
            SessionOutcome::Open => SessionOutcome::Aborted,
            other => other,
        };
        let conn = Arc::clone(&self.conn);
        let sid = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            let n = guard
                .execute(
                    "UPDATE memory_sessions SET ended_at = ?, outcome = ? WHERE id = ?",
                    params![now_ms(), outcome_str(resolved), sid],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            if n == 0 {
                return Err(MemoryError::NotFound(format!("session {sid}")));
            }
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    // ───────────── Updates ─────────────

    async fn pin(&self, chunk_id: &str, pinned: bool) -> MemoryResult<()> {
        let conn = Arc::clone(&self.conn);
        let id = chunk_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            let n = guard
                .execute(
                    "UPDATE memory_chunks SET pinned = ? WHERE id = ?",
                    params![if pinned { 1 } else { 0 }, id],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            if n == 0 {
                return Err(MemoryError::NotFound(format!("chunk {id}")));
            }
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    async fn update_importance(&self, _chunk_id: &str, _importance: f32) -> MemoryResult<()> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::update_importance — Phase 1",
        ))
    }

    async fn supersede(&self, _old_id: &str, _new_id: &str) -> MemoryResult<()> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::supersede — Phase 1",
        ))
    }

    async fn record_access(
        &self,
        _chunk_id: &str,
        _retrieved_by: &str,
        _used: bool,
    ) -> MemoryResult<()> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::record_access — Phase 1",
        ))
    }

    // ───────────── Deletes ─────────────

    async fn delete(&self, chunk_id: &str, reason: EvictionReason) -> MemoryResult<()> {
        let conn = Arc::clone(&self.conn);
        let id = chunk_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let n = tx
                .execute("DELETE FROM memory_chunks WHERE id = ?", params![id])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            if n == 0 {
                return Err(MemoryError::NotFound(format!("chunk {id}")));
            }
            tx.execute("DELETE FROM memory_vec WHERE chunk_id = ?", params![id])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute(
                "INSERT INTO memory_eviction_log(ts, chunk_id, reason, metadata)
                 VALUES (?,?,?, '{}')",
                params![now_ms(), id, eviction_reason_str(reason)],
            )
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    async fn delete_matching(
        &self,
        _predicate: MemoryPredicate,
        _reason: EvictionReason,
    ) -> MemoryResult<usize> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::delete_matching — Phase 1",
        ))
    }

    async fn purge_all(&self) -> MemoryResult<PurgeReport> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<PurgeReport, MemoryError> {
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let chunks: i64 = tx
                .query_row("SELECT COUNT(*) FROM memory_chunks", [], |r| r.get(0))
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let sessions: i64 = tx
                .query_row("SELECT COUNT(*) FROM memory_sessions", [], |r| r.get(0))
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let access: i64 = tx
                .query_row("SELECT COUNT(*) FROM memory_access_log", [], |r| r.get(0))
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let evictions: i64 = tx
                .query_row("SELECT COUNT(*) FROM memory_eviction_log", [], |r| r.get(0))
                .map_err(|e| MemoryError::Storage(e.to_string()))?;

            tx.execute("DELETE FROM memory_chunks", [])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute("DELETE FROM memory_vec", [])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute("DELETE FROM memory_sessions", [])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute("DELETE FROM memory_access_log", [])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute("DELETE FROM memory_eviction_log", [])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.execute("DELETE FROM memory_summary_members", [])
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;

            Ok(PurgeReport {
                chunks_deleted: chunks as usize,
                sessions_deleted: sessions as usize,
                access_log_deleted: access as usize,
                eviction_log_deleted: evictions as usize,
            })
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    // ───────────── Summarization ─────────────

    async fn summarize_session(&self, _session_id: &str) -> MemoryResult<MemoryChunk> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::summarize_session — Phase 3",
        ))
    }
    async fn rollup_day(&self, _date: NaiveDate) -> MemoryResult<Vec<MemoryChunk>> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::rollup_day — Phase 3",
        ))
    }
    async fn rollup_rule_week(
        &self,
        _rule_id: &str,
        _week_start: NaiveDate,
    ) -> MemoryResult<MemoryChunk> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::rollup_rule_week — Phase 3",
        ))
    }

    // ───────────── Maintenance ─────────────

    async fn run_aging_sweep(&self) -> MemoryResult<AgingReport> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::run_aging_sweep — Phase 3",
        ))
    }
    async fn re_embed_all(&self, _target_model: &str) -> MemoryResult<ReEmbedReport> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::re_embed_all — Phase 4",
        ))
    }
    async fn export(&self, _filter: ExportFilter) -> MemoryResult<ExportBundle> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::export — Phase 4",
        ))
    }

    async fn stats(&self) -> MemoryResult<MemoryStats> {
        let conn = Arc::clone(&self.conn);
        let model = self.embedder.model_name().to_string();
        tokio::task::spawn_blocking(move || -> Result<MemoryStats, MemoryError> {
            let guard = conn.blocking_lock();
            let total: i64 = guard
                .query_row("SELECT COUNT(*) FROM memory_chunks", [], |r| r.get(0))
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let session_tier: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_chunks WHERE tier = 'session'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let lt_tier: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_chunks WHERE tier = 'long_term'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let total_sessions: i64 = guard
                .query_row("SELECT COUNT(*) FROM memory_sessions", [], |r| r.get(0))
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let open: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_sessions WHERE outcome = 'open'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(MemoryStats {
                total_chunks: total as usize,
                session_chunks: session_tier as usize,
                long_term_chunks: lt_tier as usize,
                total_sessions: total_sessions as usize,
                open_sessions: open as usize,
                db_bytes: 0, // computed by `cellar doctor` separately
                embedding_model: Some(model),
            })
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }
}

fn eviction_reason_str(r: EvictionReason) -> &'static str {
    match r {
        EvictionReason::UserDelete => "user_delete",
        EvictionReason::Aging => "aging",
        EvictionReason::LowImportance => "low_importance",
        EvictionReason::RedactRule => "redact_rule",
        EvictionReason::StorageCap => "storage_cap",
        EvictionReason::PurgeAll => "purge_all",
    }
}

// Silence unused-import warnings when `record_access` etc. aren't wired.
#[allow(dead_code)]
fn _unused_imports_anchor(_: AccessEntry, _: EvictionEntry, _: CallerScope) {}
