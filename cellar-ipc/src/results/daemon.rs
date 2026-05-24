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
