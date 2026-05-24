//! Rule schema.
//!
//! Three UI-level rule kinds (`watcher`, `guard`, `audit`) over one underlying
//! schema. The kind is metadata for the UI; the matcher only cares about
//! `match` (expression) and `action`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::expression::Expression;

/// A compiled rule. Persisted as JSON in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    /// Stable identifier (e.g., `rule_01H...`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// The natural-language original the user typed.
    pub nl_original: String,
    /// Rule kind for the UI.
    pub kind: RuleKind,
    /// Whether the rule is currently active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Compiled match expression.
    #[serde(rename = "match")]
    pub match_expr: Expression,
    /// What to do when the rule matches.
    pub action: Action,
    /// Minimum seconds between consecutive fires of this rule. 0 = no cooldown.
    #[serde(default)]
    pub cooldown_seconds: u64,
    /// When the rule was created.
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
}

fn default_true() -> bool {
    true
}

/// UI-level rule taxonomy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// Notify only (typically webhook).
    Watcher,
    /// Intercept and intervene (`require_confirmation` / `veto` / `soft_block`).
    Guard,
    /// Silent log-only. Useful for compliance and analytics.
    Audit,
}

/// What happens when a rule fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    /// The action variant.
    #[serde(rename = "type")]
    pub action_type: ActionType,
    /// Reference to a webhook config (when `action_type = Webhook`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_id: Option<String>,
    /// Confirmation timeout in seconds (when `action_type = RequireConfirmation`).
    /// Defaults to the daemon's `default_confirmation_timeout_s` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

/// Action variants.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    /// Explicit exception — this action is always allowed regardless of other
    /// matching rules. Highest decision precedence; created by the
    /// "Always allow this kind" path in the confirmation flow.
    Allow,
    /// Fire a webhook. Used by `watcher` rules.
    Webhook,
    /// Pause the action; ask the user via confirmation modal. Used by `guard` rules.
    RequireConfirmation,
    /// Veto the action outright; agent gets an error. Used by `guard` rules.
    Veto,
    /// Attempt to undo / counter-act via `cel_act` (close window, navigate away, etc.).
    /// Used by `guard` rules. Best-effort.
    SoftBlock,
    /// Silent log only. Used by `audit` rules.
    LogOnly,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::{Expression, Operator};
    use serde_json::json;

    #[test]
    fn round_trip_rule() {
        let r = Rule {
            id: "rule_test".into(),
            name: "Big delete".into(),
            nl_original: "notify when files >1GB are deleted from Documents".into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: Expression::all(vec![
                Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
                Expression::leaf("data.path", Operator::StartsWith, json!("~/Documents")),
                Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
            ]),
            action: Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
            cooldown_seconds: 60,
            created_at: Utc::now(),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: Rule = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn enabled_defaults_true_when_absent() {
        let json = serde_json::json!({
            "id": "rule_x",
            "name": "x",
            "nl_original": "x",
            "kind": "watcher",
            "match": { "all": [] },
            "action": { "type": "log_only" },
            "created_at": "2026-05-14T00:00:00Z"
        });
        let r: Rule = serde_json::from_value(json).unwrap();
        assert!(r.enabled);
    }
}
