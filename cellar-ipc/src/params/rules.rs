//! `rules.*` request parameters.

use cellar_types::Rule;
use serde::{Deserialize, Serialize};

/// Params for `rules.list`. Currently empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RulesListParams {}

/// Params for `rules.get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RulesGetParams {
    /// Rule ID.
    pub id: String,
}

/// Params for `rules.add`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RulesAddParams {
    /// The compiled rule to persist.
    pub rule: Rule,
}

/// Params for `rules.update`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RulesUpdateParams {
    /// Rule ID to update.
    pub id: String,
    /// The new compiled rule.
    pub rule: Rule,
}

/// Params for `rules.remove` / `rules.pause` / `rules.resume`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleIdParams {
    /// Rule ID.
    pub id: String,
}

/// Params for `rules.compile`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RulesCompileParams {
    /// The user's natural-language rule description.
    pub nl_string: String,
}

/// Params for `rules.test`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RulesTestParams {
    /// Rule ID to test.
    pub id: String,
    /// Lower bound on event timestamps to replay against.
    pub since: chrono::DateTime<chrono::Utc>,
}
