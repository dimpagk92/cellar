//! Attempt outcomes and the eventual delivery result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Outcome of a single HTTP attempt. Classifying retryable vs permanent
/// happens at this layer so the dispatcher doesn't have to know HTTP details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// 2xx response.
    Success {
        /// Status code.
        status: u16,
    },
    /// 408 / 429 / 5xx. Eligible for retry.
    RetryableHttp {
        /// Status code.
        status: u16,
        /// Server-supplied `Retry-After` hint, if any.
        retry_after_s: Option<u64>,
    },
    /// 4xx other than 408/429. Permanent.
    PermanentHttp {
        /// Status code.
        status: u16,
    },
    /// Connect / timeout / request error. Eligible for retry.
    RetryableNetwork {
        /// Underlying error message.
        message: String,
    },
    /// Non-retryable transport error.
    PermanentOther {
        /// Underlying error message.
        message: String,
    },
}

impl AttemptOutcome {
    /// True when the dispatcher should schedule a retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AttemptOutcome::RetryableHttp { .. } | AttemptOutcome::RetryableNetwork { .. }
        )
    }

    /// True when this is a terminal success.
    pub fn is_success(&self) -> bool {
        matches!(self, AttemptOutcome::Success { .. })
    }
}

/// A single attempt record (for the daemon's fired-log / retry queue).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attempt {
    /// 1-indexed attempt number for this delivery.
    pub attempt_number: u32,
    /// When the attempt was made.
    pub attempted_at: DateTime<Utc>,
    /// Outcome — only the public-safe summary, not the full message body.
    pub summary: String,
    /// True if the outcome was a 2xx.
    pub succeeded: bool,
}

/// Terminal result of a webhook delivery (success, or last failure after
/// retries exhausted). Persisted in the fired-log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DispatchResult {
    /// Webhook config id this delivery targeted.
    pub webhook_id: String,
    /// Rule id whose firing triggered the delivery.
    pub rule_id: String,
    /// All attempts that were made, in order.
    pub attempts: Vec<Attempt>,
    /// True if delivery ultimately succeeded.
    pub succeeded: bool,
    /// Total wall-clock time from first attempt to terminal outcome.
    pub elapsed: chrono::Duration,
}
