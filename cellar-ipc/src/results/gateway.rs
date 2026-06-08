//! `gateway.*` response types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Result of `gateway.intercept`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayInterceptResult {
    /// The final outcome of the intercepted action.
    pub outcome: GatewayOutcomeWire,
}

/// Wire-level outcome — mirrors `cel_act_gateway::ActionOutcome` with
/// serde-friendly tag/content encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayOutcomeWire {
    /// Action passed all rules and was executed by the daemon's actuator.
    Executed {
        /// Actuator-specific result payload (may be `null` for no-op actuators).
        result: Value,
    },
    /// A `veto` rule matched; action did not execute.
    Vetoed {
        /// ID of the rule that vetoed.
        rule_id: String,
        /// Human-readable name of the rule.
        rule_name: String,
    },
    /// A `require_confirmation` rule matched and the user denied.
    ConfirmationDenied {
        /// ID of the rule that requested confirmation.
        rule_id: String,
        /// Human-readable name of the rule.
        rule_name: String,
    },
    /// A `require_confirmation` rule matched and the timeout elapsed before
    /// the user responded.
    ConfirmationTimedOut {
        /// ID of the rule that requested confirmation.
        rule_id: String,
        /// Human-readable name of the rule.
        rule_name: String,
        /// Seconds that elapsed before the timeout.
        timeout_s: u64,
    },
}

impl GatewayOutcomeWire {
    /// True iff the action was actually executed.
    pub fn executed(&self) -> bool {
        matches!(self, GatewayOutcomeWire::Executed { .. })
    }

    /// Short one-line summary for CLI display.
    pub fn summary(&self) -> String {
        match self {
            Self::Executed { .. } => "✅ allowed — action executed".into(),
            Self::Vetoed {
                rule_id, rule_name, ..
            } => format!("🚫 vetoed by rule \"{rule_name}\" ({rule_id})"),
            Self::ConfirmationDenied {
                rule_id, rule_name, ..
            } => format!("⛔ denied by user (rule \"{rule_name}\", {rule_id})"),
            Self::ConfirmationTimedOut {
                rule_id,
                rule_name,
                timeout_s,
            } => format!(
                "⏰ confirmation timed out after {timeout_s}s (rule \"{rule_name}\", {rule_id})"
            ),
        }
    }
}
