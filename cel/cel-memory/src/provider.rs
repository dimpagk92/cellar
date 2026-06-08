//! The [`MemoryProvider`] trait — the durable cross-turn memory contract.
//!
//! This is the single trait every memory backend implements and every caller
//! depends on. In Cellar that means the embedded agent runtime, the NL rule
//! compiler, the `cel_act` gateway, the rule-matcher post-fire hook, the
//! Activity / Memory tabs, and the MCP `cel_remember` / `cel_recall` /
//! `cel_forget` handlers — but the trait itself is runtime-agnostic.
//! [`crate::BasicMemoryProvider`] is the in-crate reference backend; a full
//! storage backend (e.g. the `cel-memory-sqlite` crate) drops in later behind
//! the same surface without caller churn.

use async_trait::async_trait;
use chrono::NaiveDate;

use crate::chunk::{MemoryChunk, NewMemoryChunk};
use crate::error::Result;
use crate::ops::{
    AgingReport, ExportBundle, ExportFilter, MemoryStats, PurgeReport, ReEmbedReport,
};
use crate::query::{MemoryPredicate, MemoryQuery};
use crate::session::{MemorySession, NewMemorySession, SessionFilter, SessionOutcome};

use crate::ops::EvictionReason;

/// The full memory provider surface.
///
/// All methods are async. All return `Result<T, MemoryError>`. The in-crate
/// [`crate::BasicMemoryProvider`] backs the read/write/session/export surface
/// (some methods return [`crate::MemoryError::NotImplemented`]); a full storage
/// backend (e.g. the `cel-memory-sqlite` crate) implements every method.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    // ─────────────── Reads ───────────────

    /// Hybrid retrieval over the memory store. The provider applies the
    /// query's profile, scope, and filters and returns up to `query.k`
    /// chunks in descending relevance order.
    async fn retrieve(&self, query: MemoryQuery) -> Result<Vec<MemoryChunk>>;

    /// Fetch a chunk by ID.
    async fn get(&self, chunk_id: &str) -> Result<Option<MemoryChunk>>;

    /// Fetch a session by ID.
    async fn get_session(&self, session_id: &str) -> Result<Option<MemorySession>>;

    /// List sessions matching the filter, most-recent-first.
    async fn list_sessions(&self, filter: SessionFilter) -> Result<Vec<MemorySession>>;

    // ─────────────── Writes ───────────────

    /// Persist a single new chunk. The provider assigns `id`, `created_at`,
    /// `tier`, and embedding-model metadata.
    async fn write(&self, chunk: NewMemoryChunk) -> Result<MemoryChunk>;

    /// Persist many chunks in one call. The provider may batch embeddings;
    /// the v1 stub processes them one at a time.
    async fn write_batch(&self, chunks: Vec<NewMemoryChunk>) -> Result<Vec<MemoryChunk>>;

    /// Open a new session. Returns the session record.
    async fn open_session(&self, init: NewMemorySession) -> Result<MemorySession>;

    /// Close an open session. Triggers session summarization where supported.
    async fn close_session(&self, session_id: &str, outcome: SessionOutcome) -> Result<()>;

    /// Rename a session's display title. Returns `Err(NotFound)` if the session
    /// does not exist. The v1 `BasicMemoryProvider` implements this in-memory;
    /// `SqliteMemoryProvider` updates the `memory_sessions.title` column.
    async fn rename_session(&self, session_id: &str, title: &str) -> Result<()>;

    // ─────────────── Updates ───────────────

    /// Set or clear the pin flag on a chunk.
    async fn pin(&self, chunk_id: &str, pinned: bool) -> Result<()>;

    /// Set the importance score on a chunk. Out-of-range values are clamped
    /// to `[0.0, 1.0]`. The v1 stub no-ops (importance is not scored).
    async fn update_importance(&self, chunk_id: &str, importance: f32) -> Result<()>;

    /// Mark `old_id` as superseded by `new_id`. Both chunks are retained;
    /// retrieval surfaces the superseder. The v1 stub no-ops.
    async fn supersede(&self, old_id: &str, new_id: &str) -> Result<()>;

    /// Record that a chunk was returned by a retrieval call. `used=true`
    /// signals the consumer cited or acted on it (drives importance bumps
    /// in the full impl).
    async fn record_access(&self, chunk_id: &str, retrieved_by: &str, used: bool) -> Result<()>;

    // ─────────────── Deletes ───────────────

    /// Hard-delete a single chunk, logging the reason.
    async fn delete(&self, chunk_id: &str, reason: EvictionReason) -> Result<()>;

    /// Hard-delete all chunks matching the predicate. Returns the count.
    ///
    /// As a footgun-prevention measure, an empty predicate is a no-op (see
    /// [`MemoryPredicate::is_empty`]). Use [`purge_all`] to delete everything.
    ///
    /// [`MemoryPredicate::is_empty`]: crate::MemoryPredicate::is_empty
    /// [`purge_all`]: MemoryProvider::purge_all
    async fn delete_matching(
        &self,
        predicate: MemoryPredicate,
        reason: EvictionReason,
    ) -> Result<usize>;

    /// Hard-delete every chunk, session, access-log row, and eviction-log
    /// row. The "forget everything" flow.
    async fn purge_all(&self) -> Result<PurgeReport>;

    // ─────────────── Summarization ───────────────

    /// Produce a `JobSummary` chunk synthesizing the named session. Inserted
    /// into the store; returned to the caller. v1 stub returns
    /// [`crate::MemoryError::NotImplemented`].
    async fn summarize_session(&self, session_id: &str) -> Result<MemoryChunk>;

    /// Produce one or more `Rollup` chunks for the named day. v1 stub returns
    /// [`crate::MemoryError::NotImplemented`].
    ///
    /// Skips the day if a rollup already exists for it (idempotent;
    /// cron sweeper can safely re-run). Use [`Self::rollup_day_forced`]
    /// to force re-summarization.
    async fn rollup_day(&self, date: NaiveDate) -> Result<Vec<MemoryChunk>>;

    /// Force-produce one or more `Rollup` chunks for the named day even
    /// if a prior rollup exists. Default impl delegates to
    /// [`Self::rollup_day`] for backward compatibility; the SQLite
    /// provider overrides to actually honour the force flag.
    async fn rollup_day_forced(&self, date: NaiveDate) -> Result<Vec<MemoryChunk>> {
        self.rollup_day(date).await
    }

    /// Produce a `Rollup` chunk for the named rule across the named week.
    /// v1 stub returns [`crate::MemoryError::NotImplemented`].
    ///
    /// Skips the (rule, week) pair if a rollup already exists for it
    /// (idempotent). Use [`Self::rollup_rule_week_forced`] to force
    /// re-summarization.
    async fn rollup_rule_week(&self, rule_id: &str, week_start: NaiveDate) -> Result<MemoryChunk>;

    /// Force-produce a `Rollup` chunk for the named rule + week even if
    /// a prior rollup exists. Default impl delegates to
    /// [`Self::rollup_rule_week`] for backward compatibility; the SQLite
    /// provider overrides to honour the force flag.
    async fn rollup_rule_week_forced(
        &self,
        rule_id: &str,
        week_start: NaiveDate,
    ) -> Result<MemoryChunk> {
        self.rollup_rule_week(rule_id, week_start).await
    }

    // ─────────────── Maintenance ───────────────

    /// Run the aging sweeper: tier transitions, retention-horizon deletes,
    /// importance-based eviction. Idempotent.
    async fn run_aging_sweep(&self) -> Result<AgingReport>;

    /// Re-embed every chunk against the target model. Long-running. Resumable.
    /// v1 stub returns [`crate::MemoryError::NotImplemented`].
    async fn re_embed_all(&self, target_model: &str) -> Result<ReEmbedReport>;

    /// Export memory matching the filter as a self-contained bundle.
    async fn export(&self, filter: ExportFilter) -> Result<ExportBundle>;

    /// Summary statistics for the dashboard / doctor check.
    async fn stats(&self) -> Result<MemoryStats>;
}
