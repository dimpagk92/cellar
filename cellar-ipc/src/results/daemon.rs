//! `daemon.*` response types.

use serde::{Deserialize, Serialize};

/// Result of `daemon.status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaemonStatusResult {
    /// Overall health.
    pub healthy: bool,
    /// Uptime in seconds.
    pub uptime_s: u64,
    /// Rule counts.
    pub rules: RuleStats,
    /// Watchlist counts.
    pub watchlists: WatchlistStats,
    /// Fires in the last 24 hours.
    pub recent_fires_24h: u64,
    /// Confirmations currently pending user action.
    pub pending_confirmations: u64,
    /// Number of currently-active agent sessions.
    pub agent_sessions_active: u64,
    /// Daemon semver.
    pub daemon_version: String,
    /// Resident memory in megabytes (best-effort estimate).
    pub memory_mb: f64,
    /// Recent CPU utilisation as a percentage (best-effort).
    pub cpu_pct: f64,
    /// Memory corpus counts (chunks + sessions + storage bytes).
    /// Optional so older daemons that don't populate it deserialise
    /// cleanly; clients should treat the absence as "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryCorpusStats>,
    /// Whether the daemon is currently hosting a live Cortex (perception +
    /// execution). `false` on daemons that don't host one. Optional/default so
    /// older daemons (and clients) deserialise cleanly.
    #[serde(default)]
    pub cortex_running: bool,
}

/// Rule count breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleStats {
    /// Total rules persisted.
    pub total: u64,
    /// Rules currently enabled.
    pub enabled: u64,
}

/// Watchlist count breakdown.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchlistStats {
    /// Total watchlists.
    pub total: u64,
}

/// Memory-corpus snapshot embedded in [`DaemonStatusResult::memory`].
///
/// Surfaces the same data the Memory tab header strip shows: total chunks,
/// tier breakdown (session vs. long-term), session counts, and a rough
/// storage-bytes estimate. Pulled from
/// `cel_memory::MemoryProvider::stats()` at the moment `daemon.status` is
/// called — cheap enough that we don't bother caching.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryCorpusStats {
    /// Total persisted chunks across all tiers + callers.
    pub total_chunks: u64,
    /// Chunks in the Session tier (raw rows, not yet rolled up).
    pub session_chunks: u64,
    /// Chunks in the LongTerm tier (rollups + pinned).
    pub long_term_chunks: u64,
    /// Total sessions ever opened.
    pub total_sessions: u64,
    /// Sessions currently in `Open` state.
    pub open_sessions: u64,
    /// Approximate database size in bytes.
    pub db_bytes: u64,
    /// Embedding model id (`mock-384`, `bge-small-en-v1.5`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
}
