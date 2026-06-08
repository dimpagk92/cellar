//! `gateway.*` request parameter types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parameters for `gateway.intercept`.
///
/// Submits a proposed action to the daemon's `cel_act` gateway. The daemon
/// runs the rule matcher, applies `allow` / `veto` / `require_confirmation`
/// decisions, and returns the outcome. If a `require_confirmation` rule fires,
/// the call **blocks** until the user resolves the confirmation (via
/// `confirmation.resolve`) or the rule's `timeout_s` elapses.
///
/// Use `cellar confirmation list` to see pending confirmations while the
/// request is in flight, and `cellar confirmation resolve <id> allow|deny`
/// to unblock it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayInterceptParams {
    /// Normalised caller — `"cli"`, `"mcp:cursor"`, `"embedded"`, etc.
    pub caller: String,
    /// The action type — `"copy_file"`, `"fs.move"`, `"shell.run"`, etc.
    pub action_type: String,
    /// Action arguments, forwarded verbatim into `data.action_args`.
    pub action_args: Value,
    /// Optional agent-session ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// Optional working directory / project root for scope-aware rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}
