//! `system.*` response types.

use serde::{Deserialize, Serialize};

/// Result of `system.hello`. Negotiates protocol version and exposes
/// daemon-level capabilities the client can feature-gate against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemHelloResult {
    /// Highest protocol version the daemon supports that the client also
    /// supports.
    pub protocol_version: String,
    /// Daemon semver string.
    pub daemon_version: String,
    /// Daemon uptime in seconds.
    pub daemon_uptime_s: u64,
    /// Per-connection session ID. Used in logs and (in future) to correlate
    /// reconnections.
    pub session_id: String,
    /// Capability strings (e.g., `"agent"`, `"memory.basic"`, `"external_mcp"`).
    /// Clients use this set to feature-gate UI elements.
    pub capabilities: Vec<String>,
}

/// Result of `system.shutdown`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemShutdownResult {
    /// True iff the daemon accepted the shutdown request.
    pub shutting_down: bool,
}
