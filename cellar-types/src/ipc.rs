//! IPC message shapes shared by daemon and clients.
//!
//! See `cellar-ipc-protocol.md` for the wire-level RFC. This module declares
//! the strongly-typed request/response payloads referenced there. The actual
//! transport (JSON-RPC over Unix Domain Socket) is implemented by the daemon
//! and the Tauri Rust backend separately.

use serde::{Deserialize, Serialize};

/// Current protocol version advertised in `system.hello`.
pub const PROTOCOL_VERSION: &str = "1";

/// `system.hello` request params.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloRequest {
    /// Client-provided name, e.g. `"cellar-tauri"`.
    pub client_name: String,
    /// Client version (semver).
    pub client_version: String,
    /// Protocol versions the client supports, ordered preference-high-first.
    pub supported_protocol_versions: Vec<String>,
}

/// `system.hello` response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloResponse {
    /// Chosen protocol version.
    pub protocol_version: String,
    /// Daemon version (semver).
    pub daemon_version: String,
    /// Daemon uptime in seconds.
    pub daemon_uptime_s: u64,
    /// Per-connection session id (not a chat session id).
    pub session_id: String,
    /// Daemon capability flags, e.g. `"agent"`, `"memory.basic"`, `"external_mcp"`.
    pub capabilities: Vec<String>,
}

/// Filter applied to event / fire / agent_action queries and subscriptions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventFilter {
    /// Lower-bound timestamp (RFC3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Maximum number of items to return (for `.recent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Filter by event kind names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Filter by event source names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Filter by rule ids (fires only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_ids: Vec<String>,
    /// Filter by caller (agent_actions only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<String>,
}

/// Error codes returned by RPC methods.
#[allow(missing_docs)]
pub mod error_codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub const DAEMON_SHUTTING_DOWN: i32 = -32000;
    pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32001;
    pub const NOT_AUTHORIZED: i32 = -32002;
    pub const RATE_LIMITED: i32 = -32003;
    pub const RULE_NOT_FOUND: i32 = -32004;
    pub const WATCHLIST_NOT_FOUND: i32 = -32005;
    pub const WEBHOOK_NOT_FOUND: i32 = -32006;
    pub const SESSION_NOT_FOUND: i32 = -32007;
    pub const CONFIRMATION_NOT_FOUND: i32 = -32008;
    pub const CONFIRMATION_ALREADY_RESOLVED: i32 = -32009;
    pub const VALIDATION_FAILED: i32 = -32010;
    pub const LLM_PROVIDER_ERROR: i32 = -32011;
    pub const EXTERNAL_MCP_DISABLED: i32 = -32012;
    pub const TAURI_NOT_ATTACHED: i32 = -32013;
}
