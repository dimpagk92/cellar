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

/// Params for `events.publish` — inject an external event into the daemon's
/// event bus so the rule matcher evaluates it. Intended for the Tauri app to
/// bridge Cortex events (e.g. `url_changed`) that originate inside the Tauri
/// process and would otherwise never reach the daemon matcher.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventsPublishParams {
    /// Snake-case event kind string (e.g. `"url_changed"`, `"file_deleted"`).
    /// Must match a variant of `cellar_types::EventKind`.
    pub kind: String,
    /// Snake-case event source string (e.g. `"cortex_cdp"`, `"cortex_ax"`).
    /// Defaults to `"cortex_cdp"` when omitted — the expected source for
    /// Tauri-bridged navigation events.
    #[serde(default)]
    pub source: Option<String>,
    /// Arbitrary key-value payload forwarded verbatim into `Event::data`.
    /// Rule expressions address these as `data.<key>`.
    #[serde(default)]
    pub data: serde_json::Map<String, serde_json::Value>,
}
