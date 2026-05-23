//! `rules.*` response types.

use cellar_types::Rule;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Result of `rules.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RulesListResult {
    /// All rules, newest-first.
    pub rules: Vec<Rule>,
}

/// Result of `rules.get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RulesGetResult {
    /// The requested rule, or `None` if not found.
    pub rule: Option<Rule>,
}

/// Result of `rules.add`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RulesAddResult {
    /// ID assigned to the new rule.
    pub rule_id: String,
}

/// Result of `rules.compile`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RulesCompileResult {
    /// The compiled rule. ID is a `draft_*` placeholder until the user calls
    /// `rules.add`.
    pub draft_rule: Rule,
    /// Human-readable summary of the compiled rule (rendered in the
    /// preview dialog).
    pub human_readable: String,
    /// Compiler warnings the user should see (e.g., overly broad rule).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Result of `rules.test`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RulesTestResult {
    /// The events that would have fired this rule in the test window.
    pub matched_events: Vec<Value>,
}
