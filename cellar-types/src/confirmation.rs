//! Pending confirmation rows.
//!
//! When a `guard` rule with `action.type = require_confirmation` fires on an
//! `agent_action_attempted` event, the gateway stores a `PendingConfirmation`
//! and pushes it to the Tauri app. The user resolves via the modal.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::Event;

/// A pending confirmation request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingConfirmation {
    /// Stable identifier.
    pub id: String,
    /// When created.
    pub created_at: DateTime<Utc>,
    /// When it auto-resolves as deny.
    pub expires_at: DateTime<Utc>,
    /// The rule that triggered.
    pub rule_id: String,
    /// The matched event.
    pub event: Event,
    /// The original `cel_act` args being held.
    pub originating_action: Value,
    /// Who attempted the action: `"embedded"` or `"mcp:<client_id>"`.
    pub caller: String,
    /// If from the embedded agent: the session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
}

/// User decision on a pending confirmation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationDecision {
    /// One-time allow.
    Allow,
    /// Reject the action.
    Deny,
    /// Allow and remember the choice (creates watchlist add or exception rule).
    AlwaysAllow,
}

/// Carries the inferred "always allow" scope back to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RememberKind {
    /// Add an item to an existing watchlist.
    WatchlistAdd {
        /// Watchlist name to add the item to.
        watchlist_name: String,
        /// Item to add.
        item: String,
    },
    /// Create a new exception rule.
    ExceptionRule {
        /// Suggested name for the new rule.
        suggested_name: String,
    },
}
