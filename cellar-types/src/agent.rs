//! Embedded agent types — chat sessions, messages, tool calls.
//!
//! The agent lives in the daemon. The Tauri Chat tab is a streaming client.
//! These types are shared on both ends.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A chat session — one conversation with the embedded agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSession {
    /// Stable identifier (e.g., `sess_01H...`).
    pub id: String,
    /// User-supplied or auto-generated title.
    pub title: String,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last activity (message or tool call).
    pub updated_at: DateTime<Utc>,
    /// Number of messages in the session.
    pub message_count: u64,
}

/// A message in a chat session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    /// Stable identifier.
    pub id: String,
    /// Session this belongs to.
    pub session_id: String,
    /// Role of the speaker.
    pub role: ChatRole,
    /// Message content.
    pub content: String,
    /// Tool calls associated with this message (assistant only).
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// When created.
    pub created_at: DateTime<Utc>,
}

/// Message roles.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    /// User-typed message.
    User,
    /// Agent-generated assistant message.
    Assistant,
    /// System (rare in v1, but reserved).
    System,
    /// Tool result fed back into the conversation.
    Tool,
}

/// A tool call attempted by the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Stable identifier (`tc_...`).
    pub id: String,
    /// Tool name (e.g., `cel_act`).
    pub name: String,
    /// Tool arguments (JSON).
    pub args: Value,
    /// Resolution of the tool call, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ToolCallOutcome>,
}

/// Outcome of a tool call after the gateway runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolCallOutcome {
    /// Allowed and completed; carries the tool result.
    Allowed {
        /// The result returned to the agent.
        result: Value,
    },
    /// Vetoed by a rule; carries the rule id and reason.
    Vetoed {
        /// Rule that vetoed.
        rule_id: String,
        /// Human-readable reason.
        reason: String,
    },
    /// User denied via confirmation modal.
    Denied {
        /// Confirmation id.
        confirmation_id: String,
    },
    /// Errored during execution (not policy-related).
    Errored {
        /// Error message.
        message: String,
    },
}
