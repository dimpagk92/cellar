//! JSON-RPC 2.0 envelope types.
//!
//! The crate's "wire" layer. Every message on the socket is one of these
//! shapes encoded as line-delimited JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::IpcError;

/// JSON-RPC 2.0 request ID. The spec allows string, number, or null;
/// notifications omit the field entirely (see [`JsonRpcRequest::id`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum RequestId {
    /// String ID.
    Str(String),
    /// Integer ID.
    Num(i64),
}

impl RequestId {
    /// Convert to a stable string form for logging / hashtable keys.
    pub fn to_str(&self) -> String {
        match self {
            RequestId::Str(s) => s.clone(),
            RequestId::Num(n) => n.to_string(),
        }
    }
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Num(n)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        RequestId::Str(s)
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        RequestId::Str(s.to_string())
    }
}

/// A JSON-RPC 2.0 request (or notification when `id` is absent).
///
/// Notifications are one-way client → server messages (`system.pong`) or
/// server → client messages (subscription frames, `system.ping`).
///
/// The `trace_id` field is an opt-in correlation token (RFC §9). When a
/// client supplies one, the server propagates it through every log line
/// emitted while serving the request, echoes it in the response, and
/// stamps it on every subscription frame produced by that request. When
/// the client omits it, the server mints a fresh UUID v7 so every request
/// has a trace_id available for correlation in the daemon's structured
/// logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcRequest {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Absent for notifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<RequestId>,
    /// Dotted method name, e.g. `"rules.list"`.
    pub method: String,
    /// Method-specific params. Always an object per RFC §3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Optional client-supplied correlation token. Echoed in the response
    /// envelope and stamped on every subscription frame produced by this
    /// request. Backwards compatible — older clients omit the field and
    /// the daemon mints one server-side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl JsonRpcRequest {
    /// Construct a new request.
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.into()),
            method: method.into(),
            params: Some(params),
            trace_id: None,
        }
    }

    /// Construct a new request with an explicit `trace_id`.
    pub fn new_with_trace(
        id: impl Into<RequestId>,
        method: impl Into<String>,
        params: Value,
        trace_id: impl Into<String>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.into()),
            method: method.into(),
            params: Some(params),
            trace_id: Some(trace_id.into()),
        }
    }

    /// Construct a notification (no `id`).
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params: Some(params),
            trace_id: None,
        }
    }

    /// Construct a notification with an explicit `trace_id` (used by
    /// subscription frame forwarders so frames carry the originating
    /// subscribe request's trace_id).
    pub fn notification_with_trace(
        method: impl Into<String>,
        params: Value,
        trace_id: impl Into<String>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params: Some(params),
            trace_id: Some(trace_id.into()),
        }
    }

    /// True iff this message has no `id` (it's a notification, not a request
    /// expecting a response).
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC 2.0 response. Either carries `result` or `error`, never both.
///
/// The optional `trace_id` echoes whatever the server propagated for the
/// request (either the client-supplied value or the server-minted one).
/// Clients can use it to correlate the response with the daemon-side
/// structured-log lines emitted during the call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Mirrors the request `id`. (`null` for parse errors.)
    pub id: Option<RequestId>,
    /// Success payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error object. Mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    /// Per-request correlation token. Echoes the request's `trace_id`
    /// (when supplied by the client) or a server-minted one when the
    /// client omitted it. See [`JsonRpcRequest::trace_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl JsonRpcResponse {
    /// Construct a success response.
    pub fn ok(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            result: Some(result),
            error: None,
            trace_id: None,
        }
    }

    /// Construct a success response with a `trace_id` echoed back to the
    /// caller.
    pub fn ok_with_trace(id: RequestId, result: Value, trace_id: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            result: Some(result),
            error: None,
            trace_id: Some(trace_id.into()),
        }
    }

    /// Construct an error response.
    pub fn err(id: Option<RequestId>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
            trace_id: None,
        }
    }

    /// Construct an error response carrying a `trace_id`.
    pub fn err_with_trace(
        id: Option<RequestId>,
        error: JsonRpcError,
        trace_id: impl Into<String>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
            trace_id: Some(trace_id.into()),
        }
    }

    /// Construct an error response from an [`IpcError`]. The error's code
    /// and structured data are extracted automatically.
    pub fn from_ipc_error(id: Option<RequestId>, err: &IpcError) -> Self {
        Self::err(
            id,
            JsonRpcError {
                code: err.code(),
                message: err.to_string(),
                data: err.data(),
            },
        )
    }

    /// Construct an error response from an [`IpcError`] carrying a
    /// `trace_id`.
    pub fn from_ipc_error_with_trace(
        id: Option<RequestId>,
        err: &IpcError,
        trace_id: impl Into<String>,
    ) -> Self {
        Self::err_with_trace(
            id,
            JsonRpcError {
                code: err.code(),
                message: err.to_string(),
                data: err.data(),
            },
            trace_id,
        )
    }
}

/// The JSON-RPC `error` object inside a response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    /// Standard JSON-RPC error code or one of the daemon-specific codes
    /// from [`cellar-ipc-protocol.md`] §3.2.
    ///
    /// [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md
    pub code: i32,
    /// Short human-readable message.
    pub message: String,
    /// Optional structured context (e.g., `{"rule_id": "rule_x"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Convenient wire-level alias for the error object alone. Same shape as
/// [`JsonRpcError`]; re-exported for callers that build errors directly.
pub type ErrorObject = JsonRpcError;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_id_serializes_as_untagged_union() {
        let s: RequestId = "abc".into();
        assert_eq!(serde_json::to_value(&s).unwrap(), json!("abc"));
        let n: RequestId = 42_i64.into();
        assert_eq!(serde_json::to_value(&n).unwrap(), json!(42));
    }

    #[test]
    fn request_round_trip() {
        let req = JsonRpcRequest::new(1_i64, "rules.list", json!({}));
        let wire = serde_json::to_string(&req).unwrap();
        let back: JsonRpcRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn notification_has_no_id() {
        let n = JsonRpcRequest::notification("system.ping", json!({}));
        assert!(n.is_notification());
        let wire = serde_json::to_string(&n).unwrap();
        assert!(!wire.contains("\"id\""));
    }

    #[test]
    fn response_ok_skips_error_field() {
        let r = JsonRpcResponse::ok(RequestId::Num(1), json!({"status": "ok"}));
        let wire = serde_json::to_string(&r).unwrap();
        assert!(!wire.contains("\"error\""));
        assert!(wire.contains("\"result\""));
    }

    #[test]
    fn from_ipc_error_populates_code_and_data() {
        let err = IpcError::RuleNotFound("rule_x".into());
        let resp = JsonRpcResponse::from_ipc_error(Some(RequestId::Num(7)), &err);
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32004);
        assert_eq!(e.data.unwrap()["rule_id"], "rule_x");
    }

    #[test]
    fn trace_id_round_trips_through_request() {
        // A request with `trace_id` should serialize the field and a
        // wire-decoded copy should compare equal.
        let req = JsonRpcRequest::new_with_trace(1_i64, "rules.list", json!({}), "trace-abc-123");
        let wire = serde_json::to_string(&req).unwrap();
        assert!(wire.contains("\"trace_id\":\"trace-abc-123\""));
        let back: JsonRpcRequest = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.trace_id.as_deref(), Some("trace-abc-123"));
        assert_eq!(req, back);
    }

    #[test]
    fn request_without_trace_id_skips_field_on_wire() {
        // Backward compat: legacy clients that don't set trace_id must
        // produce wire bytes without the field at all.
        let req = JsonRpcRequest::new(1_i64, "rules.list", json!({}));
        assert!(req.trace_id.is_none());
        let wire = serde_json::to_string(&req).unwrap();
        assert!(!wire.contains("trace_id"), "wire was: {wire}");
        // And a wire payload missing the field must deserialize cleanly
        // with `trace_id == None`.
        let legacy_wire = r#"{"jsonrpc":"2.0","id":1,"method":"rules.list","params":{}}"#;
        let parsed: JsonRpcRequest = serde_json::from_str(legacy_wire).unwrap();
        assert!(parsed.trace_id.is_none());
    }

    #[test]
    fn trace_id_round_trips_through_response() {
        let resp = JsonRpcResponse::ok_with_trace(
            RequestId::Num(1),
            json!({"status": "ok"}),
            "trace-resp-1",
        );
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(wire.contains("\"trace_id\":\"trace-resp-1\""));
        let back: JsonRpcResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.trace_id.as_deref(), Some("trace-resp-1"));
    }

    #[test]
    fn trace_id_round_trips_through_error_response() {
        let err = IpcError::RuleNotFound("rule_x".into());
        let resp = JsonRpcResponse::from_ipc_error_with_trace(
            Some(RequestId::Num(7)),
            &err,
            "trace-err-1",
        );
        assert_eq!(resp.trace_id.as_deref(), Some("trace-err-1"));
        let wire = serde_json::to_string(&resp).unwrap();
        let back: JsonRpcResponse = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.trace_id.as_deref(), Some("trace-err-1"));
        assert_eq!(back.error.as_ref().unwrap().code, -32004);
    }
}
