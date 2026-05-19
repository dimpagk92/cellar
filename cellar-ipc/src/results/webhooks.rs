//! `webhooks.*` response types.

use cellar_types::WebhookConfig;
use serde::{Deserialize, Serialize};

/// Result of `webhooks.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhooksListResult {
    /// All configured webhooks.
    pub webhooks: Vec<WebhookConfig>,
}

/// Result of `webhooks.test`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhooksTestResult {
    /// Whether the test POST succeeded.
    pub reachable: bool,
    /// HTTP status from the target (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// Response time in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Error message if the test failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
