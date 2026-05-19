//! `webhooks.*` request parameters.

use cellar_types::WebhookConfig;
use serde::{Deserialize, Serialize};

/// Params for `webhooks.list`. Currently empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhooksListParams {}

/// Params for `webhooks.add`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhooksAddParams {
    /// Webhook configuration. Secrets reference env-var names only.
    pub config: WebhookConfig,
}

/// Params for `webhooks.remove` and `webhooks.test`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookIdParams {
    /// Webhook ID.
    pub id: String,
}
