//! `SqliteMemoryProvider` — the SQLite + vector backing storage.
//!
//! Implements the full [`cel_memory::MemoryProvider`] surface:
//!
//! - `open` loads sqlite-vec, runs migrations, holds the connection.
//! - `write` / `get` / `stats` / `purge_all` plus session lifecycle.
//! - Hybrid (vector + FTS + recency) `retrieve`, fronted by a TTL+LRU cache.
//! - `summarize_session` / `rollup_day` / `rollup_rule_week` via an injected
//!   [`cel_memory::Summarizer`] (these return `Err(NotImplemented)` only when
//!   no summarizer is attached).
//! - `run_aging_sweep` and `export`.
//! - `re_embed_all` is the one method still returning `Err(NotImplemented)`.
//!
//! The provider is `Send + Sync` and lives behind an `Arc<dyn MemoryProvider>` —
//! the same surface as [`BasicMemoryProvider`], so swapping it in is one line.
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

use crate::cache::RetrieveCache;
use crate::embedder::Embedder;
use crate::error::SqliteMemoryError;
use crate::migrations;

/// Default capacity of the per-provider retrieve cache. A "heavy" session
/// is ~30 retrievals/min; 256 buys ~8 minutes of distinct queries before
/// LRU eviction. Tuned for the embedded agent + NL compiler hot path.
const RETRIEVE_CACHE_CAPACITY: usize = 256;
/// Default TTL of cached retrieve results. Long enough to absorb a user
/// turn cluster, short enough that a write 30s ago will be visible to the
/// next matching read even if the eager `clear` was missed.
const RETRIEVE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// SQLite-backed [`MemoryProvider`].
pub struct SqliteMemoryProvider {
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<dyn Embedder>,
    /// Optional pre-write hook (memory_write_attempted governance seam).
    /// When unset, every write proceeds verbatim. When set, the provider
    /// consults the hook before persisting each chunk; on `Redact`, the
    /// chunk is dropped and a redaction record is logged.
    write_hook: Option<Arc<dyn cel_memory::MemoryWriteHook>>,
    /// Optional summarizer used by [`MemoryProvider::summarize_session`]
    /// and the rollup methods. When unset, those methods fall through to
    /// [`MemoryError::NotImplemented`] — preserving the contract for daemons
    /// that don't enable summarization. Attach one via [`Self::with_summarizer`].
    summarizer: Option<Arc<dyn cel_memory::Summarizer>>,
    /// Small TTL+LRU cache for [`MemoryProvider::retrieve`] results. See
    /// `crate::cache` for the contract.
    retrieve_cache: Arc<RetrieveCache<Vec<MemoryChunk>>>,
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
            write_hook: None,
            summarizer: None,
            retrieve_cache: Arc::new(RetrieveCache::new(
                RETRIEVE_CACHE_CAPACITY,
                RETRIEVE_CACHE_TTL,
            )),
        })
    }

    /// Attach a [`MemoryWriteHook`](cel_memory::MemoryWriteHook) consulted
    /// before every write. The daemon wires this to the rule matcher so
    /// `redact_memory`-style rules can suppress writes that mention
    /// sensitive content.
    pub fn with_write_hook(mut self, hook: Arc<dyn cel_memory::MemoryWriteHook>) -> Self {
        self.write_hook = Some(hook);
        self
    }

    /// Attach a [`Summarizer`](cel_memory::Summarizer) used by
    /// [`MemoryProvider::summarize_session`], `rollup_day`, and
    /// `rollup_rule_week`. Daemons pass a concrete summarizer (a downstream
    /// [`cel_memory::Summarizer`] impl); tests pass a
    /// [`cel_memory::MockSummarizer`].
    ///
    /// Calling [`MemoryProvider::summarize_session`] without first
    /// attaching a summarizer returns
    /// [`cel_memory::MemoryError::NotImplemented`] — preserving the v1
    /// contract for daemons that opt out of summarization.
    pub fn with_summarizer(mut self, summarizer: Arc<dyn cel_memory::Summarizer>) -> Self {
        self.summarizer = Some(summarizer);
        self
    }

    /// Test-only accessor for the underlying connection Arc, used by the
    /// smoke tests to backdate `created_at` for aging-sweep tests. Marked
    /// `#[doc(hidden)]` and behind `cfg(any(test, feature = "test-support"))`
    /// so production code can't grab the lock and bypass the provider's
    /// transactional guarantees.
    #[doc(hidden)]
    pub fn conn_for_test(&self) -> Arc<tokio::sync::Mutex<rusqlite::Connection>> {
        Arc::clone(&self.conn)
    }

    /// Drop every cached retrieve result. Called by every mutator on the
    /// provider so reads following a write never observe stale rankings.
    fn invalidate_retrieve_cache(&self) {
        self.retrieve_cache.clear();
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
            write_hook: None,
            summarizer: None,
            retrieve_cache: Arc::new(RetrieveCache::new(
                RETRIEVE_CACHE_CAPACITY,
                RETRIEVE_CACHE_TTL,
            )),
        })
    }

    /// Fetch every chunk attached to a session, ordered oldest-first.
    ///
    /// Used by [`MemoryProvider::summarize_session`]; kept as a private
    /// helper rather than a trait method because the only call site
    /// today is summarization and the `MemoryQuery` surface would force
    /// caller-id and text constraints that don't apply here. Promote to
    /// the trait if a second caller appears.
    ///
    /// Excludes existing `JobSummary` chunks for the same session so
    /// re-summarization doesn't snowball — each call re-summarizes the
    /// raw history, not the prior summary.
    async fn fetch_session_chunks(&self, session_id: &str) -> MemoryResult<Vec<MemoryChunk>> {
        let conn = Arc::clone(&self.conn);
        let sid = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<MemoryChunk>, MemoryError> {
            let guard = conn.blocking_lock();
            let mut stmt = guard
                .prepare(
                    "SELECT id, created_at, kind, tier, source, session_id,
                                project_root, caller_id, content, metadata,
                                importance, pinned, shareable, superseded_by,
                                embedding_model, embedding_dim
                         FROM memory_chunks
                         WHERE session_id = ?
                           AND kind != 'job_summary'
                         ORDER BY created_at ASC",
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let rows = stmt
                .query_map(params![sid], row_to_chunk)
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    /// Fetch every chunk whose `created_at` falls within the UTC date
    /// `date` (00:00:00 inclusive → next day 00:00:00 exclusive). Excludes
    /// `Rollup` chunks so re-running the daily rollup doesn't snowball.
    /// Ordered oldest-first so the summarizer sees the day as it
    /// unfolded.
    async fn fetch_day_chunks(&self, date: NaiveDate) -> MemoryResult<Vec<MemoryChunk>> {
        let start = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| MemoryError::InvalidArgument(format!("invalid date: {date}")))?
            .and_utc()
            .timestamp_millis();
        let end = date
            .succ_opt()
            .ok_or_else(|| MemoryError::InvalidArgument(format!("date overflow: {date}")))?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| MemoryError::InvalidArgument(format!("invalid date: {date}")))?
            .and_utc()
            .timestamp_millis();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<Vec<MemoryChunk>, MemoryError> {
            let guard = conn.blocking_lock();
            let mut stmt = guard
                .prepare(
                    "SELECT id, created_at, kind, tier, source, session_id,
                                project_root, caller_id, content, metadata,
                                importance, pinned, superseded_by,
                                embedding_model, embedding_dim
                         FROM memory_chunks
                         WHERE created_at >= ?
                           AND created_at < ?
                           AND kind != 'rollup'
                         ORDER BY created_at ASC",
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let rows = stmt
                .query_map(params![start, end], row_to_chunk)
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    /// Fetch every `Fire` chunk for `rule_id` whose `created_at` falls
    /// within the ISO week starting at `week_start` (a Monday, by
    /// convention; this code does not enforce that — it just sums the
    /// 7-day window). The rule id is stored on the chunk's
    /// `metadata.rule_id` field, written by the matcher consumer task.
    /// Excludes prior `Rollup` chunks to avoid snowball.
    async fn fetch_rule_week_chunks(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
    ) -> MemoryResult<Vec<MemoryChunk>> {
        let start = week_start
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| MemoryError::InvalidArgument(format!("invalid date: {week_start}")))?
            .and_utc()
            .timestamp_millis();
        let end_date = week_start
            .checked_add_days(chrono::Days::new(7))
            .ok_or_else(|| MemoryError::InvalidArgument(format!("week overflow: {week_start}")))?;
        let end = end_date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| MemoryError::InvalidArgument(format!("invalid date: {end_date}")))?
            .and_utc()
            .timestamp_millis();
        let conn = Arc::clone(&self.conn);
        let rid = rule_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<MemoryChunk>, MemoryError> {
            let guard = conn.blocking_lock();
            let mut stmt = guard
                .prepare(
                    "SELECT id, created_at, kind, tier, source, session_id,
                                project_root, caller_id, content, metadata,
                                importance, pinned, superseded_by,
                                embedding_model, embedding_dim
                         FROM memory_chunks
                         WHERE kind = 'fire'
                           AND created_at >= ?
                           AND created_at < ?
                           AND json_extract(metadata, '$.rule_id') = ?
                         ORDER BY created_at ASC",
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let rows = stmt
                .query_map(params![start, end, rid], row_to_chunk)
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    /// True if a `Rollup` chunk already exists for `date` (matched via
    /// `metadata.rollup_date = '<YYYY-MM-DD>'`). Used to short-circuit
    /// the day-rollup cron pass when force=false.
    async fn day_rollup_exists(&self, date: NaiveDate) -> MemoryResult<bool> {
        let date_s = date.to_string();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<bool, MemoryError> {
            let guard = conn.blocking_lock();
            let count: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_chunks
                         WHERE kind = 'rollup'
                           AND json_extract(metadata, '$.rollup_date') = ?",
                    params![date_s],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(count > 0)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    /// True if a `Rollup` chunk already exists for (`rule_id`,
    /// `week_start`). Matched via `metadata.rollup_rule_id` +
    /// `metadata.rollup_week_start`.
    async fn rule_week_rollup_exists(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
    ) -> MemoryResult<bool> {
        let week_s = week_start.to_string();
        let rid = rule_id.to_string();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<bool, MemoryError> {
            let guard = conn.blocking_lock();
            let count: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_chunks
                         WHERE kind = 'rollup'
                           AND json_extract(metadata, '$.rollup_rule_id') = ?
                           AND json_extract(metadata, '$.rollup_week_start') = ?",
                    params![rid, week_s],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(count > 0)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    /// Insert one row per (rollup_id, member_id) into
    /// `memory_summary_members`. INSERT OR IGNORE so a re-run is
    /// idempotent on the link table.
    async fn link_rollup_members(
        &self,
        rollup_id: &str,
        member_ids: &[String],
    ) -> MemoryResult<()> {
        let rid = rollup_id.to_string();
        let members: Vec<String> = member_ids.to_vec();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            for mid in &members {
                tx.execute(
                    "INSERT OR IGNORE INTO memory_summary_members(rollup_id, member_id)
                         VALUES (?, ?)",
                    params![rid, mid],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            }
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    /// Shared implementation behind both [`MemoryProvider::rollup_day`]
    /// and [`MemoryProvider::rollup_day_forced`].
    ///
    /// Groups the day's chunks (UTC) and produces a single `Rollup` chunk
    /// per call. When `force=false`, a no-op `Ok(vec![])` is returned if
    /// a rollup already exists for `date`. When `force=true`, a fresh
    /// rollup is always produced.
    ///
    /// Returns the summary as a `NotImplemented` error if no summarizer
    /// has been attached (preserving the v1 contract for daemons that
    /// opt out of summarization). Returns `Ok(vec![])` if the day has no
    /// non-rollup chunks — there's nothing to summarise and the cron
    /// sweeper should treat this as a successful no-op.
    async fn rollup_day_inner(
        &self,
        date: NaiveDate,
        force: bool,
    ) -> MemoryResult<Vec<MemoryChunk>> {
        let summarizer = self.summarizer.clone().ok_or(MemoryError::NotImplemented(
            "SqliteMemoryProvider::rollup_day — no summarizer attached \
             (call `with_summarizer` first)",
        ))?;

        if !force && self.day_rollup_exists(date).await? {
            tracing::debug!(date = %date, "rollup_day: existing rollup found, skipping");
            return Ok(Vec::new());
        }

        let members = self.fetch_day_chunks(date).await?;
        if members.is_empty() {
            tracing::debug!(date = %date, "rollup_day: no chunks to summarise, no-op");
            return Ok(Vec::new());
        }
        let member_ids: Vec<String> = members.iter().map(|c| c.id.clone()).collect();

        let kind_label = format!("day {date}");
        let ctx = cel_memory::SummaryContext {
            kind_label: Some(kind_label.clone()),
            note: Some(format!(
                "Daily rollup for {date} ({} chunks)",
                members.len()
            )),
            max_words: None,
        };
        let summary_text = summarizer
            .summarize(&members, &ctx)
            .await
            .map_err(|e| match e {
                cel_memory::SummarizerError::NoInput => MemoryError::InvalidArgument(
                    "summarizer received no input despite day having chunks".into(),
                ),
                other => MemoryError::Provider(format!(
                    "summarizer {} failed: {other}",
                    summarizer.name()
                )),
            })?;

        // Pick a representative caller_id for the rollup. We use "system"
        // because daily rollups span every caller; the rollup is daemon-
        // synthesised, not attributable to one upstream client.
        let new_chunk = NewMemoryChunk {
            kind: ChunkKind::Rollup,
            source: ChunkSource::System,
            session_id: None,
            project_root: None,
            caller_id: "system".to_string(),
            content: summary_text,
            metadata: serde_json::json!({
                "rollup_kind": "day",
                "rollup_date": date.to_string(),
                "member_count": member_ids.len(),
                "summarizer": summarizer.name(),
            }),
            importance: None,
            shareable: false,
            pinned: false,
        };
        let written = self.write(new_chunk).await?;
        self.link_rollup_members(&written.id, &member_ids).await?;
        Ok(vec![written])
    }

    /// Shared implementation behind both
    /// [`MemoryProvider::rollup_rule_week`] and
    /// [`MemoryProvider::rollup_rule_week_forced`].
    ///
    /// Groups all `Fire` chunks for `rule_id` in the 7-day window
    /// starting at `week_start` and produces one `Rollup` chunk. When
    /// `force=false`, returns `Err(InvalidArgument)` if a rollup exists
    /// (caller can decide whether to surface or suppress). Returns
    /// `Err(NotFound)` if no fires exist in the window — the cron sweeper
    /// should treat this as expected and skip the call.
    async fn rollup_rule_week_inner(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
        force: bool,
    ) -> MemoryResult<MemoryChunk> {
        let summarizer = self.summarizer.clone().ok_or(MemoryError::NotImplemented(
            "SqliteMemoryProvider::rollup_rule_week — no summarizer attached \
             (call `with_summarizer` first)",
        ))?;

        if !force && self.rule_week_rollup_exists(rule_id, week_start).await? {
            return Err(MemoryError::InvalidArgument(format!(
                "rollup already exists for rule {rule_id} week {week_start}"
            )));
        }

        let members = self.fetch_rule_week_chunks(rule_id, week_start).await?;
        if members.is_empty() {
            return Err(MemoryError::NotFound(format!(
                "no fires for rule {rule_id} in week of {week_start}"
            )));
        }
        let member_ids: Vec<String> = members.iter().map(|c| c.id.clone()).collect();

        let kind_label = format!("week of {week_start} for rule {rule_id}");
        let ctx = cel_memory::SummaryContext {
            kind_label: Some(kind_label.clone()),
            note: Some(format!(
                "Weekly rollup for rule {rule_id} ({} fires)",
                members.len()
            )),
            max_words: None,
        };
        let summary_text = summarizer
            .summarize(&members, &ctx)
            .await
            .map_err(|e| match e {
                cel_memory::SummarizerError::NoInput => MemoryError::InvalidArgument(
                    "summarizer received no input despite rule-week having chunks".into(),
                ),
                other => MemoryError::Provider(format!(
                    "summarizer {} failed: {other}",
                    summarizer.name()
                )),
            })?;

        let new_chunk = NewMemoryChunk {
            kind: ChunkKind::Rollup,
            source: ChunkSource::System,
            session_id: None,
            project_root: None,
            caller_id: "system".to_string(),
            content: summary_text,
            metadata: serde_json::json!({
                "rollup_kind": "rule_week",
                "rollup_rule_id": rule_id,
                "rollup_week_start": week_start.to_string(),
                "member_count": member_ids.len(),
                "summarizer": summarizer.name(),
            }),
            importance: None,
            shareable: false,
            pinned: false,
        };
        let written = self.write(new_chunk).await?;
        self.link_rollup_members(&written.id, &member_ids).await?;
        Ok(written)
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Stable cache key for a [`MemoryQuery`]. We round-trip through
/// `serde_json` so every field — including `caller_id`, `profile`, all
/// filters, `k`, `include_rollups`, `min_importance` — contributes to the
/// hash. JSON is overkill for a key but the surface area is small and the
/// `Serialize` derive is already paid for.
fn cache_key_for_query(query: &MemoryQuery) -> Option<u64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let s = serde_json::to_string(query).ok()?;
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    Some(h.finish())
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
    // shareable column is `INTEGER NOT NULL DEFAULT 0` in 001_initial.sql.
    // Read tolerantly (missing column → false) so a hand-rolled SELECT that
    // forgets to include `shareable` doesn't fail the whole row decode.
    let shareable = row.get::<_, i64>("shareable").unwrap_or(0) != 0;
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
        shareable,
        superseded_by: row.get("superseded_by")?,
        embedding_model: row.get("embedding_model")?,
        embedding_dim: row.get::<_, i64>("embedding_dim")? as u32,
    })
}

#[async_trait]
impl MemoryProvider for SqliteMemoryProvider {
    // ───────────── Reads ─────────────

    async fn retrieve(&self, query: MemoryQuery) -> MemoryResult<Vec<MemoryChunk>> {
        // Hybrid retrieval — reciprocal-rank fusion of vector + FTS, decayed
        // by recency:
        //   score = w_vec*rrf(rank_vec) + w_fts*rrf(rank_fts)
        //         + w_rec*exp(-Δt/half_life)
        //
        // Three sub-retrievals run in one connection (we're already
        // serialising through the Mutex anyway; parallelism would only
        // help if we had a connection pool):
        //   1. sqlite-vec k-NN against the query embedding
        //   2. FTS5 BM25 against the query text
        //   3. recency-only pass (newest-first within the filter window)
        //
        // The three rankings get fused via Reciprocal Rank Fusion (RRF)
        // with profile-driven weights. Filters (kind / scope / time /
        // project_root) are applied at SQL-time inside the candidate
        // queries; min_importance + include_rollups are applied
        // post-fusion since they're metadata signals, not ranking ones.
        if query.text.trim().is_empty() {
            return Err(MemoryError::InvalidArgument(
                "query.text must not be empty".into(),
            ));
        }

        // Hot-path cache. The cache is invalidated eagerly by every
        // mutator (`write`, `delete`, …), with a 30 s TTL as a backstop.
        let cache_key = cache_key_for_query(&query);
        if let Some(key) = cache_key {
            if let Some(hit) = self.retrieve_cache.get(key) {
                return Ok(hit);
            }
        }
        let k = query.k.max(1);
        // We over-fetch each sub-retrieval (3*k) so RRF has enough
        // candidates to merge without dropping near-misses.
        let candidate_k = (3 * k).max(16);

        let (w_vec, w_fts, w_rec, half_life_secs) = retrieval_weights(query.profile);

        let q_embedding = self
            .embedder
            .embed(&query.text)
            .await
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
        let q_text = query.text.clone();
        let conn = Arc::clone(&self.conn);

        let compute =
            tokio::task::spawn_blocking(move || -> Result<Vec<MemoryChunk>, MemoryError> {
                let guard = conn.blocking_lock();

                // Sub-retrieval 1: vector k-NN. sqlite-vec accepts the embedding
                // as a JSON-array string. We use `vec_distance_l2` ordering
                // (default for the `MATCH` operator). The WHERE filter is
                // applied via a join because vec0 virtual tables don't honor
                // arbitrary predicates on adjoining columns at MATCH time.
                let v_json = serde_json::to_string(&q_embedding)
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let vec_ids: Vec<String> = {
                    let sql = "SELECT v.chunk_id FROM memory_vec v
                           WHERE v.embedding MATCH ?
                             AND k = ?
                           ORDER BY distance";
                    let mut stmt = guard
                        .prepare(sql)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let rows = stmt
                        .query_map(params![v_json, candidate_k as i64], |r| {
                            r.get::<_, String>(0)
                        })
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let mut out = Vec::new();
                    for r in rows {
                        out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                    }
                    out
                };

                // Sub-retrieval 2: FTS5 BM25 over the same query text. FTS5
                // ranks lower-distance as higher relevance, hence ASC by rank.
                let fts_ids: Vec<String> = {
                    let sql = "SELECT chunk_id FROM memory_fts
                           WHERE memory_fts MATCH ?
                           ORDER BY rank
                           LIMIT ?";
                    let mut stmt = guard
                        .prepare(sql)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let rows = stmt
                        .query_map(
                            params![fts_query_escape(&q_text), candidate_k as i64],
                            |r| r.get::<_, String>(0),
                        )
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let mut out = Vec::new();
                    for r in rows {
                        // FTS may return no-match if the tokenizer drops the
                        // whole query (e.g., punctuation-only). Surface as
                        // empty list rather than panic.
                        match r {
                            Ok(id) => out.push(id),
                            Err(_) => break,
                        }
                    }
                    out
                };

                // Sub-retrieval 3: recency. Newest-first within the filter
                // window. This is the same candidate set we'll join the
                // chunk rows from.
                let recency_ids: Vec<String> = {
                    let sql = "SELECT id FROM memory_chunks
                           ORDER BY created_at DESC LIMIT ?";
                    let mut stmt = guard
                        .prepare(sql)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let rows = stmt
                        .query_map(params![candidate_k as i64], |r| r.get::<_, String>(0))
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let mut out = Vec::new();
                    for r in rows {
                        out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                    }
                    out
                };

                // RRF fusion. The constant 60 is the standard RRF prior.
                const RRF_K: f32 = 60.0;
                let mut scores: std::collections::HashMap<String, f32> =
                    std::collections::HashMap::new();
                for (rank, id) in vec_ids.iter().enumerate() {
                    *scores.entry(id.clone()).or_insert(0.0) += w_vec / (RRF_K + (rank + 1) as f32);
                }
                for (rank, id) in fts_ids.iter().enumerate() {
                    *scores.entry(id.clone()).or_insert(0.0) += w_fts / (RRF_K + (rank + 1) as f32);
                }
                // Recency is keyed off the chunk's `created_at` directly.
                // Skip the score contribution for chunks we don't have a row
                // for yet (rare race: scored vector for a deleted chunk).
                let now_ms_val = now_ms();
                let recency_rows: std::collections::HashMap<String, i64> = {
                    if recency_ids.is_empty() {
                        Default::default()
                    } else {
                        let placeholders = vec!["?"; recency_ids.len()].join(",");
                        let sql = format!(
                            "SELECT id, created_at FROM memory_chunks WHERE id IN ({placeholders})"
                        );
                        let p: Vec<&dyn rusqlite::ToSql> = recency_ids
                            .iter()
                            .map(|s| s as &dyn rusqlite::ToSql)
                            .collect();
                        let mut stmt = guard
                            .prepare(&sql)
                            .map_err(|e| MemoryError::Storage(e.to_string()))?;
                        let rows = stmt
                            .query_map(p.as_slice(), |r| {
                                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                            })
                            .map_err(|e| MemoryError::Storage(e.to_string()))?;
                        let mut map = std::collections::HashMap::new();
                        for r in rows {
                            let (id, t) = r.map_err(|e| MemoryError::Storage(e.to_string()))?;
                            map.insert(id, t);
                        }
                        map
                    }
                };
                for (id, created_ms) in &recency_rows {
                    let dt_secs = ((now_ms_val - created_ms).max(0) / 1000) as f32;
                    let recency = (-dt_secs / half_life_secs).exp();
                    *scores.entry(id.clone()).or_insert(0.0) += w_rec * recency;
                }

                // Materialise candidate chunks from the union of ID sets.
                let mut id_set: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                id_set.extend(vec_ids);
                id_set.extend(fts_ids);
                id_set.extend(recency_ids);
                if id_set.is_empty() {
                    return Ok(Vec::new());
                }
                let placeholders = vec!["?"; id_set.len()].join(",");
                let select_sql = format!(
                    "SELECT id, created_at, kind, tier, source, session_id,
                        project_root, caller_id, content, metadata,
                        importance, pinned, shareable, superseded_by,
                        embedding_model, embedding_dim
                 FROM memory_chunks WHERE id IN ({placeholders})"
                );
                let ids_vec: Vec<String> = id_set.into_iter().collect();
                let candidates: Vec<MemoryChunk> = {
                    let p: Vec<&dyn rusqlite::ToSql> =
                        ids_vec.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                    let mut stmt = guard
                        .prepare(&select_sql)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let rows = stmt
                        .query_map(p.as_slice(), row_to_chunk)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let mut out = Vec::new();
                    for r in rows {
                        out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                    }
                    out
                };

                // Apply non-ranking filters now (kind, session, scope, time,
                // project_root, min_importance, include_rollups).
                let mut filtered: Vec<MemoryChunk> = candidates
                    .into_iter()
                    .filter(|c| chunk_matches_query(c, &query))
                    .collect();

                // Sort by fused score descending; truncate to k.
                filtered.sort_by(|a, b| {
                    let sa = scores.get(&a.id).copied().unwrap_or(0.0);
                    let sb = scores.get(&b.id).copied().unwrap_or(0.0);
                    sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
                });
                filtered.truncate(k);
                Ok(filtered)
            });
        let result = compute
            .await
            .map_err(|e| MemoryError::Internal(format!("join: {e}")))??;

        // Populate the cache after a successful computation. Errors are
        // never cached — they're cheap to recompute and stale errors would
        // mask transient lock contention.
        if let Some(key) = cache_key {
            self.retrieve_cache.insert(key, result.clone());
        }
        Ok(result)
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
                                importance, pinned, shareable, superseded_by,
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

    async fn list_sessions(&self, filter: SessionFilter) -> MemoryResult<Vec<MemorySession>> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<Vec<MemorySession>, MemoryError> {
            let guard = conn.blocking_lock();
            // Build a parameterized query. Each Option<...> on the filter
            // adds a single `AND col op ?` clause. The matcher takes
            // owned params so we don't fight rusqlite's lifetime rules.
            let mut sql = String::from(
                "SELECT id, started_at, ended_at, caller_id, title, summary,
                        outcome, metadata
                 FROM memory_sessions WHERE 1=1",
            );
            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            if let Some(c) = &filter.caller_id {
                sql.push_str(" AND caller_id = ?");
                params_vec.push(Box::new(c.clone()));
            }
            if let Some(o) = filter.outcome {
                sql.push_str(" AND outcome = ?");
                params_vec.push(Box::new(outcome_str(o).to_string()));
            } else if filter.open_only {
                sql.push_str(" AND outcome = ?");
                params_vec.push(Box::new("open".to_string()));
            }
            if let Some(since) = filter.since {
                sql.push_str(" AND started_at >= ?");
                params_vec.push(Box::new(dt_to_ms(since)));
            }
            if let Some(until) = filter.until {
                sql.push_str(" AND started_at <= ?");
                params_vec.push(Box::new(dt_to_ms(until)));
            }
            sql.push_str(" ORDER BY started_at DESC");
            if let Some(n) = filter.limit {
                sql.push_str(" LIMIT ?");
                params_vec.push(Box::new(n as i64));
            }

            let mut stmt = guard
                .prepare(&sql)
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let p: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
            let rows = stmt
                .query_map(p.as_slice(), |r| {
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
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| MemoryError::Storage(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    // ───────────── Writes ─────────────

    async fn write(&self, new_chunk: NewMemoryChunk) -> MemoryResult<MemoryChunk> {
        if new_chunk.content.trim().is_empty() {
            return Err(MemoryError::InvalidArgument(
                "content must not be empty".into(),
            ));
        }
        // Eager invalidation. Combined with the conn-mutex serialising
        // every read against this write, any retrieve started after this
        // point observes the post-write state. See `cache.rs` docs.
        self.invalidate_retrieve_cache();

        // Pre-write hook: rule-matcher seam. On Redact, return a synthetic
        // chunk without persisting (and without embedding — that's the
        // expensive part). The caller sees an Ok-with-redacted-marker
        // chunk; the SQL store stays untouched.
        if let Some(hook) = &self.write_hook {
            match hook.before_write(&new_chunk).await? {
                cel_memory::WriteDecision::Allow => {}
                cel_memory::WriteDecision::Redact { reason } => {
                    return Ok(MemoryChunk {
                        id: Uuid::now_v7().to_string(),
                        created_at: Utc::now(),
                        kind: new_chunk.kind,
                        tier: MemoryTier::Session,
                        source: new_chunk.source,
                        session_id: new_chunk.session_id,
                        project_root: new_chunk.project_root,
                        caller_id: new_chunk.caller_id,
                        content: format!("<redacted: {reason}>"),
                        metadata: serde_json::json!({"redacted": true, "reason": reason}),
                        importance: 0.0,
                        pinned: false,
                        shareable: false,
                        superseded_by: None,
                        embedding_model: "none".into(),
                        embedding_dim: 0,
                    });
                }
            }
        }

        let id = Uuid::now_v7().to_string();
        let created_at_ms = now_ms();
        // Score via the shared heuristic (cel_memory::importance::score).
        // If the caller supplied an explicit value, it's honored after clamp;
        // otherwise the kind + metadata drive the score.
        let importance = cel_memory::importance::score(&new_chunk);
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
            shareable: new_chunk.shareable,
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
        // Batched path: one embedding call, one transaction, one Mutex hold.
        // Single-chunk path delegates to `write` (which already runs the
        // single-call optimisation through the embedder; no point batching
        // a batch of one).
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        if chunks.len() == 1 {
            let nc = chunks.into_iter().next().expect("len == 1");
            return Ok(vec![self.write(nc).await?]);
        }
        self.invalidate_retrieve_cache();

        // Hook-present path: per-chunk evaluation is required so redacted
        // chunks don't get embedded. Fall back to sequential writes to
        // preserve input → output ordering without adding per-chunk
        // bookkeeping to the batched insert.
        if self.write_hook.is_some() {
            let mut out = Vec::with_capacity(chunks.len());
            for nc in chunks {
                out.push(self.write(nc).await?);
            }
            return Ok(out);
        }

        // Validate content non-empty up front so we don't half-embed a
        // batch and then fail mid-transaction.
        for (i, nc) in chunks.iter().enumerate() {
            if nc.content.trim().is_empty() {
                return Err(MemoryError::InvalidArgument(format!(
                    "chunks[{i}].content must not be empty"
                )));
            }
        }

        let embedder_dim = self.embedder.dim();
        let embedder_name = self.embedder.model_name().to_string();

        // One batched embedding call. The injected `Embedder` implementation
        // (fastembed, OpenAI, Voyage) provides the real batching;
        // `MockEmbedder` falls back to the trait's default impl which is
        // sequential but at least keeps the contract honest.
        let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
        let embeddings = self
            .embedder
            .embed_batch(&texts)
            .await
            .map_err(|e| MemoryError::Storage(e.to_string()))?;
        if embeddings.len() != chunks.len() {
            return Err(MemoryError::Internal(format!(
                "embedder returned {} vectors for {} inputs",
                embeddings.len(),
                chunks.len()
            )));
        }
        for (i, v) in embeddings.iter().enumerate() {
            if v.len() != embedder_dim {
                return Err(MemoryError::Internal(format!(
                    "embedder produced dim {} for chunk {i}, declared {}",
                    v.len(),
                    embedder_dim
                )));
            }
        }

        // Build all MemoryChunks now so the spawn_blocking payload owns
        // everything it needs.
        let now_ms_val = now_ms();
        let mut owned: Vec<(MemoryChunk, Vec<f32>, bool, String)> =
            Vec::with_capacity(chunks.len());
        for (nc, embedding) in chunks.into_iter().zip(embeddings) {
            let id = Uuid::now_v7().to_string();
            let importance = cel_memory::importance::score(&nc);
            let metadata_json =
                serde_json::to_string(&nc.metadata).unwrap_or_else(|_| "null".into());
            let mc = MemoryChunk {
                id,
                created_at: ms_to_dt(now_ms_val),
                kind: nc.kind,
                tier: MemoryTier::Session,
                source: nc.source,
                session_id: nc.session_id,
                project_root: nc.project_root,
                caller_id: nc.caller_id,
                content: nc.content,
                metadata: nc.metadata,
                importance,
                pinned: nc.pinned,
                shareable: nc.shareable,
                superseded_by: None,
                embedding_model: embedder_name.clone(),
                embedding_dim: embedder_dim as u32,
            };
            owned.push((mc, embedding, nc.shareable, metadata_json));
        }

        let conn = Arc::clone(&self.conn);
        let inserted =
            tokio::task::spawn_blocking(move || -> Result<Vec<MemoryChunk>, MemoryError> {
                let mut guard = conn.blocking_lock();
                let tx = guard
                    .transaction()
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let mut out = Vec::with_capacity(owned.len());
                for (mc, embedding, shareable, metadata_json) in &owned {
                    tx.execute(
                        "INSERT INTO memory_chunks(
                        id, created_at, kind, tier, source, session_id, project_root,
                        caller_id, content, metadata, importance, pinned, shareable,
                        superseded_by, embedding_model, embedding_dim
                    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                        params![
                            mc.id,
                            now_ms_val,
                            kind_str(mc.kind),
                            tier_str(mc.tier),
                            source_str(mc.source),
                            mc.session_id,
                            mc.project_root,
                            mc.caller_id,
                            mc.content,
                            metadata_json,
                            mc.importance as f64,
                            if mc.pinned { 1 } else { 0 },
                            if *shareable { 1 } else { 0 },
                            Option::<String>::None,
                            mc.embedding_model,
                            mc.embedding_dim as i64,
                        ],
                    )
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let v_json = serde_json::to_string(embedding)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    tx.execute(
                        "INSERT INTO memory_vec(chunk_id, embedding) VALUES (?, ?)",
                        params![mc.id, v_json],
                    )
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    out.push(mc.clone());
                }
                tx.commit()
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                Ok(out)
            })
            .await
            .map_err(|e| MemoryError::Internal(format!("join: {e}")))??;

        Ok(inserted)
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

    async fn rename_session(&self, session_id: &str, title: &str) -> MemoryResult<()> {
        let conn = Arc::clone(&self.conn);
        let sid = session_id.to_string();
        let new_title = title.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            let n = guard
                .execute(
                    "UPDATE memory_sessions SET title = ? WHERE id = ?",
                    params![new_title, sid],
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
        self.invalidate_retrieve_cache();
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

    async fn update_importance(&self, chunk_id: &str, importance: f32) -> MemoryResult<()> {
        self.invalidate_retrieve_cache();
        let conn = Arc::clone(&self.conn);
        let id = chunk_id.to_string();
        let clamped = importance.clamp(0.0, 1.0) as f64;
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            let n = guard
                .execute(
                    "UPDATE memory_chunks SET importance = ? WHERE id = ?",
                    params![clamped, id],
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

    async fn supersede(&self, old_id: &str, new_id: &str) -> MemoryResult<()> {
        self.invalidate_retrieve_cache();
        let conn = Arc::clone(&self.conn);
        let old = old_id.to_string();
        let new = new_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            // Both chunks must exist. We don't enforce a foreign-key relation
            // at the SQL layer (memory_chunks.superseded_by is plain TEXT) so
            // verify by hand to keep the contract clean.
            let new_exists: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_chunks WHERE id = ?",
                    params![new],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            if new_exists == 0 {
                return Err(MemoryError::NotFound(format!("new chunk {new}")));
            }
            let n = guard
                .execute(
                    "UPDATE memory_chunks SET superseded_by = ? WHERE id = ?",
                    params![new, old],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            if n == 0 {
                return Err(MemoryError::NotFound(format!("old chunk {old}")));
            }
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    async fn record_access(
        &self,
        chunk_id: &str,
        retrieved_by: &str,
        used: bool,
    ) -> MemoryResult<()> {
        let conn = Arc::clone(&self.conn);
        let id = chunk_id.to_string();
        let by = retrieved_by.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            let exists: i64 = guard
                .query_row(
                    "SELECT COUNT(*) FROM memory_chunks WHERE id = ?",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            if exists == 0 {
                return Err(MemoryError::NotFound(format!("chunk {id}")));
            }
            guard
                .execute(
                    "INSERT INTO memory_access_log(ts, chunk_id, retrieved_by, query_hash, rank, used)
                     VALUES (?, ?, ?, '', 0, ?)",
                    params![now_ms(), id, by, if used { 1 } else { 0 }],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    // ───────────── Deletes ─────────────

    async fn delete(&self, chunk_id: &str, reason: EvictionReason) -> MemoryResult<()> {
        self.invalidate_retrieve_cache();
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
        predicate: MemoryPredicate,
        reason: EvictionReason,
    ) -> MemoryResult<usize> {
        // Footgun guard: an empty predicate would match every chunk; callers
        // who want "delete everything" must use `purge_all`. Mirrors
        // BasicMemoryProvider's behavior so the locked trait surface
        // is consistent across providers.
        if predicate.is_empty() {
            return Ok(0);
        }
        self.invalidate_retrieve_cache();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<usize, MemoryError> {
            let (where_clause, params_vec) = build_chunk_predicate(&predicate);
            let select_sql = format!("SELECT id FROM memory_chunks WHERE {}", where_clause);
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let ids: Vec<String> = {
                let p: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
                let mut stmt = tx
                    .prepare(&select_sql)
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map(p.as_slice(), |r| r.get::<_, String>(0))
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                }
                out
            };
            let now = now_ms();
            let reason_s = eviction_reason_str(reason);
            for id in &ids {
                tx.execute("DELETE FROM memory_chunks WHERE id = ?", params![id])
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                tx.execute("DELETE FROM memory_vec WHERE chunk_id = ?", params![id])
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                tx.execute(
                    "INSERT INTO memory_eviction_log(ts, chunk_id, reason, metadata)
                     VALUES (?, ?, ?, '{}')",
                    params![now, id, reason_s],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            }
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(ids.len())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }

    async fn purge_all(&self) -> MemoryResult<PurgeReport> {
        self.invalidate_retrieve_cache();
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

    async fn summarize_session(&self, session_id: &str) -> MemoryResult<MemoryChunk> {
        // Honest signal when the daemon hasn't wired a summarizer —
        // preserves the v1 contract so existing callers that don't
        // opt into summarization keep their `NotImplemented` branch.
        let summarizer = self.summarizer.clone().ok_or(MemoryError::NotImplemented(
            "SqliteMemoryProvider::summarize_session — no summarizer attached \
             (call `with_summarizer` first)",
        ))?;

        // 1. Confirm the session exists. Returns NotFound with a clear
        //    message so callers can distinguish "unknown id" from
        //    "summarizer failed".
        let session = self
            .get_session(session_id)
            .await?
            .ok_or_else(|| MemoryError::NotFound(format!("session {session_id}")))?;

        // 2. Pull the constituent chunks in chronological order —
        //    oldest-first feed so the model can read the conversation
        //    as it unfolded.
        let members = self.fetch_session_chunks(session_id).await?;
        if members.is_empty() {
            // No chunks to summarize — the model would hallucinate, and
            // the summary_members link table would be empty. Surface as
            // InvalidArgument so callers can decide whether to silently
            // skip or surface to the user.
            return Err(MemoryError::InvalidArgument(format!(
                "session {session_id} has no chunks to summarize"
            )));
        }
        let member_ids: Vec<String> = members.iter().map(|c| c.id.clone()).collect();

        // 3. Call the summarizer. NoInput should be impossible here
        //    given the check above, but we still translate honestly.
        let ctx = cel_memory::SummaryContext {
            kind_label: Some("session".into()),
            note: session
                .title
                .as_ref()
                .map(|t| format!("session title: {t}")),
            max_words: None,
        };
        let summary_text = summarizer
            .summarize(&members, &ctx)
            .await
            .map_err(|e| match e {
                cel_memory::SummarizerError::NoInput => MemoryError::InvalidArgument(
                    "summarizer received no input despite session having chunks".into(),
                ),
                other => MemoryError::Provider(format!(
                    "summarizer {} failed: {other}",
                    summarizer.name()
                )),
            })?;

        // 4. Persist the summary as a fresh JobSummary chunk via the
        //    public write path so importance scoring and the
        //    embedding pipeline both stay consistent with every other
        //    write. We use the session's caller_id so the summary
        //    inherits scope, and stamp metadata with the member ids
        //    and session id for cross-reference.
        let new_chunk = NewMemoryChunk {
            kind: ChunkKind::JobSummary,
            source: ChunkSource::Embedded,
            session_id: Some(session_id.to_string()),
            project_root: members.iter().find_map(|c| c.project_root.clone()),
            caller_id: session.caller_id.clone(),
            content: summary_text,
            metadata: serde_json::json!({
                "session_id": session_id,
                "member_count": member_ids.len(),
                "summarizer": summarizer.name(),
            }),
            // Honor the importance default (+0.2 for JobSummary off baseline)
            // by leaving `importance: None` — the importance scorer
            // applied inside `write` will land on 0.7.
            importance: None,
            shareable: false,
            pinned: false,
        };
        let written = self.write(new_chunk).await?;

        // 5. Link summary → members in memory_summary_members. One row
        //    per member. We use INSERT OR IGNORE so repeated calls (or
        //    a flaky retry) don't blow up on the composite primary
        //    key.
        let conn = Arc::clone(&self.conn);
        let summary_id = written.id.clone();
        let member_ids_for_insert = member_ids.clone();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            for mid in &member_ids_for_insert {
                tx.execute(
                    "INSERT OR IGNORE INTO memory_summary_members(rollup_id, member_id)
                         VALUES (?, ?)",
                    params![summary_id, mid],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            }
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))??;

        // 6. Backfill the session's `summary` column so the
        //    `MemorySession` record exposes the latest synthesis
        //    without a join.
        let conn = Arc::clone(&self.conn);
        let sid = session_id.to_string();
        let stored_summary = written.content.clone();
        tokio::task::spawn_blocking(move || -> Result<(), MemoryError> {
            let guard = conn.blocking_lock();
            guard
                .execute(
                    "UPDATE memory_sessions SET summary = ? WHERE id = ?",
                    params![stored_summary, sid],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))??;

        Ok(written)
    }

    async fn rollup_day(&self, date: NaiveDate) -> MemoryResult<Vec<MemoryChunk>> {
        self.rollup_day_inner(date, false).await
    }

    async fn rollup_day_forced(&self, date: NaiveDate) -> MemoryResult<Vec<MemoryChunk>> {
        self.rollup_day_inner(date, true).await
    }

    async fn rollup_rule_week(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
    ) -> MemoryResult<MemoryChunk> {
        self.rollup_rule_week_inner(rule_id, week_start, false)
            .await
    }

    async fn rollup_rule_week_forced(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
    ) -> MemoryResult<MemoryChunk> {
        self.rollup_rule_week_inner(rule_id, week_start, true).await
    }

    // ───────────── Maintenance ─────────────

    async fn run_aging_sweep(&self) -> MemoryResult<AgingReport> {
        // v1 sweep: delete non-pinned non-correction chunks older than
        // the retention horizon. Matches BasicMemoryProvider's heuristic
        // (30 days) so the trait surface stays consistent across providers.
        // A future revision can replace this with an importance-aware policy.
        const RETENTION_DAYS: i64 = 30;
        let cutoff_ms =
            (chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS)).timestamp_millis();
        self.invalidate_retrieve_cache();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<AgingReport, MemoryError> {
            let mut guard = conn.blocking_lock();
            let tx = guard
                .transaction()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let ids: Vec<String> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id FROM memory_chunks
                         WHERE pinned = 0
                           AND kind != 'correction'
                           AND created_at < ?",
                    )
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map(params![cutoff_ms], |r| r.get::<_, String>(0))
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                }
                out
            };
            let now = now_ms();
            let reason_s = eviction_reason_str(EvictionReason::Aging);
            for id in &ids {
                tx.execute("DELETE FROM memory_chunks WHERE id = ?", params![id])
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                tx.execute("DELETE FROM memory_vec WHERE chunk_id = ?", params![id])
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                tx.execute(
                    "INSERT INTO memory_eviction_log(ts, chunk_id, reason, metadata)
                     VALUES (?, ?, ?, '{}')",
                    params![now, id, reason_s],
                )
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            }
            tx.commit()
                .map_err(|e| MemoryError::Storage(e.to_string()))?;
            let deleted = ids.len();
            Ok(AgingReport {
                tier_promoted: 0, // session→long_term transition is Phase 3 work
                deleted,
                bytes_reclaimed: 0,
                deletions_by_reason: vec![(EvictionReason::Aging, deleted)],
            })
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
    }
    async fn re_embed_all(&self, _target_model: &str) -> MemoryResult<ReEmbedReport> {
        Err(MemoryError::NotImplemented(
            "SqliteMemoryProvider::re_embed_all — Phase 4",
        ))
    }
    async fn export(&self, filter: ExportFilter) -> MemoryResult<ExportBundle> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<ExportBundle, MemoryError> {
            let guard = conn.blocking_lock();

            // Chunks first; honor the optional predicate.
            let (where_clause, params_vec) = match &filter.predicate {
                Some(p) if !p.is_empty() => build_chunk_predicate(p),
                _ => ("1=1".to_string(), Vec::new()),
            };
            let select_sql = format!(
                "SELECT id, created_at, kind, tier, source, session_id,
                        project_root, caller_id, content, metadata,
                        importance, pinned, shareable, superseded_by,
                        embedding_model, embedding_dim
                 FROM memory_chunks WHERE {} ORDER BY created_at DESC",
                where_clause
            );
            let chunks: Vec<MemoryChunk> = {
                let p: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
                let mut stmt = guard
                    .prepare(&select_sql)
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map(p.as_slice(), row_to_chunk)
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                }
                out
            };

            // Sessions: only those referenced by the included chunks, and
            // only if include_sessions is set.
            let sessions = if filter.include_sessions {
                let session_ids: std::collections::HashSet<String> =
                    chunks.iter().filter_map(|c| c.session_id.clone()).collect();
                if session_ids.is_empty() {
                    Vec::new()
                } else {
                    let placeholders = vec!["?"; session_ids.len()].join(",");
                    let sql = format!(
                        "SELECT id, started_at, ended_at, caller_id, title, summary,
                                outcome, metadata
                         FROM memory_sessions WHERE id IN ({placeholders})"
                    );
                    let ids: Vec<String> = session_ids.into_iter().collect();
                    let p: Vec<&dyn rusqlite::ToSql> =
                        ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                    let mut stmt = guard
                        .prepare(&sql)
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let rows = stmt
                        .query_map(p.as_slice(), |r| {
                            let metadata_str: String = r.get("metadata")?;
                            let metadata: serde_json::Value = serde_json::from_str(&metadata_str)
                                .unwrap_or(serde_json::Value::Null);
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
                        .map_err(|e| MemoryError::Storage(e.to_string()))?;
                    let mut out = Vec::new();
                    for r in rows {
                        out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                    }
                    out
                }
            } else {
                Vec::new()
            };

            let evictions = if filter.include_eviction_log {
                let mut stmt = guard
                    .prepare(
                        "SELECT ts, chunk_id, reason, metadata
                         FROM memory_eviction_log ORDER BY ts DESC",
                    )
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map([], |r| {
                        let reason_s: String = r.get("reason")?;
                        let metadata_s: String = r.get("metadata")?;
                        let metadata: serde_json::Value =
                            serde_json::from_str(&metadata_s).unwrap_or(serde_json::Value::Null);
                        Ok(EvictionEntry {
                            ts: ms_to_dt(r.get::<_, i64>("ts")?),
                            chunk_id: r.get("chunk_id")?,
                            reason: str_to_eviction_reason(&reason_s)
                                .unwrap_or(EvictionReason::UserDelete),
                            metadata,
                        })
                    })
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                }
                out
            } else {
                Vec::new()
            };

            let accesses = if filter.include_access_log {
                let mut stmt = guard
                    .prepare(
                        "SELECT ts, chunk_id, retrieved_by, query_hash, rank, used
                         FROM memory_access_log ORDER BY ts DESC",
                    )
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(AccessEntry {
                            ts: ms_to_dt(r.get::<_, i64>("ts")?),
                            chunk_id: r.get("chunk_id")?,
                            retrieved_by: r.get("retrieved_by")?,
                            query_hash: r.get("query_hash")?,
                            rank: r.get::<_, i64>("rank")? as usize,
                            used: r.get::<_, i64>("used")? != 0,
                        })
                    })
                    .map_err(|e| MemoryError::Storage(e.to_string()))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(|e| MemoryError::Storage(e.to_string()))?);
                }
                out
            } else {
                Vec::new()
            };

            Ok(ExportBundle {
                chunks,
                sessions,
                evictions,
                accesses,
            })
        })
        .await
        .map_err(|e| MemoryError::Internal(format!("join: {e}")))?
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

/// Per-profile retrieval weights. Tuned defaults, not exposed via the trait
/// yet, so callers configure profiles via the `RetrievalProfile` enum only.
/// Returns `(w_vec, w_fts, w_rec, half_life_seconds)`.
fn retrieval_weights(profile: cel_memory::RetrievalProfile) -> (f32, f32, f32, f32) {
    use cel_memory::RetrievalProfile::*;
    match profile {
        // Embedded agent's per-turn retrieval — semantic-heavy, short
        // recency half-life (7 days).
        AgentChatTurn => (0.55, 0.30, 0.15, 7.0 * 86400.0),
        // Long-horizon job context — heavier weight on long_term + summaries.
        AgentDelegatedJob => (0.55, 0.30, 0.15, 30.0 * 86400.0),
        // NL compiler: similar prior rules. Keyword match dominates because
        // users are looking for "the rule that mentions ~/Workspace".
        NLCompilerSimilarRules => (0.40, 0.40, 0.20, 30.0 * 86400.0),
        // NL compiler: similar prior fires. Same balance, slightly longer
        // half-life because rule history compounds.
        NLCompilerSimilarFires => (0.40, 0.40, 0.20, 30.0 * 86400.0),
        // Audit / Activity tab — keyword-dominant, wide window.
        AuditTimeline => (0.30, 0.50, 0.20, 90.0 * 86400.0),
        // User free-text search — like audit but with a longer half-life.
        UserSearch => (0.40, 0.50, 0.10, 365.0 * 86400.0),
    }
}

/// Sanitise the query string for FTS5. FTS5's query language treats
/// punctuation and special characters as operators; the simplest safe
/// approach is to quote the entire query as a phrase and escape any
/// internal double-quote.
fn fts_query_escape(raw: &str) -> String {
    let escaped = raw.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Apply the non-ranking filters from a `MemoryQuery` to a candidate
/// chunk. Returns true if the chunk should be included in the result.
fn chunk_matches_query(c: &MemoryChunk, q: &MemoryQuery) -> bool {
    // Kind filter
    if let Some(kinds) = &q.kinds {
        if !kinds.contains(&c.kind) {
            return false;
        }
    }
    // Rollups gate
    if !q.include_rollups && c.kind == cel_memory::ChunkKind::Rollup {
        return false;
    }
    // Time bounds
    if let Some(since) = q.since {
        if c.created_at < since {
            return false;
        }
    }
    if let Some(until) = q.until {
        if c.created_at > until {
            return false;
        }
    }
    // Session
    if let Some(sid) = &q.session_id {
        if c.session_id.as_deref() != Some(sid.as_str()) {
            return false;
        }
    }
    // Project root prefix
    if let Some(prefix) = &q.project_root_prefix {
        match &c.project_root {
            Some(root) if root.starts_with(prefix.as_str()) => {}
            _ => return false,
        }
    }
    // Min importance
    if let Some(min) = q.min_importance {
        if c.importance < min {
            return false;
        }
    }
    // Caller scope. `Own` restricts to the caller's own chunks.
    // `OwnPlusShared` (Phase 4) permits the caller's own chunks *plus* any
    // chunk tagged `shareable=true` from another caller. `Global` permits
    // everything — granted only to privileged surfaces (Memory tab,
    // audit timeline).
    match q.caller_scope {
        cel_memory::CallerScope::Own => {
            if c.caller_id != q.caller_id {
                return false;
            }
        }
        cel_memory::CallerScope::OwnPlusShared => {
            if c.caller_id != q.caller_id && !c.shareable {
                return false;
            }
        }
        cel_memory::CallerScope::Global => {}
    }
    true
}

/// Translate a [`MemoryPredicate`] into a parameterized WHERE clause for
/// `memory_chunks`. Returns the clause body (without the leading `WHERE`)
/// and the parameter vector. The clause is composed of `AND`-joined
/// sub-clauses, defaulting to `1=1` if (somehow) every predicate field is
/// `None` — but [`MemoryPredicate::is_empty`] should have short-circuited
/// before this is called.
fn build_chunk_predicate(p: &MemoryPredicate) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut clauses: Vec<String> = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(kinds) = &p.kinds {
        let placeholders = vec!["?"; kinds.len()].join(",");
        clauses.push(format!("kind IN ({placeholders})"));
        for k in kinds {
            params.push(Box::new(kind_str(*k).to_string()));
        }
    }
    if let Some(callers) = &p.callers {
        let placeholders = vec!["?"; callers.len()].join(",");
        clauses.push(format!("caller_id IN ({placeholders})"));
        for c in callers {
            params.push(Box::new(c.clone()));
        }
    }
    if let Some(sids) = &p.session_ids {
        let placeholders = vec!["?"; sids.len()].join(",");
        clauses.push(format!("session_id IN ({placeholders})"));
        for s in sids {
            params.push(Box::new(s.clone()));
        }
    }
    if let Some(prefix) = &p.project_root_prefix {
        clauses.push("project_root LIKE ?".to_string());
        params.push(Box::new(format!("{prefix}%")));
    }
    if let Some(before) = p.before {
        clauses.push("created_at < ?".to_string());
        params.push(Box::new(dt_to_ms(before)));
    }
    if let Some(after) = p.after {
        clauses.push("created_at > ?".to_string());
        params.push(Box::new(dt_to_ms(after)));
    }
    if let Some(pinned) = p.pinned {
        clauses.push("pinned = ?".to_string());
        params.push(Box::new(if pinned { 1_i64 } else { 0 }));
    }
    if let Some(below) = p.importance_below {
        clauses.push("importance < ?".to_string());
        params.push(Box::new(below as f64));
    }
    if let Some(needle) = &p.content_contains {
        clauses.push("LOWER(content) LIKE ?".to_string());
        params.push(Box::new(format!("%{}%", needle.to_lowercase())));
    }
    (clauses.join(" AND "), params)
}

/// Inverse of [`eviction_reason_str`]. Unknown strings (e.g., from a future
/// schema version) fall back to `UserDelete` rather than panicking; the
/// caller is expected to log + carry on.
fn str_to_eviction_reason(s: &str) -> Result<EvictionReason, MemoryError> {
    Ok(match s {
        "user_delete" => EvictionReason::UserDelete,
        "aging" => EvictionReason::Aging,
        "low_importance" => EvictionReason::LowImportance,
        "redact_rule" => EvictionReason::RedactRule,
        "storage_cap" => EvictionReason::StorageCap,
        "purge_all" => EvictionReason::PurgeAll,
        other => {
            return Err(MemoryError::Storage(format!(
                "unknown eviction reason: {other}"
            )))
        }
    })
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
