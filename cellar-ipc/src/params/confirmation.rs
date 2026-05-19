//! `confirmation.*` request parameters and shared types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Params for `confirmation.list_pending`. Currently empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmationListPendingParams {}

/// Params for `confirmation.subscribe`. Currently empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmationSubscribeParams {}

/// Params for `confirmation.resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmationResolveParams {
    /// Confirmation ID.
    pub id: String,
    /// Allow / Deny / AlwaysAllow.
    pub decision: ConfirmationDecisionWire,
    /// Required when `decision = always_allow`. Describes how the user
    /// wants the daemon to remember the override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_kind: Option<RememberKind>,
}

/// Wire-level confirmation decision. Distinct from
/// `cel_act_gateway::ConfirmationDecision` because that one has variants for
/// internal timeout cases that the client doesn't send.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationDecisionWire {
    /// User clicks Allow.
    Allow,
    /// User clicks Deny.
    Deny,
    /// User clicks "Always allow this kind."
    AlwaysAllow,
}

/// Describes how the daemon should remember an `always_allow` decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RememberKind {
    /// Add an item to a named watchlist (preferred when the matched rule
    /// references one).
    WatchlistAdd {
        /// Target watchlist.
        watchlist_name: String,
        /// Item to add.
        item: String,
    },
    /// Create a permissive exception rule (fallback when the matched rule
    /// doesn't reference a watchlist).
    ExceptionRule {
        /// Human-readable name for the new rule.
        name: String,
    },
}

/// A pending confirmation surfaced to the client via
/// `confirmation.list_pending` or as a `confirmation.frame` notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingConfirmation {
    /// Confirmation ID.
    pub id: String,
    /// When the confirmation was raised.
    pub created_at: DateTime<Utc>,
    /// When the confirmation will auto-deny if unresolved.
    pub expires_at: DateTime<Utc>,
    /// Rule that requested confirmation.
    pub rule: PendingRule,
    /// The originating event (synthesised by the gateway).
    pub event: Value,
    /// Full `cel_act` payload that triggered the confirmation.
    pub originating_action: Value,
    /// Normalised caller — `"embedded"`, `"mcp:cursor"`, etc.
    pub caller: String,
    /// Optional agent-session ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
}

/// Subset of a rule sent in confirmation payloads (avoids shipping the
/// whole compiled rule on every confirmation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingRule {
    /// Rule ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The user's NL original.
    pub nl_original: String,
}
