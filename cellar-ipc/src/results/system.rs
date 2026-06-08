//! `system.*` response types.

use chrono::{DateTime, Utc};
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

/// Result of `system.connected_clients` — recent IPC clients that have
/// said hello to this daemon. Deduped by `client_name`; the timestamp is
/// the most recent hello.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemConnectedClientsResult {
    /// One entry per distinct `client_name`. Sorted newest-first by
    /// `last_hello_at`.
    pub clients: Vec<ConnectedClient>,
}

/// One row in [`SystemConnectedClientsResult::clients`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectedClient {
    /// Whatever the client passed in `system.hello.params.client_name`,
    /// e.g. `"cellar-tauri"`, `"cellar-cli"`, `"claude-code"`.
    pub client_name: String,
    /// Client semver string from `system.hello`.
    pub client_version: String,
    /// Wallclock time of the most recent hello.
    pub last_hello_at: DateTime<Utc>,
}
