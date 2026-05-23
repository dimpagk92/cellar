//! End-to-end: watcher rule → cel_act_gateway → WebhookHook (cellar-webhook
//! GatewayHook adapter) → WebhookService → Sender → captured payload.
//!
//! Proves the locked-trait wiring works: the gateway invokes the hook for
//! watcher fires, the hook delegates to the service, the service runs the
//! dispatcher loop, the dispatcher calls the sender, and the payload that
//! lands on the wire matches the WebhookPayload schema.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cel_act_gateway::test_support::{fake_action, AutoAllowBroker, RecordingActuator};
use cel_act_gateway::traits::StaticRules;
use cel_act_gateway::Gateway;
use cel_memory::{BasicMemoryProvider, MemoryProvider};
use cellar_types::{
    Action, ActionType, EventKind, Expression, InMemoryWatchlists, Operator, Rule, RuleKind,
    WebhookConfig,
};
use cellar_webhook::{
    AttemptOutcome, DispatcherConfig, GatewayHook, Sender, WebhookSecret, WebhookService,
    WebhookServiceConfig,
};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// Sender that captures the exact payload bytes for assertion + signals
/// completion via oneshot so the test doesn't sleep.
struct CapturingSender {
    captured: Arc<Mutex<Option<Vec<u8>>>>,
    done: Mutex<Option<oneshot::Sender<()>>>,
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
        if let Some(tx) = self.done.lock().unwrap().take() {
            let _ = tx.send(());
        }
        AttemptOutcome::Success { status: 200 }
    }
}

fn watcher_rule(webhook_id: &str) -> Rule {
    Rule {
        id: "rule_app_outside_allowlist".into(),
        name: "App outside allowlist".into(),
        nl_original: "notify when an app outside approved_apps launches".into(),
        kind: RuleKind::Watcher,
        enabled: true,
        match_expr: Expression::all(vec![Expression::leaf(
            "kind",
            Operator::Eq,
            json!(EventKind::AgentActionAttempted),
        )]),
        action: Action {
            action_type: ActionType::Webhook,
            webhook_id: Some(webhook_id.into()),
            timeout_s: None,
        },
        cooldown_seconds: 0,
        created_at: Utc::now(),
    }
}

fn webhook(id: &str) -> WebhookConfig {
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
async fn gateway_fan_out_to_webhook_service_delivers_payload() {
    // Memory subsystem (locked trait).
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());

    // Webhook service with a single configured webhook and a CapturingSender.
    let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let (done_tx, done_rx) = oneshot::channel();
    let sender = CapturingSender {
        captured: captured.clone(),
        done: Mutex::new(Some(done_tx)),
    };

    let mut webhooks = HashMap::new();
    webhooks.insert("default".into(), webhook("default"));

    let svc = Arc::new(WebhookService::spawn(
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
        |_r| {},
    ));

    // Wire the adapter into the gateway.
    let hook = Arc::new(GatewayHook::new(svc));
    let gw = Gateway::new(
        RecordingActuator::with_response(json!({"ok": true})),
        AutoAllowBroker,
        StaticRules(vec![watcher_rule("default")]),
        InMemoryWatchlists::default(),
        memory,
    )
    .with_webhook_hook(hook);

    // Drive an agent action; the watcher rule fires; the gateway fans out
    // to the webhook hook; the hook enqueues; the worker delivers.
    let mut action = fake_action("embedded", "fs.copy");
    action.action_args = json!({"source_path": "/x", "dest_path": "/y"});

    let outcome = gw.intercept(action).await.unwrap();
    assert!(outcome.executed(), "expected Executed, got {outcome:?}");

    // Wait for delivery.
    tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
        .await
        .expect("webhook delivery never completed")
        .expect("oneshot dropped without sending");

    // Inspect the captured payload.
    let bytes = captured.lock().unwrap().clone().expect("payload captured");
    let parsed: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["rule"]["id"], "rule_app_outside_allowlist");
    assert_eq!(parsed["rule"]["name"], "App outside allowlist");
    assert!(parsed["rule"]["nl_original"]
        .as_str()
        .unwrap()
        .contains("approved_apps"));
    assert_eq!(parsed["event"]["kind"], "agent_action_attempted");
    assert!(parsed["event"]["data"].is_object());
    assert!(parsed["fired_at"].is_string());
}

#[tokio::test]
async fn gateway_without_webhook_hook_still_runs() {
    // A gateway with no hook attached doesn't blow up; webhook fires are
    // recorded in memory but no delivery happens.
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoAllowBroker,
        StaticRules(vec![watcher_rule("default")]),
        InMemoryWatchlists::default(),
        memory,
    );

    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    assert!(outcome.executed());
}

#[tokio::test]
async fn gateway_non_webhook_fire_does_not_invoke_hook() {
    // Build a guard rule (require_confirmation). The gateway fires it but
    // should NOT call the webhook hook because action_type != Webhook.

    struct PanicHook;
    #[async_trait]
    impl cel_act_gateway::WebhookHook for PanicHook {
        async fn deliver(
            &self,
            _fire: &cel_act_gateway::FiredRuleSnapshot,
            _event: &cellar_types::Event,
        ) {
            panic!("webhook hook should NOT have been called for a guard rule fire");
        }
    }

    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let mut rule = watcher_rule("default");
    rule.kind = RuleKind::Guard;
    rule.action = Action {
        action_type: ActionType::RequireConfirmation,
        webhook_id: None,
        timeout_s: Some(60),
    };
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoAllowBroker,
        StaticRules(vec![rule]),
        InMemoryWatchlists::default(),
        memory,
    )
    .with_webhook_hook(Arc::new(PanicHook));

    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    // AutoAllowBroker means the action executes despite require_confirmation.
    assert!(outcome.executed());
}
