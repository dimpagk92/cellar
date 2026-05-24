//! End-to-end test: a fired rule → service → dispatcher → mocked sender →
//! verified payload shape on the wire.

use cellar_types::{
    event::{Event, EventKind, EventSource},
    expression::{Expression, Operator},
    rule::{Action, ActionType, Rule, RuleKind},
    webhook::WebhookConfig,
};
use cellar_webhook::{
    AttemptOutcome, DispatchResult, Dispatcher, DispatcherConfig, Sender, WebhookSecret,
    WebhookService, WebhookServiceConfig,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

/// Sender that captures the exact payload bytes it received.
struct CapturingSender {
    captured: Arc<Mutex<Option<Vec<u8>>>>,
}

#[async_trait]
impl Sender for CapturingSender {
    async fn send(
        &self,
        _cfg: &WebhookConfig,
        _secret: Option<&WebhookSecret>,
        payload: &[u8],
    ) -> AttemptOutcome {
        *self.captured.lock().unwrap() = Some(payload.to_vec());
        AttemptOutcome::Success { status: 200 }
    }
}

fn watcher_rule() -> Rule {
    Rule {
        id: "rule_big_delete".into(),
        name: "Big delete".into(),
        nl_original: "notify when files >1GB are deleted from Documents".into(),
        kind: RuleKind::Watcher,
        enabled: true,
        match_expr: Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
        ]),
        action: Action {
            action_type: ActionType::Webhook,
            webhook_id: Some("default".into()),
            timeout_s: None,
        },
        cooldown_seconds: 60,
        created_at: Utc::now(),
    }
}

fn webhook() -> WebhookConfig {
    WebhookConfig {
        id: "default".into(),
        url: "https://example.test/hook".into(),
        headers: BTreeMap::new(),
        secret_header: None,
        secret_value_env: None,
        timeout_ms: 5000,
    }
}

#[tokio::test]
async fn fired_watcher_delivers_payload_with_rule_and_event() {
    let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let sender = CapturingSender {
        captured: captured.clone(),
    };

    let mut webhooks = HashMap::new();
    webhooks.insert("default".into(), webhook());

    let (tx, rx) = oneshot::channel();
    let mut tx_holder = Some(tx);

    let svc = WebhookService::spawn(
        WebhookServiceConfig {
            queue_capacity: 8,
            dispatcher: DispatcherConfig {
                max_attempts: 1,
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

    let rule = watcher_rule();
    let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
        .with_data("path", "~/Documents/big.pdf")
        .with_data("size_bytes", 2_147_483_648u64);

    svc.enqueue(&rule, &event).unwrap();
    let result = rx.await.unwrap();
    assert!(result.succeeded);

    let bytes = captured.lock().unwrap().clone().expect("payload captured");
    let parsed: Value = serde_json::from_slice(&bytes).unwrap();

    // Payload shape per cellar-types::webhook::WebhookPayload
    assert!(parsed["fired_at"].is_string());
    assert_eq!(parsed["rule"]["id"], "rule_big_delete");
    assert_eq!(parsed["rule"]["name"], "Big delete");
    assert_eq!(parsed["rule"]["nl_original"], rule.nl_original);
    assert_eq!(parsed["event"]["kind"], "file_deleted");
    assert_eq!(parsed["event"]["source"], "fsevents");
    assert_eq!(parsed["event"]["data"]["path"], "~/Documents/big.pdf");
    assert_eq!(parsed["event"]["data"]["size_bytes"], 2_147_483_648u64);
}

/// Dispatcher-only smoke test (no service / queue) with a sender that scripts
/// a few retryable failures before succeeding.
#[tokio::test(start_paused = true)]
async fn dispatcher_retries_until_success() {
    struct Scripted {
        outcomes: Mutex<Vec<AttemptOutcome>>,
    }

    #[async_trait]
    impl Sender for Scripted {
        async fn send(
            &self,
            _: &WebhookConfig,
            _: Option<&WebhookSecret>,
            _: &[u8],
        ) -> AttemptOutcome {
            let mut q = self.outcomes.lock().unwrap();
            if q.len() > 1 {
                q.remove(0)
            } else {
                q.first().cloned().unwrap()
            }
        }
    }

    let sender = Scripted {
        outcomes: Mutex::new(vec![
            AttemptOutcome::RetryableNetwork {
                message: "timeout".into(),
            },
            AttemptOutcome::RetryableHttp {
                status: 503,
                retry_after_s: None,
            },
            AttemptOutcome::Success { status: 200 },
        ]),
    };
    let d = Dispatcher::new(
        sender,
        DispatcherConfig {
            max_attempts: 5,
            base_backoff_ms: 1,
            max_backoff_ms: 2,
        },
    );
    let r = d.dispatch("rule_x", &webhook(), None, b"{}").await;
    assert!(r.succeeded);
    assert_eq!(r.attempts.len(), 3);
}
