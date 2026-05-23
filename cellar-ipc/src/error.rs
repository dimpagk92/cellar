//! IPC errors with the JSON-RPC 2.0 error-code allocation from the RFC.

use serde_json::Value;
use thiserror::Error;

/// Result alias.
pub type IpcResult<T> = std::result::Result<T, IpcError>;

/// Errors produced by IPC operations.
///
/// Variants are roughly grouped: JSON-RPC standard errors (mirror the spec),
/// daemon-specific codes from [`cellar-ipc-protocol.md`] §3.2, and transport
/// errors that don't have a JSON-RPC code (connection drops, codec failures).
///
/// [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md
#[derive(Debug, Error)]
pub enum IpcError {
    // ───── Standard JSON-RPC 2.0 errors ─────
    /// `-32700` — invalid JSON received.
    #[error("parse error: {0}")]
    Parse(String),
    /// `-32600` — not a valid JSON-RPC 2.0 request.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// `-32601` — no such method.
    #[error("method not found: {0}")]
    MethodNotFound(String),
    /// `-32602` — params don't match schema.
    #[error("invalid params: {0}")]
    InvalidParams(String),
    /// `-32603` — daemon internal error.
    #[error("internal error: {0}")]
    Internal(String),

    // ───── Daemon-specific (RFC §3.2) ─────
    /// `-32000` — daemon is shutting down.
    #[error("daemon shutting down")]
    ShuttingDown,
    /// `-32001` — client and daemon have no protocol-version overlap.
    #[error("unsupported protocol version (client supports: {0:?})")]
    UnsupportedProtocolVersion(Vec<String>),
    /// `-32002` — auth failed (reserved; only file-perm auth in v1).
    #[error("not authorized")]
    NotAuthorized,
    /// `-32003` — request rejected to protect daemon.
    #[error("rate limited")]
    RateLimited,
    /// `-32004` — rule not found.
    #[error("rule not found: {0}")]
    RuleNotFound(String),
    /// `-32005` — watchlist not found.
    #[error("watchlist not found: {0}")]
    WatchlistNotFound(String),
    /// `-32006` — webhook not found.
    #[error("webhook not found: {0}")]
    WebhookNotFound(String),
    /// `-32007` — agent session not found.
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// `-32008` — confirmation not found.
    #[error("confirmation not found: {0}")]
    ConfirmationNotFound(String),
    /// `-32009` — confirmation already resolved.
    #[error("confirmation already resolved: {0}")]
    ConfirmationAlreadyResolved(String),
    /// `-32010` — NL compiler produced invalid rule, or user-supplied
    /// rule invalid.
    #[error("validation failed: {0}")]
    ValidationFailed(String),
    /// `-32011` — upstream LLM call failed.
    #[error("LLM provider error: {0}")]
    LlmProviderError(String),
    /// `-32012` — external MCP feature disabled by config.
    #[error("external MCP disabled")]
    ExternalMcpDisabled,
    /// `-32013` — RPC requires Tauri client (some RPCs may not be CLI-safe).
    #[error("Tauri not attached")]
    TauriNotAttached,

    // ───── Subsystem stubs ─────
    /// `-32099` — method is recognised but its backing subsystem isn't wired
    /// in this daemon build (the v1 stub returns this for most methods).
    /// Not in the RFC's error-code table because it's a v1-only convenience.
    #[error("not implemented in this daemon build: {0}")]
    NotImplemented(&'static str),

    // ───── Transport / codec ─────
    /// Underlying IO error (socket closed, write failed, etc.).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization failure outside the JSON-RPC
    /// envelope (e.g., reading the next line failed).
    #[error("codec error: {0}")]
    Codec(String),
    /// Client received a response without a matching request id.
    #[error("orphan response: id={0}")]
    OrphanResponse(String),
    /// Client received a malformed frame the server claimed was a stream
    /// notification (the codec dispatches by method name).
    #[error("malformed stream frame: {0}")]
    MalformedFrame(String),
    /// Connection closed before the response arrived.
    #[error("connection closed")]
    ConnectionClosed,
}

impl IpcError {
    /// The JSON-RPC error code corresponding to this variant. Used by the
    /// server to populate the `error.code` field in [`crate::JsonRpcError`].
    pub fn code(&self) -> i32 {
        match self {
            IpcError::Parse(_) => -32700,
            IpcError::InvalidRequest(_) => -32600,
            IpcError::MethodNotFound(_) => -32601,
            IpcError::InvalidParams(_) => -32602,
            IpcError::Internal(_) => -32603,
            IpcError::ShuttingDown => -32000,
            IpcError::UnsupportedProtocolVersion(_) => -32001,
            IpcError::NotAuthorized => -32002,
            IpcError::RateLimited => -32003,
            IpcError::RuleNotFound(_) => -32004,
            IpcError::WatchlistNotFound(_) => -32005,
            IpcError::WebhookNotFound(_) => -32006,
            IpcError::SessionNotFound(_) => -32007,
            IpcError::ConfirmationNotFound(_) => -32008,
            IpcError::ConfirmationAlreadyResolved(_) => -32009,
            IpcError::ValidationFailed(_) => -32010,
            IpcError::LlmProviderError(_) => -32011,
            IpcError::ExternalMcpDisabled => -32012,
            IpcError::TauriNotAttached => -32013,
            IpcError::NotImplemented(_) => -32099,
            // Transport errors don't have JSON-RPC codes; they should not be
            // serialized as errors over the wire (they happen *to* the wire).
            // The server maps them to InvalidRequest before transmitting.
            IpcError::Io(_)
            | IpcError::Codec(_)
            | IpcError::OrphanResponse(_)
            | IpcError::MalformedFrame(_)
            | IpcError::ConnectionClosed => -32603,
        }
    }

    /// Extract any error-specific structured context. The server emits this
    /// as the `error.data` field on the wire.
    pub fn data(&self) -> Option<Value> {
        match self {
            IpcError::RuleNotFound(id) => Some(serde_json::json!({ "rule_id": id })),
            IpcError::WatchlistNotFound(name) => {
                Some(serde_json::json!({ "watchlist_name": name }))
            }
            IpcError::WebhookNotFound(id) => Some(serde_json::json!({ "webhook_id": id })),
            IpcError::SessionNotFound(id) => Some(serde_json::json!({ "session_id": id })),
            IpcError::ConfirmationNotFound(id) | IpcError::ConfirmationAlreadyResolved(id) => {
                Some(serde_json::json!({ "confirmation_id": id }))
            }
            IpcError::UnsupportedProtocolVersion(client_versions) => {
                Some(serde_json::json!({ "client_supports": client_versions }))
            }
            IpcError::NotImplemented(method) => Some(serde_json::json!({ "method": method })),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_rfc() {
        assert_eq!(IpcError::Parse(String::new()).code(), -32700);
        assert_eq!(IpcError::ShuttingDown.code(), -32000);
        assert_eq!(IpcError::RuleNotFound("x".into()).code(), -32004);
        assert_eq!(IpcError::NotImplemented("x").code(), -32099);
    }

    #[test]
    fn data_carries_structured_context() {
        let e = IpcError::RuleNotFound("rule_x".into());
        assert_eq!(e.data().unwrap()["rule_id"], "rule_x");
    }
}
