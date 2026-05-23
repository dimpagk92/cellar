//! Subscription identifiers and stream-frame types.
//!
//! Subscriptions are JSON-RPC requests that, instead of returning a single
//! response, return a [`SubscriptionId`] and then emit notifications
//! (`*.frame` methods) until the client unsubscribes or the connection
//! drops. The frame envelope is always `{ "subscription_id": ..., ... }`
//! plus the typed payload defined here.
//!
//! See [`cellar-ipc-protocol.md`] §3.3 and §4.6–§4.8.
//!
//! [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::params::confirmation::PendingConfirmation;

/// Opaque subscription identifier. Stable per connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct SubscriptionId(pub String);

impl SubscriptionId {
    /// Mint a new subscription ID.
    pub fn new() -> Self {
        Self(format!("sub_{}", uuid::Uuid::now_v7()))
    }
}

impl Default for SubscriptionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The six known stream subscription targets. Identifies *which* `.subscribe`
/// method the server should treat the frame as belonging to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StreamName {
    /// `events.subscribe` — raw events from the bus.
    Events,
    /// `fires.subscribe` — rule firings.
    Fires,
    /// `agent_actions.subscribe` — every `cel_act` call from any caller.
    AgentActions,
    /// `confirmation.subscribe` — pending confirmations (critical; never
    /// dropped on backpressure).
    Confirmation,
    /// `agent.chat.subscribe` — per-session agent activity (critical).
    AgentChat,
    /// `daemon.health.subscribe` — daemon health updates.
    DaemonHealth,
}

/// A frame emitted on an open subscription. The variant carries the payload;
/// the [`SubscriptionId`] is carried alongside in [`StreamFrame`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StreamPayload {
    // ───── events.subscribe ─────
    /// One event from the bus.
    Event {
        /// The event payload. Deliberately untyped here because `cellar_types::Event`
        /// is borrowed-friendly; the wire passes a fully serialized clone.
        event: Value,
    },

    // ───── fires.subscribe ─────
    /// One rule firing.
    Fire {
        /// Fired-rule entry (id, rule, event, outcome).
        entry: Value,
    },

    // ───── agent_actions.subscribe ─────
    /// One `cel_act` call (attempted / completed / denied).
    AgentAction {
        /// Action entry payload.
        action: Value,
    },

    // ───── confirmation.subscribe ─────
    /// A pending confirmation that needs user resolution.
    Confirmation {
        /// Full pending-confirmation payload.
        confirmation: PendingConfirmation,
    },

    // ───── agent.chat.subscribe ─────
    /// Token stream: assistant message being composed.
    Token {
        /// Request that produced this token.
        request_id: String,
        /// Assistant message ID this token belongs to.
        message_id: String,
        /// The token (token-level delta from the LLM).
        delta: String,
    },
    /// Agent attempted a tool call (before the gateway intercepts).
    ToolCallAttempt {
        /// Request that produced this tool call.
        request_id: String,
        /// Tool name (typically `"cel_act"`).
        tool_name: String,
        /// Tool args.
        args: Value,
        /// Tool-call ID for cross-frame correlation.
        tool_call_id: String,
    },
    /// A guard rule fired and the gateway is awaiting confirmation.
    ToolCallAwaitingConfirmation {
        /// Request that produced this tool call.
        request_id: String,
        /// Tool-call ID for cross-frame correlation.
        tool_call_id: String,
        /// Confirmation ID the user must resolve.
        confirmation_id: String,
    },
    /// The tool call resolved (executed / vetoed / denied).
    ToolCallResult {
        /// Request that produced this tool call.
        request_id: String,
        /// Tool-call ID for cross-frame correlation.
        tool_call_id: String,
        /// `"allowed"` | `"vetoed"` | `"denied"` | `"timed_out"`.
        outcome: String,
        /// Result payload (allowed) or reason string (otherwise).
        result: Option<Value>,
    },
    /// Assistant message complete.
    MessageComplete {
        /// Request that produced this message.
        request_id: String,
        /// Assistant message ID.
        message_id: String,
    },
    /// Agent loop finished for this user message.
    RequestDone {
        /// Request that just completed.
        request_id: String,
        /// Token usage for the loop.
        tokens_used: u64,
    },
    /// Agent loop errored.
    Error {
        /// Request that errored.
        request_id: String,
        /// Human-readable message.
        message: String,
        /// True if the agent can be resumed (transient).
        recoverable: bool,
    },

    // ───── daemon.health.subscribe ─────
    /// Periodic health snapshot or alert.
    Health {
        /// Health payload.
        snapshot: Value,
    },

    // ───── any subscription ─────
    /// Backpressure signal — server dropped N frames for this subscription.
    /// Client should `*.recent`-fetch to catch up.
    Gap {
        /// Number of dropped frames.
        dropped: u64,
        /// Cutoff after which dropping started.
        since: chrono::DateTime<chrono::Utc>,
    },
}

/// A subscription frame. Sent as a JSON-RPC notification with method name
/// matching the originating subscription (e.g., `"events.frame"`,
/// `"agent.chat.frame"`); the params are this struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamFrame {
    /// Which subscription this frame belongs to.
    pub subscription_id: SubscriptionId,
    /// The typed payload.
    #[serde(flatten)]
    pub payload: StreamPayload,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subscription_id_serializes_transparently() {
        let s = SubscriptionId("sub_xyz".into());
        assert_eq!(serde_json::to_value(&s).unwrap(), json!("sub_xyz"));
    }

    #[test]
    fn stream_name_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(StreamName::AgentChat).unwrap(),
            json!("agent_chat")
        );
    }

    #[test]
    fn frame_round_trip_event() {
        let frame = StreamFrame {
            subscription_id: SubscriptionId("sub_xyz".into()),
            payload: StreamPayload::Event {
                event: json!({"kind": "file_deleted"}),
            },
        };
        let wire = serde_json::to_string(&frame).unwrap();
        let back: StreamFrame = serde_json::from_str(&wire).unwrap();
        assert_eq!(frame, back);
    }

    #[test]
    fn frame_round_trip_token() {
        let frame = StreamFrame {
            subscription_id: SubscriptionId("sub_chat".into()),
            payload: StreamPayload::Token {
                request_id: "req_1".into(),
                message_id: "msg_2".into(),
                delta: "Hello".into(),
            },
        };
        let wire = serde_json::to_string(&frame).unwrap();
        let back: StreamFrame = serde_json::from_str(&wire).unwrap();
        assert_eq!(frame, back);
    }
}
