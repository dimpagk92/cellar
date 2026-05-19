//! `events.*` request parameters.

use serde::{Deserialize, Serialize};

use super::stream_filter::StreamFilter;
use crate::subscription::SubscriptionId;

/// Params for `events.recent`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventsRecentParams {
    /// Filter applied to the recent-events query.
    #[serde(flatten)]
    pub filter: StreamFilter,
}

/// Params for `events.subscribe`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventsSubscribeParams {
    /// Filter applied to the subscription stream.
    pub filter: StreamFilter,
}

/// Params for `events.unsubscribe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsubscribeParams {
    /// The subscription ID to cancel.
    pub subscription_id: SubscriptionId,
}
