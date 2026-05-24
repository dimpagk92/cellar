//! Webhook configuration and payload shapes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::event::Event;
use crate::rule::Rule;

/// A user-configured webhook destination. Secrets referenced by env-var name
/// only — the raw value is never persisted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookConfig {
    /// Stable identifier referenced by `Action.webhook_id`.
    pub id: String,
    /// Destination URL.
    pub url: String,
    /// Extra request headers to send (besides the secret header).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Header name carrying the secret (e.g., `X-Webhook-Secret`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_header: Option<String>,
    /// Environment variable holding the secret value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_value_env: Option<String>,
    /// Request timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    5000
}

/// The payload POSTed when a rule fires. Send-only: holds references into the
/// fired event and rule and is serialized once before the borrowed values go
/// out of scope. `Deserialize` is intentionally omitted (you cannot deserialize
/// into borrowed `&str` / `&Event` without lifetime tricks the rest of the
/// daemon doesn't need).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebhookPayload<'a> {
    /// When the rule fired.
    pub fired_at: DateTime<Utc>,
    /// The rule that fired (subset of fields; full rule may be too large).
    pub rule: WebhookRule<'a>,
    /// The event that matched.
    pub event: &'a Event,
}

/// Subset of `Rule` sent in webhook payloads. Send-only — see [`WebhookPayload`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebhookRule<'a> {
    /// Rule id.
    pub id: &'a str,
    /// Rule name.
    pub name: &'a str,
    /// NL original.
    pub nl_original: &'a str,
}

impl<'a> From<&'a Rule> for WebhookRule<'a> {
    fn from(r: &'a Rule) -> Self {
        WebhookRule {
            id: &r.id,
            name: &r.name,
            nl_original: &r.nl_original,
        }
    }
}
