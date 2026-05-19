//! Webhook sender for Cellar — the output side of `watcher` rules.
//!
//! When a `watcher` rule fires with `action.type = Webhook`, the daemon
//! calls into this crate to deliver the payload. Two pieces:
//! - `Sender` trait — abstracts the actual HTTP. Production uses
//!   [`ReqwestSender`]; tests substitute mocks.
//! - `WebhookService` — owns a retry queue and a worker task. The matcher's
//!   post-fire hook calls `WebhookService::enqueue(rule_id, event)` and
//!   returns immediately; the worker handles delivery, retries, and the
//!   eventual dead-letter on persistent failure.
//!
//! Failure model:
//! - 2xx → success
//! - 408 / 429 / 5xx → retryable with exponential backoff (1s, 2s, 4s, 8s, 16s,
//!   capped at 16s, max 5 attempts)
//! - All other status codes → permanent failure, dead-letter
//! - Network / timeout errors → retryable like 5xx
//!
//! Secrets:
//! Webhook configs carry only an env-var *name* for the secret. The daemon
//! resolves the value at startup and passes it via [`WebhookSecret`].

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod attempt;
pub mod dispatcher;
pub mod sender;
pub mod service;

pub use attempt::{Attempt, AttemptOutcome, DispatchResult};
pub use dispatcher::{Dispatcher, DispatcherConfig};
pub use sender::{ReqwestSender, Sender, WebhookSecret};
pub use service::{EnqueueError, WebhookService, WebhookServiceConfig};
