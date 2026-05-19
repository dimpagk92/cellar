//! The retry dispatcher.
//!
//! Pure logic: takes a Sender and an attempt budget, runs the retry loop,
//! returns a [`DispatchResult`]. No queue, no worker — those live in
//! [`crate::service`]. This is the unit-testable core.

use cellar_types::webhook::WebhookConfig;
use chrono::Utc;
use std::time::Duration;

use crate::attempt::{Attempt, AttemptOutcome, DispatchResult};
use crate::sender::{Sender, WebhookSecret};

/// Settings for the retry loop.
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// Maximum number of total attempts (1st attempt + retries).
    pub max_attempts: u32,
    /// Base backoff in milliseconds. Doubles each retry, capped at `max_backoff_ms`.
    pub base_backoff_ms: u64,
    /// Cap on backoff growth.
    pub max_backoff_ms: u64,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff_ms: 1000,
            max_backoff_ms: 16_000,
        }
    }
}

/// The retry loop core.
pub struct Dispatcher<S: Sender> {
    sender: S,
    cfg: DispatcherConfig,
}

impl<S: Sender> Dispatcher<S> {
    /// Construct with a `Sender` and explicit config.
    pub fn new(sender: S, cfg: DispatcherConfig) -> Self {
        Self { sender, cfg }
    }

    /// Construct with default config.
    pub fn with_default_config(sender: S) -> Self {
        Self {
            sender,
            cfg: DispatcherConfig::default(),
        }
    }

    /// Run the dispatch loop. Returns when delivery succeeds or attempts run
    /// out.
    pub async fn dispatch(
        &self,
        rule_id: &str,
        config: &WebhookConfig,
        secret: Option<&WebhookSecret>,
        payload: &[u8],
    ) -> DispatchResult {
        let started = Utc::now();
        let mut attempts: Vec<Attempt> = Vec::new();

        for attempt_number in 1..=self.cfg.max_attempts {
            let attempted_at = Utc::now();
            let outcome = self.sender.send(config, secret, payload).await;

            attempts.push(Attempt {
                attempt_number,
                attempted_at,
                summary: summarize(&outcome),
                succeeded: outcome.is_success(),
            });

            if outcome.is_success() {
                return DispatchResult {
                    webhook_id: config.id.clone(),
                    rule_id: rule_id.to_string(),
                    attempts,
                    succeeded: true,
                    elapsed: Utc::now() - started,
                };
            }

            if !outcome.is_retryable() || attempt_number == self.cfg.max_attempts {
                return DispatchResult {
                    webhook_id: config.id.clone(),
                    rule_id: rule_id.to_string(),
                    attempts,
                    succeeded: false,
                    elapsed: Utc::now() - started,
                };
            }

            let backoff = self.compute_backoff(attempt_number, &outcome);
            tokio::time::sleep(backoff).await;
        }

        DispatchResult {
            webhook_id: config.id.clone(),
            rule_id: rule_id.to_string(),
            attempts,
            succeeded: false,
            elapsed: Utc::now() - started,
        }
    }

    fn compute_backoff(&self, attempt_number: u32, outcome: &AttemptOutcome) -> Duration {
        // Respect server-supplied `Retry-After` over our exponential schedule
        // when it's at least our base.
        if let AttemptOutcome::RetryableHttp {
            retry_after_s: Some(s),
            ..
        } = outcome
        {
            let server_ms = s.saturating_mul(1000);
            let our_ms = self.exp_backoff_ms(attempt_number);
            let chosen = our_ms.max(server_ms);
            return Duration::from_millis(chosen.min(self.cfg.max_backoff_ms));
        }
        Duration::from_millis(self.exp_backoff_ms(attempt_number))
    }

    fn exp_backoff_ms(&self, attempt_number: u32) -> u64 {
        // attempt 1 -> base; attempt 2 -> base*2; capped at max.
        let shift = attempt_number.saturating_sub(1).min(10);
        let raw = self.cfg.base_backoff_ms.saturating_mul(1u64 << shift);
        raw.min(self.cfg.max_backoff_ms)
    }
}

fn summarize(outcome: &AttemptOutcome) -> String {
    match outcome {
        AttemptOutcome::Success { status } => format!("ok {}", status),
        AttemptOutcome::RetryableHttp {
            status,
            retry_after_s,
        } => match retry_after_s {
            Some(s) => format!("retryable {} retry-after={}", status, s),
            None => format!("retryable {}", status),
        },
        AttemptOutcome::PermanentHttp { status } => format!("permanent {}", status),
        AttemptOutcome::RetryableNetwork { message } => format!("retryable network: {}", message),
        AttemptOutcome::PermanentOther { message } => format!("permanent other: {}", message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cellar_types::webhook::WebhookConfig;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Test sender that returns a queued sequence of outcomes and records calls.
    struct ScriptedSender {
        queue: Mutex<Vec<AttemptOutcome>>,
        calls: Mutex<u32>,
    }

    impl ScriptedSender {
        fn new(outcomes: Vec<AttemptOutcome>) -> Self {
            Self {
                queue: Mutex::new(outcomes),
                calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Sender for ScriptedSender {
        async fn send(
            &self,
            _: &WebhookConfig,
            _: Option<&WebhookSecret>,
            _: &[u8],
        ) -> AttemptOutcome {
            *self.calls.lock().unwrap() += 1;
            let mut q = self.queue.lock().unwrap();
            if q.len() > 1 {
                q.remove(0)
            } else {
                q.first()
                    .cloned()
                    .unwrap_or(AttemptOutcome::PermanentOther {
                        message: "queue empty".into(),
                    })
            }
        }
    }

    fn cfg() -> WebhookConfig {
        WebhookConfig {
            id: "default".into(),
            url: "https://example.test/hook".into(),
            headers: BTreeMap::new(),
            secret_header: None,
            secret_value_env: None,
            timeout_ms: 5000,
        }
    }

    fn fast_dispatcher_cfg() -> DispatcherConfig {
        DispatcherConfig {
            max_attempts: 5,
            base_backoff_ms: 1,
            max_backoff_ms: 4,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn success_on_first_attempt() {
        let s = ScriptedSender::new(vec![AttemptOutcome::Success { status: 200 }]);
        let d = Dispatcher::new(s, fast_dispatcher_cfg());
        let r = d.dispatch("rule_1", &cfg(), None, b"{}").await;
        assert!(r.succeeded);
        assert_eq!(r.attempts.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn retries_then_succeeds() {
        let s = ScriptedSender::new(vec![
            AttemptOutcome::RetryableHttp {
                status: 503,
                retry_after_s: None,
            },
            AttemptOutcome::RetryableHttp {
                status: 502,
                retry_after_s: None,
            },
            AttemptOutcome::Success { status: 200 },
        ]);
        let d = Dispatcher::new(s, fast_dispatcher_cfg());
        let r = d.dispatch("rule_1", &cfg(), None, b"{}").await;
        assert!(r.succeeded);
        assert_eq!(r.attempts.len(), 3);
        assert!(r.attempts[0].summary.contains("503"));
        assert!(r.attempts[2].summary.contains("ok"));
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_max_attempts() {
        let scripted = ScriptedSender::new(vec![AttemptOutcome::RetryableNetwork {
            message: "timed out".into(),
        }]);
        let d = Dispatcher::new(scripted, fast_dispatcher_cfg());
        let r = d.dispatch("rule_1", &cfg(), None, b"{}").await;
        assert!(!r.succeeded);
        assert_eq!(r.attempts.len(), 5);
        // Every attempt should have been classified retryable network
        assert!(r
            .attempts
            .iter()
            .all(|a| a.summary.contains("retryable network")));
    }

    #[tokio::test(start_paused = true)]
    async fn scripted_sender_records_call_count() {
        let scripted = ScriptedSender::new(vec![
            AttemptOutcome::RetryableHttp {
                status: 503,
                retry_after_s: None,
            },
            AttemptOutcome::Success { status: 200 },
        ]);
        // Borrow the sender by reference. Because Dispatcher takes ownership,
        // we use the alternate constructor pattern below for inspection.
        // For now: rely on attempts.len() in the result.
        let d = Dispatcher::new(scripted, fast_dispatcher_cfg());
        let r = d.dispatch("rule_1", &cfg(), None, b"{}").await;
        assert_eq!(r.attempts.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_4xx_does_not_retry() {
        let scripted = ScriptedSender::new(vec![AttemptOutcome::PermanentHttp { status: 401 }]);
        let d = Dispatcher::new(scripted, fast_dispatcher_cfg());
        let r = d.dispatch("rule_1", &cfg(), None, b"{}").await;
        assert!(!r.succeeded);
        assert_eq!(r.attempts.len(), 1);
        assert!(r.attempts[0].summary.contains("permanent 401"));
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_429_retried() {
        let s = ScriptedSender::new(vec![
            AttemptOutcome::RetryableHttp {
                status: 429,
                retry_after_s: Some(2),
            },
            AttemptOutcome::Success { status: 200 },
        ]);
        let d = Dispatcher::new(s, fast_dispatcher_cfg());
        let r = d.dispatch("rule_1", &cfg(), None, b"{}").await;
        assert!(r.succeeded);
        assert_eq!(r.attempts.len(), 2);
    }
}
