//! `agent_actions.*` request parameters.
//!
//! Every `cel_act` call from any caller — embedded agent or external MCP
//! client — appears in this stream.

use serde::{Deserialize, Serialize};

use super::stream_filter::StreamFilter;

/// Params for `agent_actions.recent`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentActionsRecentParams {
    /// Filter applied to the recent-actions query.
    #[serde(flatten)]
    pub filter: StreamFilter,
}

/// Params for `agent_actions.subscribe`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentActionsSubscribeParams {
    /// Filter applied to the subscription stream.
    pub filter: StreamFilter,
}
