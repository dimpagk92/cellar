//! `settings.*` request parameters.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Params for `settings.get`. Currently empty (returns all settings).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SettingsGetParams {}

/// Params for `settings.set`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SettingsSetParams {
    /// Setting key (dot-separated path through the settings tree).
    pub key: String,
    /// New value.
    pub value: Value,
}
