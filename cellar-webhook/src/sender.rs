//! The `Sender` trait and its production HTTP implementation.

use async_trait::async_trait;
use cellar_types::webhook::WebhookConfig;
use serde::Serialize;
use std::time::Duration;

use crate::attempt::AttemptOutcome;

/// Resolved secret material to pass on each delivery. The daemon reads the
/// underlying env var at startup; the value never gets written to disk.
#[derive(Debug, Clone)]
pub struct WebhookSecret {
    /// Header name (e.g., `"X-Webhook-Secret"`).
    pub header_name: String,
    /// Header value (the raw secret).
    pub header_value: String,
}

/// Abstract sender — production sends real HTTP, tests substitute mocks.
///
/// Implementations must be `Send + Sync` for use across tokio tasks.
#[async_trait]
pub trait Sender: Send + Sync {
    /// Send a single delivery attempt. Implementations classify the outcome
    /// per the rules documented on [`AttemptOutcome`].
    ///
    /// `payload` is provided pre-serialized to keep the trait object-safe
    /// without generics. Callers serialize the typed payload once and pass
    /// the JSON bytes.
    async fn send(
        &self,
        config: &WebhookConfig,
        secret: Option<&WebhookSecret>,
        payload: &[u8],
    ) -> AttemptOutcome;
}

/// Production [`Sender`] backed by `reqwest::Client`.
pub struct ReqwestSender {
    client: reqwest::Client,
}

impl ReqwestSender {
    /// Construct a new sender with a default `reqwest::Client`.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client build"),
        }
    }
}

impl Default for ReqwestSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Sender for ReqwestSender {
    async fn send(
        &self,
        config: &WebhookConfig,
        secret: Option<&WebhookSecret>,
        payload: &[u8],
    ) -> AttemptOutcome {
        let mut req = self
            .client
            .post(&config.url)
            .timeout(Duration::from_millis(config.timeout_ms))
            .header("content-type", "application/json");

        for (name, value) in &config.headers {
            req = req.header(name, value);
        }
        if let Some(s) = secret {
            req = req.header(&s.header_name, &s.header_value);
        }

        match req.body(payload.to_vec()).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if (200..300).contains(&status) {
                    AttemptOutcome::Success { status }
                } else if matches!(status, 408 | 429) || (500..600).contains(&status) {
                    AttemptOutcome::RetryableHttp {
                        status,
                        retry_after_s: parse_retry_after(&resp),
                    }
                } else {
                    AttemptOutcome::PermanentHttp { status }
                }
            }
            Err(err) => {
                if err.is_timeout() || err.is_connect() || err.is_request() {
                    AttemptOutcome::RetryableNetwork {
                        message: err.to_string(),
                    }
                } else {
                    AttemptOutcome::PermanentOther {
                        message: err.to_string(),
                    }
                }
            }
        }
    }
}

fn parse_retry_after(resp: &reqwest::Response) -> Option<u64> {
    let header = resp.headers().get("retry-after")?;
    let s = header.to_str().ok()?;
    s.trim().parse::<u64>().ok()
}

/// Helper to serialize any `Serialize` payload to JSON bytes. Used by the
/// service to convert a typed `WebhookPayload<'_>` once before fan-out.
pub fn serialize<T: Serialize>(payload: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(payload)
}
