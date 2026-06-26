//! Operational types — eviction, access logging, export bundles, reports.
//!
//! These are the supporting types for [`crate::MemoryProvider`]'s maintenance
//! and audit surface (`delete`, `delete_matching`, `purge_all`, `export`,
//! `run_aging_sweep`, `re_embed_all`, `record_access`, `stats`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chunk::MemoryChunk;
use crate::query::MemoryPredicate;
use crate::session::MemorySession;

/// Why a chunk was evicted. Recorded in `memory_eviction_log` and reported in
/// [`AgingReport`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvictionReason {
    /// The user explicitly requested deletion.
    UserDelete,
    /// The chunk aged past its per-kind retention horizon.
    Aging,
    /// The chunk's importance fell below the eviction threshold during a
    /// storage-cap sweep.
    LowImportance,
    /// A `redact_memory` rule matched the chunk and elected to remove it.
    RedactRule,
    /// Storage cap exceeded; lowest-importance unpinned non-correction non-fire
    /// chunks were evicted oldest-first.
    StorageCap,
    /// A `purge_all` call by the user wiped everything.
    PurgeAll,
}

/// One entry in the eviction audit log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvictionEntry {
    /// When the eviction happened.
    pub ts: DateTime<Utc>,
    /// ID of the evicted chunk.
    pub chunk_id: String,
    /// Why it was evicted.
    pub reason: EvictionReason,
    /// Optional structured context.
    #[serde(default)]
    pub metadata: Value,
}

/// One entry in the retrieval access log. Used for relevance learning and the
/// recall@k benchmark.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessEntry {
    /// When the access happened.
    pub ts: DateTime<Utc>,
    /// ID of the chunk that was returned.
    pub chunk_id: String,
    /// Caller string used by the consuming subsystem (`"agent"`, `"compiler"`,
    /// `"audit"`, `"user"`).
    pub retrieved_by: String,
    /// Stable hash of the retrieval query — group rows produced by the same
    /// query call.
    pub query_hash: String,
    /// Position in the returned result list (0-indexed).
    pub rank: usize,
    /// True if the agent went on to cite or act on this chunk.
    pub used: bool,
}

/// Filter for [`crate::MemoryProvider::export`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportFilter {
    /// Optional predicate over chunks. `None` includes everything.
    #[serde(default)]
    pub predicate: Option<MemoryPredicate>,
    /// Include the eviction log in the bundle.
    #[serde(default)]
    pub include_eviction_log: bool,
    /// Include the access log in the bundle.
    #[serde(default)]
    pub include_access_log: bool,
    /// Include closed sessions whose chunks are included.
    #[serde(default = "default_true")]
    pub include_sessions: bool,
}

fn default_true() -> bool {
    true
}

/// Result of [`crate::MemoryProvider::export`]. JSON-serializable; the caller is
/// responsible for writing it to disk as `.tar.gz`, `.json`, or another archive
/// format.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExportBundle {
    /// Exported chunks.
    pub chunks: Vec<MemoryChunk>,
    /// Sessions referenced by the chunks (when `include_sessions=true`).
    #[serde(default)]
    pub sessions: Vec<MemorySession>,
    /// Eviction log entries (when `include_eviction_log=true`).
    #[serde(default)]
    pub evictions: Vec<EvictionEntry>,
    /// Access log entries (when `include_access_log=true`).
    #[serde(default)]
    pub accesses: Vec<AccessEntry>,
}

/// Report returned by [`crate::MemoryProvider::purge_all`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PurgeReport {
    /// Number of chunks deleted.
    pub chunks_deleted: usize,
    /// Number of sessions deleted.
    pub sessions_deleted: usize,
    /// Number of access-log entries deleted.
    pub access_log_deleted: usize,
    /// Number of eviction-log entries deleted.
    pub eviction_log_deleted: usize,
}

/// Report returned by [`crate::MemoryProvider::run_aging_sweep`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgingReport {
    /// Chunks transitioned from session tier to long-term tier.
    pub tier_promoted: usize,
    /// Chunks deleted due to aging or storage cap.
    pub deleted: usize,
    /// Total bytes reclaimed (best-effort estimate by the storage layer).
    pub bytes_reclaimed: u64,
    /// Per-reason deletion counts.
    #[serde(default)]
    pub deletions_by_reason: Vec<(EvictionReason, usize)>,
}

/// Report returned by [`crate::MemoryProvider::re_embed_all`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReEmbedReport {
    /// Total chunks targeted.
    pub total: usize,
    /// Chunks successfully re-embedded.
    pub succeeded: usize,
    /// Chunks that failed (model error, content too large, etc.).
    pub failed: usize,
    /// Wall-clock time spent, in milliseconds.
    pub elapsed_ms: u64,
}

/// Summary statistics returned by [`crate::MemoryProvider::stats`]. Useful for
/// dashboards, health checks, and storage monitoring.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryStats {
    /// Total chunks across both tiers.
    pub total_chunks: usize,
    /// Chunks in the `session` tier.
    pub session_chunks: usize,
    /// Chunks in the `long_term` tier.
    pub long_term_chunks: usize,
    /// Total sessions (open + closed).
    pub total_sessions: usize,
    /// Currently open sessions.
    pub open_sessions: usize,
    /// On-disk database size in bytes (best-effort).
    pub db_bytes: u64,
    /// Name of the embedding model in use, if any.
    pub embedding_model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn eviction_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(EvictionReason::StorageCap).unwrap(),
            json!("storage_cap")
        );
        assert_eq!(
            serde_json::to_value(EvictionReason::UserDelete).unwrap(),
            json!("user_delete")
        );
    }

    #[test]
    fn export_filter_defaults_include_sessions() {
        let raw = "{}";
        let f: ExportFilter = serde_json::from_str(raw).unwrap();
        assert!(f.include_sessions);
        assert!(!f.include_access_log);
        assert!(!f.include_eviction_log);
    }

    #[test]
    fn stats_round_trip() {
        let s = MemoryStats {
            total_chunks: 100,
            session_chunks: 30,
            long_term_chunks: 70,
            total_sessions: 5,
            open_sessions: 1,
            db_bytes: 1024 * 1024,
            embedding_model: Some("bge-small-en-v1.5".into()),
        };
        let raw = serde_json::to_string(&s).unwrap();
        let back: MemoryStats = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, back);
    }
}
