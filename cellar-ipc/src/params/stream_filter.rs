//! Shared filter shape for `events.*`, `fires.*`, and `agent_actions.*`.
//!
//! See [`cellar-ipc-protocol.md`] §4.6.
//!
//! [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md

use serde::{Deserialize, Serialize};

/// Filter applied to `events.*`, `fires.*`, and `agent_actions.*` calls.
/// Empty by default (matches everything).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamFilter {
    /// Lower bound on item timestamps. `.recent` defaults to "last 1h" if
    /// absent; subscriptions default to "now onward".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Cap on result count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Restrict to these event kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Restrict to these event sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,
    /// (`fires.*` and `agent_actions.*` only) restrict to these rule IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_ids: Option<Vec<String>>,
    /// (`agent_actions.*` only) restrict to these callers (`"embedded"`,
    /// `"mcp:cursor"`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callers: Option<Vec<String>>,
}
