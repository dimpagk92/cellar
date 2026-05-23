//! Shared subscribe-call result.

use serde::{Deserialize, Serialize};

use crate::subscription::SubscriptionId;

/// Result of any `*.subscribe` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscribeResult {
    /// Stable subscription ID; client uses this to correlate frames and
    /// later to `unsubscribe`.
    pub subscription_id: SubscriptionId,
}
