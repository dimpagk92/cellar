//! `fires.*` request parameters.

use serde::{Deserialize, Serialize};

use super::stream_filter::StreamFilter;

/// Params for `fires.recent`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FiresRecentParams {
    /// Filter applied to the recent-fires query.
    #[serde(flatten)]
    pub filter: StreamFilter,
}

/// Params for `fires.subscribe`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FiresSubscribeParams {
    /// Filter applied to the subscription stream.
    pub filter: StreamFilter,
}
