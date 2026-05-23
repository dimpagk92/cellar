//! `agent.*` response types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Result of `agent.sessions.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentSessionsListResult {
    /// All sessions, newest-first.
    pub sessions: Vec<AgentSessionMetadata>,
}

/// Minimal session metadata for the list view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSessionMetadata {
    /// Session ID.
    pub id: String,
    /// Optional human-readable title.
    pub title: Option<String>,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last updated (user or agent message).
    pub updated_at: DateTime<Utc>,
    /// Number of messages in the session.
    pub message_count: u64,
    /// `"open"` | `"success"` | `"failure"` | `"aborted"`.
    pub outcome: String,
}

/// Result of `agent.sessions.create`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSessionsCreateResult {
    /// New session ID.
    pub session_id: String,
}

/// Result of `agent.sessions.get` — full session history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSessionsGetResult {
    /// The session metadata.
    pub session: AgentSessionMetadata,
    /// Chat messages in chronological order.
    pub messages: Vec<AgentMessage>,
}

/// A single message in an agent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentMessage {
    /// Message ID.
    pub id: String,
    /// `"user"` | `"assistant"` | `"tool"` | `"system"`.
    pub role: String,
    /// Message content (text or tool-call payload).
    pub content: Value,
    /// When the message was emitted.
    pub created_at: DateTime<Utc>,
}

/// Result of `agent.message`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMessageResult {
    /// Request correlator — appears on every subsequent `agent.chat.frame`
    /// notification produced by this turn.
    pub request_id: String,
    /// ID assigned to the user's message.
    pub message_id: String,
}
