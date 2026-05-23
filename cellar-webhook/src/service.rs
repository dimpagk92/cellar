//! The `WebhookService` — async queue + worker.
//!
//! The matcher's post-fire hook calls [`WebhookService::enqueue`] with the
//! rule id and the event. The service serializes the payload and pushes it
//! into a bounded mpsc channel; the background worker consumes from the
//! channel and runs each delivery through the [`crate::Dispatcher`].
//!
//! Backpressure: the channel is bounded. If the queue is full,
//! [`WebhookService::enqueue`] returns [`EnqueueError::QueueFull`] and the
//! caller logs + drops (matches the `retry_queue_max` cap in
//! `cellar-app-v1.md` §15). The daemon emits a `subscription.gap` notification
//! upstream so the user sees that fires were dropped.

use cellar_types::{
    event::Event, rule::Rule, webhook::WebhookConfig, webhook::WebhookPayload, webhook::WebhookRule,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::attempt::DispatchResult;
use crate::dispatcher::{Dispatcher, DispatcherConfig};
use crate::sender::{Sender, WebhookSecret};

/// Object-safe trait for hot-reloading webhook configurations at runtime.
///
/// Implemented by [`WebhookService<S>`]. The daemon stores an
/// `Arc<dyn WebhookRegistry>` in the IPC handler so `webhooks.add` and
/// `webhooks.remove` can register / unregister configs without restarting.
pub trait WebhookRegistry: Send + Sync {
    /// Register (or replace) a webhook configuration. Takes effect on the
    /// next delivery from the background worker.
    fn register_webhook(&self, config: cellar_types::webhook::WebhookConfig, secret: Option<WebhookSecret>);

    /// Remove a webhook configuration. In-flight jobs queued before the
    /// removal will log a "dropping job" warning and be discarded.
    fn unregister_webhook(&self, webhook_id: &str);
}

impl<S: Sender + Send + Sync + 'static> WebhookRegistry for WebhookService<S> {
    fn register_webhook(&self, config: cellar_types::webhook::WebhookConfig, secret: Option<WebhookSecret>) {
        WebhookService::register_webhook(self, config, secret);
    }

    fn unregister_webhook(&self, webhook_id: &str) {
        WebhookService::unregister_webhook(self, webhook_id);
    }
}

/// Per-service tunables.
#[derive(Debug, Clone)]
pub struct WebhookServiceConfig {
    /// Channel capacity (`retry_queue_max` from `monitor.toml`).
    pub queue_capacity: usize,
    /// Dispatcher tuning passed through to each delivery.
    pub dispatcher: DispatcherConfig,
}

impl Default for WebhookServiceConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 1000,
            dispatcher: DispatcherConfig::default(),
        }
    }
}

/// Reasons `enqueue` can refuse a fire.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnqueueError {
    /// The queue is at capacity. The caller should log + drop.
    #[error("webhook queue full (cap {capacity})")]
    QueueFull {
        /// Configured capacity.
        capacity: usize,
    },
    /// The matched rule references a webhook id that isn't registered.
    #[error("unknown webhook id: {0}")]
    UnknownWebhook(String),
    /// Payload serialization failed (should be impossible with well-formed types).
    #[error("serialize: {0}")]
    Serialize(String),
}

/// One item in the queue.
struct Job {
    rule_id: String,
    webhook_id: String,
    payload: Vec<u8>,
}

/// The service handle the daemon holds.
///
/// `webhooks` and `secrets` are wrapped in `Arc<RwLock<...>>` so they can be
/// mutated at runtime via [`WebhookService::register_webhook`] and
/// [`WebhookService::unregister_webhook`] without restarting the daemon.
/// The background worker holds clones of these arcs and reads through the
/// lock on every job, so mutations are visible immediately to the next
/// dequeued delivery.
pub struct WebhookService<S: Sender + 'static> {
    cfg: WebhookServiceConfig,
    webhooks: Arc<RwLock<HashMap<String, WebhookConfig>>>,
    secrets: Arc<RwLock<HashMap<String, WebhookSecret>>>,
    sender: Arc<S>,
    tx: mpsc::Sender<Job>,
}

impl<S: Sender + 'static> WebhookService<S> {
    /// Construct and spawn the worker task.
    ///
    /// `webhooks` maps webhook id → config (loaded from SQLite at startup).
    /// `secrets` maps webhook id → resolved [`WebhookSecret`] (resolved from
    /// env vars at startup). Webhooks without a secret are absent from this map.
    pub fn spawn(
        cfg: WebhookServiceConfig,
        webhooks: HashMap<String, WebhookConfig>,
        secrets: HashMap<String, WebhookSecret>,
        sender: S,
        mut on_result: impl FnMut(DispatchResult) + Send + 'static,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<Job>(cfg.queue_capacity);
        let webhooks = Arc::new(RwLock::new(webhooks));
        let secrets = Arc::new(RwLock::new(secrets));
        let sender = Arc::new(sender);

        let dispatcher_cfg = cfg.dispatcher.clone();
        let webhooks_for_worker = webhooks.clone();
        let secrets_for_worker = secrets.clone();
        let sender_for_worker = sender.clone();

        tokio::spawn(async move {
            // We construct one Dispatcher per job because Dispatcher owns its
            // sender; with Arc<S> we re-borrow on each call. To avoid that, we
            // use an adapter that holds an Arc<S> internally.
            let dispatcher = Dispatcher::new(
                ArcSender {
                    inner: sender_for_worker.clone(),
                },
                dispatcher_cfg,
            );
            while let Some(job) = rx.recv().await {
                // Clone config + secret out of the RwLock so the lock guard
                // doesn't live across the async dispatch boundary.
                let cfg_opt = webhooks_for_worker
                    .read()
                    .expect("webhooks rwlock poisoned")
                    .get(&job.webhook_id)
                    .cloned();
                let Some(cfg_val) = cfg_opt else {
                    tracing::warn!(
                        webhook_id = %job.webhook_id,
                        rule_id = %job.rule_id,
                        "dropping job for unknown webhook id"
                    );
                    continue;
                };
                let secret_opt = secrets_for_worker
                    .read()
                    .expect("webhook secrets rwlock poisoned")
                    .get(&job.webhook_id)
                    .cloned();
                let result = dispatcher
                    .dispatch(&job.rule_id, &cfg_val, secret_opt.as_ref(), &job.payload)
                    .await;
                on_result(result);
            }
        });

        Self {
            cfg,
            webhooks,
            secrets,
            sender,
            tx,
        }
    }

    /// Register (or replace) a webhook config at runtime. Takes effect for the
    /// next delivery — the background worker reads through the `RwLock` on each
    /// job so no daemon restart is needed.
    pub fn register_webhook(&self, config: WebhookConfig, secret: Option<WebhookSecret>) {
        let id = config.id.clone();
        self.webhooks
            .write()
            .expect("webhooks rwlock poisoned")
            .insert(id.clone(), config);
        match secret {
            Some(s) => {
                self.secrets
                    .write()
                    .expect("webhook secrets rwlock poisoned")
                    .insert(id.clone(), s);
            }
            None => {
                // Remove any stale secret if the new config has none.
                self.secrets
                    .write()
                    .expect("webhook secrets rwlock poisoned")
                    .remove(&id);
            }
        }
        tracing::info!(webhook_id = %id, "webhook registered (hot-reload)");
    }

    /// Remove a webhook config at runtime. Any in-flight jobs for this id that
    /// are already in the queue will log a "dropping job for unknown webhook id"
    /// warning when the worker processes them.
    pub fn unregister_webhook(&self, webhook_id: &str) {
        self.webhooks
            .write()
            .expect("webhooks rwlock poisoned")
            .remove(webhook_id);
        self.secrets
            .write()
            .expect("webhook secrets rwlock poisoned")
            .remove(webhook_id);
        tracing::info!(webhook_id = %webhook_id, "webhook unregistered (hot-reload)");
    }

    /// Enqueue a delivery for a fired rule. Returns immediately.
    ///
    /// Serializes the payload up-front so the worker doesn't hold borrows.
    /// Delegates to [`Self::enqueue_parts`] after destructuring the rule.
    pub fn enqueue(&self, rule: &Rule, event: &Event) -> Result<(), EnqueueError> {
        let webhook_id = rule.action.webhook_id.as_deref().unwrap_or("default");
        self.enqueue_parts(&rule.id, &rule.name, &rule.nl_original, webhook_id, event)
    }

    /// Lower-level enqueue that takes the rule pieces the [`WebhookPayload`]
    /// needs (id / name / NL original) plus the webhook id. Used by the
    /// gateway's [`WebhookHook`] adapter, which only has a
    /// `FiredRuleSnapshot` — not a full [`Rule`] — at the fire site.
    ///
    /// [`WebhookHook`]: cel_act_gateway::WebhookHook
    pub fn enqueue_parts(
        &self,
        rule_id: &str,
        rule_name: &str,
        rule_nl_original: &str,
        webhook_id: &str,
        event: &Event,
    ) -> Result<(), EnqueueError> {
        if !self
            .webhooks
            .read()
            .expect("webhooks rwlock poisoned")
            .contains_key(webhook_id)
        {
            return Err(EnqueueError::UnknownWebhook(webhook_id.to_string()));
        }

        let payload = WebhookPayload {
            fired_at: Utc::now(),
            rule: WebhookRule {
                id: rule_id,
                name: rule_name,
                nl_original: rule_nl_original,
            },
            event,
        };
        let bytes =
            serde_json::to_vec(&payload).map_err(|e| EnqueueError::Serialize(e.to_string()))?;

        let job = Job {
            rule_id: rule_id.to_string(),
            webhook_id: webhook_id.to_string(),
            payload: bytes,
        };

        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(EnqueueError::QueueFull {
                capacity: self.cfg.queue_capacity,
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(EnqueueError::QueueFull { capacity: 0 })
            }
        }
    }

    /// Snapshot of the configured webhook ids (for `cellar daemon doctor`).
    pub fn webhook_ids(&self) -> Vec<String> {
        self.webhooks
            .read()
            .expect("webhooks rwlock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// Reference to the sender (testing aid).
    pub fn sender(&self) -> &Arc<S> {
        &self.sender
    }
}

// Adapter to satisfy the `Dispatcher<S: Sender>` ownership model while
// letting the service share a single Sender across many jobs.
struct ArcSender<S: Sender> {
    inner: Arc<S>,
}

#[async_trait::async_trait]
impl<S: Sender> Sender for ArcSender<S> {
    async fn send(
        &self,
        config: &WebhookConfig,
        secret: Option<&WebhookSecret>,
        payload: &[u8],
    ) -> crate::attempt::AttemptOutcome {
        self.inner.send(config, secret, payload).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attempt::AttemptOutcome;
    use async_trait::async_trait;
    use cellar_types::{
        event::{Event, EventKind, EventSource},
        expression::{Expression, Operator},
        rule::{Action, ActionType, RuleKind},
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    /// Sender that always succeeds and counts calls via Arc<Mutex>.
    struct CountingSender {
        count: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl Sender for CountingSender {
        async fn send(
            &self,
            _: &WebhookConfig,
            _: Option<&WebhookSecret>,
            _: &[u8],
        ) -> AttemptOutcome {
            *self.count.lock().unwrap() += 1;
            AttemptOutcome::Success { status: 200 }
        }
    }

    fn rule_with_webhook(webhook_id: &str) -> Rule {
        Rule {
            id: "rule_x".into(),
            name: "x".into(),
            nl_original: "x".into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            action: Action {
                action_type: ActionType::Webhook,
                webhook_id: Some(webhook_id.into()),
                timeout_s: None,
            },
            cooldown_seconds: 0,
            created_at: Utc::now(),
        }
    }

    fn webhook_cfg(id: &str) -> WebhookConfig {
        WebhookConfig {
            id: id.into(),
            url: "https://example.test/hook".into(),
            headers: BTreeMap::new(),
            secret_header: None,
            secret_value_env: None,
            timeout_ms: 5000,
        }
    }

    #[tokio::test]
    async fn enqueue_and_dispatch_success() {
        let count = Arc::new(Mutex::new(0u32));
        let sender = CountingSender {
            count: count.clone(),
        };

        let mut webhooks = HashMap::new();
        webhooks.insert("default".into(), webhook_cfg("default"));

        let (tx, rx) = oneshot::channel();
        let mut tx_holder = Some(tx);

        let svc = WebhookService::spawn(
            WebhookServiceConfig {
                queue_capacity: 8,
                dispatcher: DispatcherConfig {
                    max_attempts: 3,
                    base_backoff_ms: 1,
                    max_backoff_ms: 1,
                },
            },
            webhooks,
            HashMap::new(),
            sender,
            move |r: DispatchResult| {
                if let Some(tx) = tx_holder.take() {
                    let _ = tx.send(r);
                }
            },
        );

        let rule = rule_with_webhook("default");
        let event =
            Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/tmp/x");
        svc.enqueue(&rule, &event).unwrap();

        let result = rx.await.unwrap();
        assert!(result.succeeded);
        assert_eq!(result.rule_id, "rule_x");
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unknown_webhook_id_errors_synchronously() {
        let count = Arc::new(Mutex::new(0u32));
        let sender = CountingSender {
            count: count.clone(),
        };
        let webhooks: HashMap<String, WebhookConfig> = HashMap::new(); // empty
        let svc = WebhookService::spawn(
            WebhookServiceConfig::default(),
            webhooks,
            HashMap::new(),
            sender,
            |_r: DispatchResult| {},
        );

        let rule = rule_with_webhook("doesnotexist");
        let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let err = svc.enqueue(&rule, &event).unwrap_err();
        assert_eq!(err, EnqueueError::UnknownWebhook("doesnotexist".into()));
        assert_eq!(*count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn register_webhook_hot_reload_delivers_after_add() {
        // Start with an empty service — no webhooks at startup.
        let count = Arc::new(Mutex::new(0u32));
        let sender = CountingSender {
            count: count.clone(),
        };
        let (tx, rx) = oneshot::channel();
        let mut tx_holder = Some(tx);

        let svc = WebhookService::spawn(
            WebhookServiceConfig {
                queue_capacity: 8,
                dispatcher: DispatcherConfig {
                    max_attempts: 3,
                    base_backoff_ms: 1,
                    max_backoff_ms: 1,
                },
            },
            HashMap::new(),  // empty
            HashMap::new(),
            sender,
            move |r: DispatchResult| {
                if let Some(tx) = tx_holder.take() {
                    let _ = tx.send(r);
                }
            },
        );

        // Before registration, enqueue fails.
        let rule = rule_with_webhook("added");
        let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        assert!(svc.enqueue(&rule, &event).is_err());

        // Hot-add the webhook.
        svc.register_webhook(webhook_cfg("added"), None);

        // Now enqueue succeeds.
        svc.enqueue(&rule, &event).unwrap();

        let result = rx.await.unwrap();
        assert!(result.succeeded);
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn unregister_webhook_prevents_future_enqueues() {
        let count = Arc::new(Mutex::new(0u32));
        let sender = CountingSender {
            count: count.clone(),
        };
        let mut webhooks = HashMap::new();
        webhooks.insert("to_remove".into(), webhook_cfg("to_remove"));

        let svc = WebhookService::spawn(
            WebhookServiceConfig::default(),
            webhooks,
            HashMap::new(),
            sender,
            |_: DispatchResult| {},
        );

        // Before removal, enqueue works.
        let rule = rule_with_webhook("to_remove");
        let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        svc.enqueue(&rule, &event).unwrap();

        // Hot-remove.
        svc.unregister_webhook("to_remove");

        // After removal, enqueue errors.
        let err = svc.enqueue(&rule, &event).unwrap_err();
        assert_eq!(err, EnqueueError::UnknownWebhook("to_remove".into()));
    }
}
