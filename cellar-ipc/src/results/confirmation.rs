//! `confirmation.*` response types.

use serde::{Deserialize, Serialize};

use crate::params::confirmation::{PendingConfirmation, RememberKind};

/// Result of `confirmation.list_pending`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConfirmationListPendingResult {
    /// Currently-pending confirmations.
    pub pending: Vec<PendingConfirmation>,
}

/// Result of `confirmation.resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfirmationResolveResult {
    /// True iff the confirmation was resolved (false on race when it was
    /// already resolved / timed out).
    pub resolved: bool,
    /// `"completed"` | `"vetoed"` | `"errored"`.
    pub action_outcome: String,
    /// Echo of the remember decision the daemon actually applied. Useful
    /// when the daemon coerces `always_allow` between watchlist-add and
    /// exception-rule based on what makes sense for the matched rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remembered_as: Option<RememberKind>,
}
