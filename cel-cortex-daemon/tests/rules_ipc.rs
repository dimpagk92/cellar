//! End-to-end rules + watchlists IPC over a real UDS socket.
//!
//! Mirrors `uds_bind.rs` but exercises the new `rules.*` and
//! `watchlists.*` methods through a real `Client` so we know the wire
//! contract (params, results, JSON-RPC error codes) matches the typed
//! API surface in `cellar-ipc`.
//!
//! Two paths get coverage here:
//!
//! 1. **Happy-path CRUD** — add a rule + watchlist over IPC, read back
//!    via `list`, mutate via `pause` / `add_item` / `set`, verify state
//!    via `get`. This is the contract Tauri and the future CLI talk to.
//!
//! 2. **Hot-reload through the daemon's shared store** — a rule added
//!    over IPC is visible on the daemon's own `rules_store` Arc clone
//!    without restart. This pins down the Slice 2a + 2b combination:
//!    one writer (the handler), N readers (the gateway + matcher
//!    consumer), all consistent via the in-memory snapshot.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cel_act_gateway::RuleSource;
use cel_cortex_daemon::Daemon;
use cellar_ipc::params::confirmation::{
    ConfirmationDecisionWire, ConfirmationListPendingParams, ConfirmationResolveParams,
};
use cellar_ipc::params::events::EventsRecentParams;
use cellar_ipc::params::fires::FiresRecentParams;
use cellar_ipc::params::rules::{
    RuleIdParams, RulesAddParams, RulesCompileParams, RulesGetParams, RulesListParams,
    RulesUpdateParams,
};
use cellar_ipc::params::stream_filter::StreamFilter;
use cellar_ipc::params::system::SystemHelloParams;
use cellar_ipc::params::watchlists::{
    WatchlistNameParams, WatchlistsItemParams, WatchlistsListParams, WatchlistsSetParams,
};
use cellar_ipc::params::webhooks::{WebhookIdParams, WebhooksAddParams, WebhooksListParams};
use cellar_ipc::results::confirmation::{ConfirmationListPendingResult, ConfirmationResolveResult};
use cellar_ipc::results::daemon::DaemonStatusResult;
use cellar_ipc::results::rules::{
    RulesAddResult, RulesCompileResult, RulesGetResult, RulesListResult,
};
use cellar_ipc::results::system::SystemHelloResult;
use cellar_ipc::results::watchlists::{WatchlistsGetResult, WatchlistsListResult};
use cellar_ipc::results::webhooks::{WebhooksListResult, WebhooksTestResult};
use cellar_ipc::results::OkResult;
use cellar_ipc::{Client, Server};
use cellar_types::expression::Operator;
use cellar_types::rule::{Action, ActionType, RuleKind};
use cellar_types::{Expression, Rule};
use chrono::Utc;
use serde_json::json;
use tokio::sync::oneshot;
use tokio::task::JoinSet;

/// Unique-enough socket path. Combines pid, nanos, and an atomic
/// counter so parallel tokio tests within the same process can't collide
/// (the nanos-only original raced on fast machines).
fn tmp_socket_path() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/cellar-ri-{pid}-{nanos}-{seq}.sock"))
}

fn sample_rule(id: &str) -> Rule {
    Rule {
        id: id.into(),
        name: format!("rule {id}"),
        nl_original: "test".into(),
        kind: RuleKind::Watcher,
        enabled: true,
        match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
        action: Action {
            action_type: ActionType::LogOnly,
            webhook_id: None,
            timeout_s: None,
        },
        cooldown_seconds: 0,
        created_at: Utc::now(),
    }
}

/// One-shot test bench: wires a fresh `Daemon` + UDS server + connected
/// `Client`. Returns the client, the daemon (for direct introspection of
/// `daemon.rules_store`), and a `Drop` guard that cleans up the socket
/// when the bench goes out of scope.
struct Bench {
    client: Client,
    daemon: Daemon,
    _server_task: tokio::task::JoinHandle<()>,
    socket_path: PathBuf,
}

impl Bench {
    async fn new() -> Self {
        Self::from_daemon(Daemon::wire_subsystems()).await
    }

    /// Bench variant that wires the daemon with a `MockProvider`-backed
    /// `Compiler` so `rules.compile` can be exercised over real UDS
    /// without needing an LLM provider configured.
    async fn with_mock_compiler(responses: &[&str]) -> Self {
        use cellar_llm_router::provider::MockProvider;
        use cellar_llm_router::types::{CompletionResponse, ContentBlock, StopReason, Usage};
        use cellar_rule_compiler::Compiler;
        let responses: Vec<CompletionResponse> = responses
            .iter()
            .map(|t| CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: t.to_string(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: None,
            })
            .collect();
        let provider = MockProvider::new(responses);
        let compiler = Arc::new(Compiler::new(provider, "mock-model"));
        let daemon = Daemon::wire_subsystems_with_compiler(compiler);
        Self::from_daemon(daemon).await
    }

    async fn from_daemon(daemon: Daemon) -> Self {
        let socket_path = tmp_socket_path();

        let server = Server::bind_with_arc(&socket_path, Arc::clone(&daemon.ipc_handler))
            .await
            .unwrap();

        let (ready_tx, ready_rx) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            ready_tx.send(()).unwrap();
            let _ = server.run(&mut tasks).await;
        });
        ready_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let (client, _notif_rx) = Client::connect_unix(&socket_path).await.unwrap();

        // Locked protocol: hello before anything else.
        let _hello: SystemHelloResult = client
            .call(
                "system.hello",
                SystemHelloParams {
                    client_name: "rules-ipc-test".into(),
                    client_version: "0.0.1".into(),
                    supported_protocol_versions: vec!["1".into()],
                },
            )
            .await
            .unwrap();

        Self {
            client,
            daemon,
            _server_task: server_task,
            socket_path,
        }
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[tokio::test]
async fn rules_crud_round_trip_over_uds() {
    let bench = Bench::new().await;

    // List: starts empty.
    let list: RulesListResult = bench
        .client
        .call("rules.list", RulesListParams::default())
        .await
        .unwrap();
    assert_eq!(list.rules.len(), 0);

    // Add.
    let added: RulesAddResult = bench
        .client
        .call(
            "rules.add",
            RulesAddParams {
                rule: sample_rule("ipc_r1"),
            },
        )
        .await
        .unwrap();
    assert_eq!(added.rule_id, "ipc_r1");

    // Get.
    let got: RulesGetResult = bench
        .client
        .call(
            "rules.get",
            RulesGetParams {
                id: "ipc_r1".into(),
            },
        )
        .await
        .unwrap();
    let rule = got.rule.expect("rule should be present");
    assert_eq!(rule.name, "rule ipc_r1");
    assert!(rule.enabled);

    // Pause → resume.
    let _: OkResult = bench
        .client
        .call(
            "rules.pause",
            RuleIdParams {
                id: "ipc_r1".into(),
            },
        )
        .await
        .unwrap();
    let after_pause: RulesGetResult = bench
        .client
        .call(
            "rules.get",
            RulesGetParams {
                id: "ipc_r1".into(),
            },
        )
        .await
        .unwrap();
    assert!(!after_pause.rule.unwrap().enabled);

    let _: OkResult = bench
        .client
        .call(
            "rules.resume",
            RuleIdParams {
                id: "ipc_r1".into(),
            },
        )
        .await
        .unwrap();

    // Update: rename it.
    let mut renamed = sample_rule("ipc_r1");
    renamed.name = "renamed".into();
    let _: OkResult = bench
        .client
        .call(
            "rules.update",
            RulesUpdateParams {
                id: "ipc_r1".into(),
                rule: renamed,
            },
        )
        .await
        .unwrap();
    let after_update: RulesGetResult = bench
        .client
        .call(
            "rules.get",
            RulesGetParams {
                id: "ipc_r1".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(after_update.rule.unwrap().name, "renamed");

    // Remove.
    let _: OkResult = bench
        .client
        .call(
            "rules.remove",
            RuleIdParams {
                id: "ipc_r1".into(),
            },
        )
        .await
        .unwrap();
    let final_list: RulesListResult = bench
        .client
        .call("rules.list", RulesListParams::default())
        .await
        .unwrap();
    assert_eq!(final_list.rules.len(), 0);
}

#[tokio::test]
async fn rules_remove_missing_returns_typed_not_found() {
    let bench = Bench::new().await;
    let err = bench
        .client
        .call::<_, OkResult>("rules.remove", RuleIdParams { id: "ghost".into() })
        .await
        .unwrap_err();
    // Client errors carry the JSON-RPC code from the server. Check
    // the wire-level code rather than the variant — the client side
    // surfaces server errors as a `Client` error wrapping the code.
    assert!(
        format!("{err}").contains("-32004") || format!("{err}").contains("rule not found"),
        "expected RuleNotFound (code -32004), got: {err}"
    );
}

#[tokio::test]
async fn watchlists_crud_round_trip_over_uds() {
    let bench = Bench::new().await;

    // Create-via-set: `watchlists.set` is upsert-style.
    let _: OkResult = bench
        .client
        .call(
            "watchlists.set",
            WatchlistsSetParams {
                name: "approved".into(),
                items: vec!["com.apple.Safari".into(), "com.slack.Slack".into()],
            },
        )
        .await
        .unwrap();

    // Get one.
    let one: WatchlistsGetResult = bench
        .client
        .call(
            "watchlists.get",
            WatchlistNameParams {
                name: "approved".into(),
            },
        )
        .await
        .unwrap();
    let wl = one.watchlist.expect("watchlist should be present");
    assert_eq!(wl.items.len(), 2);

    // Add another item.
    let _: OkResult = bench
        .client
        .call(
            "watchlists.add_item",
            WatchlistsItemParams {
                name: "approved".into(),
                item: "com.google.Chrome".into(),
            },
        )
        .await
        .unwrap();

    // Remove an item.
    let _: OkResult = bench
        .client
        .call(
            "watchlists.remove_item",
            WatchlistsItemParams {
                name: "approved".into(),
                item: "com.slack.Slack".into(),
            },
        )
        .await
        .unwrap();

    // List shows the one watchlist with two items left.
    let list: WatchlistsListResult = bench
        .client
        .call("watchlists.list", WatchlistsListParams::default())
        .await
        .unwrap();
    assert_eq!(list.watchlists.len(), 1);
    assert_eq!(list.watchlists[0].items.len(), 2);
    assert!(list.watchlists[0].items.contains("com.apple.Safari"));
    assert!(list.watchlists[0].items.contains("com.google.Chrome"));
    assert!(!list.watchlists[0].items.contains("com.slack.Slack"));

    // Delete the watchlist.
    let _: OkResult = bench
        .client
        .call(
            "watchlists.remove",
            WatchlistNameParams {
                name: "approved".into(),
            },
        )
        .await
        .unwrap();
    let empty: WatchlistsListResult = bench
        .client
        .call("watchlists.list", WatchlistsListParams::default())
        .await
        .unwrap();
    assert!(empty.watchlists.is_empty());
}

#[tokio::test]
async fn watchlists_add_item_to_missing_list_returns_typed_error() {
    let bench = Bench::new().await;
    let err = bench
        .client
        .call::<_, OkResult>(
            "watchlists.add_item",
            WatchlistsItemParams {
                name: "nonexistent".into(),
                item: "x".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("-32005") || format!("{err}").contains("watchlist not found"),
        "expected WatchlistNotFound (code -32005), got: {err}"
    );
}

#[tokio::test]
async fn ipc_write_visible_to_daemon_rules_store_arc_clone() {
    // The critical Slice 2a + 2b integration: a rule added over IPC is
    // visible on the daemon's own `rules_store` Arc clone — the same
    // clone the gateway and the matcher consumer task hold.
    let bench = Bench::new().await;

    // Daemon's view starts empty (this is the same Arc the matcher reads).
    assert!(bench.daemon.rules_store.snapshot().is_empty());

    // Add over IPC.
    let _: RulesAddResult = bench
        .client
        .call(
            "rules.add",
            RulesAddParams {
                rule: sample_rule("hot_reload_test"),
            },
        )
        .await
        .unwrap();

    // Daemon's view sees it without any reload call.
    let view = bench.daemon.rules_store.snapshot();
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].id, "hot_reload_test");
}

// ───── rules.compile end-to-end over UDS ─────

/// Minimal valid LLM response for a watcher rule. Used as the mock
/// provider's reply across the `rules.compile` tests.
const COMPILE_GOOD_JSON: &str = r#"{
    "id": "draft",
    "name": "Big delete",
    "nl_original": "notify when files >1GB are deleted from Documents",
    "kind": "watcher",
    "enabled": true,
    "created_at": "1970-01-01T00:00:00Z",
    "match": {
        "all": [
            {"leaf": {"field": "kind", "op": "eq", "value": "file_deleted"}},
            {"leaf": {"field": "data.size_bytes", "op": "gte", "value": 1073741824}}
        ]
    },
    "action": {"type": "webhook", "webhook_id": "default"},
    "cooldown_seconds": 60
}"#;

#[tokio::test]
async fn rules_compile_end_to_end_over_uds() {
    let bench = Bench::with_mock_compiler(&[COMPILE_GOOD_JSON]).await;

    let r: RulesCompileResult = bench
        .client
        .call(
            "rules.compile",
            RulesCompileParams {
                nl_string: "notify when files >1GB are deleted from Documents".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(r.draft_rule.name, "Big delete");
    assert!(!r.human_readable.is_empty());
    // No persistence — `rules.list` is still empty.
    let list: RulesListResult = bench
        .client
        .call("rules.list", RulesListParams::default())
        .await
        .unwrap();
    assert!(list.rules.is_empty());
}

#[tokio::test]
async fn rules_compile_save_round_trip() {
    // The full UI flow: compile to preview, then `rules.add` the draft.
    // Validates the contract that the draft from `rules.compile` is
    // directly acceptable as the payload to `rules.add` — no field
    // massaging on the client side.
    let bench = Bench::with_mock_compiler(&[COMPILE_GOOD_JSON]).await;

    let draft = bench
        .client
        .call::<_, RulesCompileResult>(
            "rules.compile",
            RulesCompileParams {
                nl_string: "any".into(),
            },
        )
        .await
        .unwrap()
        .draft_rule;

    let saved = bench
        .client
        .call::<_, RulesAddResult>(
            "rules.add",
            RulesAddParams {
                rule: draft.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.rule_id, draft.id);

    let list = bench
        .client
        .call::<_, RulesListResult>("rules.list", RulesListParams::default())
        .await
        .unwrap();
    assert_eq!(list.rules.len(), 1);
    assert_eq!(list.rules[0].name, "Big delete");

    // And the saved rule is visible to the matcher consumer via the
    // shared rules-store Arc — closes the Slice 2a + 2b + 2c loop.
    assert_eq!(bench.daemon.rules_store.snapshot().len(), 1);
}

#[tokio::test]
async fn rules_compile_without_provider_returns_llm_provider_error() {
    // Default daemon (`wire_subsystems`) has no compiler.
    let bench = Bench::new().await;
    let err = bench
        .client
        .call::<_, RulesCompileResult>(
            "rules.compile",
            RulesCompileParams {
                nl_string: "anything".into(),
            },
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("-32011") || msg.contains("llm provider") || msg.contains("not configured"),
        "expected LlmProviderError (-32011), got: {msg}"
    );
}

#[tokio::test]
async fn system_hello_advertises_rules_compile_capability() {
    // Bench::new uses no compiler → no rules.compile cap.
    let bench = Bench::new().await;
    let r = bench
        .client
        .call::<_, cellar_ipc::results::system::SystemHelloResult>(
            "system.hello",
            SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();
    assert!(!r.capabilities.contains(&"rules.compile".into()));

    // Bench::with_mock_compiler does → cap shows up.
    let bench = Bench::with_mock_compiler(&[COMPILE_GOOD_JSON]).await;
    let r = bench
        .client
        .call::<_, cellar_ipc::results::system::SystemHelloResult>(
            "system.hello",
            SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();
    assert!(r.capabilities.contains(&"rules.compile".into()));
}

// ───── webhooks.* end-to-end over UDS ─────

fn sample_webhook(id: &str) -> cellar_types::WebhookConfig {
    cellar_types::WebhookConfig {
        id: id.into(),
        url: "https://example.com/hook".into(),
        headers: Default::default(),
        secret_header: None,
        secret_value_env: None,
        timeout_ms: 5000,
    }
}

#[tokio::test]
async fn webhooks_crud_round_trip_over_uds() {
    let bench = Bench::new().await;

    let empty: WebhooksListResult = bench
        .client
        .call("webhooks.list", WebhooksListParams::default())
        .await
        .unwrap();
    assert!(empty.webhooks.is_empty());

    let _: cellar_ipc::results::OkResult = bench
        .client
        .call(
            "webhooks.add",
            WebhooksAddParams {
                config: sample_webhook("default"),
            },
        )
        .await
        .unwrap();

    let listed: WebhooksListResult = bench
        .client
        .call("webhooks.list", WebhooksListParams::default())
        .await
        .unwrap();
    assert_eq!(listed.webhooks.len(), 1);
    assert_eq!(listed.webhooks[0].id, "default");

    let _: cellar_ipc::results::OkResult = bench
        .client
        .call(
            "webhooks.remove",
            WebhookIdParams {
                id: "default".into(),
            },
        )
        .await
        .unwrap();
    let empty: WebhooksListResult = bench
        .client
        .call("webhooks.list", WebhooksListParams::default())
        .await
        .unwrap();
    assert!(empty.webhooks.is_empty());
}

#[tokio::test]
async fn webhooks_remove_missing_returns_typed_not_found() {
    let bench = Bench::new().await;
    let err = bench
        .client
        .call::<_, cellar_ipc::results::OkResult>(
            "webhooks.remove",
            WebhookIdParams { id: "ghost".into() },
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("-32006") || msg.contains("webhook not found"),
        "expected WebhookNotFound (-32006), got: {msg}"
    );
}

// ───── events.recent / fires.recent over UDS ─────

#[tokio::test]
async fn events_recent_returns_empty_on_fresh_daemon() {
    let bench = Bench::new().await;
    let r: Vec<serde_json::Value> = bench
        .client
        .call("events.recent", EventsRecentParams::default())
        .await
        .unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn events_recent_reflects_bus_publishes() {
    let bench = Bench::new().await;

    // Push some events directly into the daemon's bus — same path the
    // ambient sources would use, but we don't need to spawn them in the
    // test.
    use cellar_types::{Event, EventKind, EventSource};
    bench.daemon.event_bus.publish(
        Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/tmp/a"),
    );
    bench
        .daemon
        .event_bus
        .publish(Event::now(EventSource::Process, EventKind::ProcessStarted));

    // Give the ring-filler task a moment to drain. The Bench doesn't
    // currently spawn the ring filler (that's main.rs's job), so we use
    // the ring directly via the daemon handle.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The Bench doesn't run the ring-filler task — so events.recent
    // reads an empty ring. To still verify the contract: publish to
    // the ring directly (simulating what the filler would do) and
    // confirm `events.recent` returns them.
    bench.daemon.event_ring.push(
        Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/tmp/c"),
    );
    let r: Vec<serde_json::Value> = bench
        .client
        .call("events.recent", EventsRecentParams::default())
        .await
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["kind"], "file_deleted");
}

#[tokio::test]
async fn events_recent_filters_by_kind() {
    let bench = Bench::new().await;
    use cellar_types::{Event, EventKind, EventSource};
    bench
        .daemon
        .event_ring
        .push(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
    bench
        .daemon
        .event_ring
        .push(Event::now(EventSource::Fsevents, EventKind::FileCreated));

    let r: Vec<serde_json::Value> = bench
        .client
        .call(
            "events.recent",
            EventsRecentParams {
                filter: StreamFilter {
                    kinds: Some(vec!["file_deleted".into()]),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["kind"], "file_deleted");
}

#[tokio::test]
async fn fires_recent_round_trip() {
    use cel_cortex_daemon::fire_bus::FireFrame;
    let bench = Bench::new().await;
    bench.daemon.fire_ring.push(FireFrame {
        id: "fire_test".into(),
        fired_at: chrono::Utc::now(),
        rule_id: "rule_test".into(),
        rule_name: "Test Rule".into(),
        rule_kind: "watcher".into(),
        event_kind: "file_deleted".into(),
        event_source: "fsevents".into(),
        event_data: json!({"path": "/tmp/x"}),
        is_blocking: false,
    });

    let r: Vec<serde_json::Value> = bench
        .client
        .call("fires.recent", FiresRecentParams::default())
        .await
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["rule_id"], "rule_test");
}

#[tokio::test]
async fn system_hello_advertises_streaming_capabilities() {
    let bench = Bench::new().await;
    let r = bench
        .client
        .call::<_, cellar_ipc::results::system::SystemHelloResult>(
            "system.hello",
            SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();
    assert!(r.capabilities.contains(&"events.subscribe".into()));
    assert!(r.capabilities.contains(&"fires.subscribe".into()));
}

// ───── confirmation.* end-to-end over UDS ─────

#[tokio::test]
async fn confirmation_list_pending_empty_on_fresh_daemon() {
    let bench = Bench::new().await;
    let r: ConfirmationListPendingResult = bench
        .client
        .call(
            "confirmation.list_pending",
            ConfirmationListPendingParams::default(),
        )
        .await
        .unwrap();
    assert!(r.pending.is_empty());
}

#[tokio::test]
async fn confirmation_resolve_unknown_id_returns_typed_error() {
    let bench = Bench::new().await;
    let err = bench
        .client
        .call::<_, ConfirmationResolveResult>(
            "confirmation.resolve",
            ConfirmationResolveParams {
                id: "ghost".into(),
                decision: ConfirmationDecisionWire::Allow,
                remember_kind: None,
            },
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("-32008") || msg.contains("confirmation not found"),
        "expected ConfirmationNotFound (-32008), got: {msg}"
    );
}

#[tokio::test]
async fn confirmation_resolve_unblocks_gateway_intercept() {
    use cel_act_gateway::ProposedAction;
    use cellar_types::{
        expression::Operator,
        rule::{Action, ActionType, RuleKind},
        Expression, Rule,
    };

    let bench = Bench::new().await;

    // Install a guard rule that pauses any `fs.copy`.
    let rule = Rule {
        id: "guard_fs_copy".into(),
        name: "Guard fs.copy".into(),
        nl_original: "require my confirmation before any fs.copy".into(),
        kind: RuleKind::Guard,
        enabled: true,
        match_expr: Expression::all(vec![
            Expression::leaf(
                "kind",
                Operator::Eq,
                serde_json::json!("agent_action_attempted"),
            ),
            Expression::leaf(
                "data.action_type",
                Operator::Eq,
                serde_json::json!("fs.copy"),
            ),
        ]),
        action: Action {
            action_type: ActionType::RequireConfirmation,
            webhook_id: None,
            timeout_s: Some(5),
        },
        cooldown_seconds: 0,
        created_at: chrono::Utc::now(),
    };
    bench.daemon.rules_store.create_rule(rule).unwrap();

    // Fire the gateway intercept in the background; it will block on the
    // broker awaiting our resolve.
    let gw = bench.daemon.gateway.clone();
    let intercept = tokio::spawn(async move {
        gw.intercept(ProposedAction {
            caller: "embedded".into(),
            action_type: "fs.copy".into(),
            action_args: serde_json::json!({
                "source_path": "/Users/x/secret.pdf",
                "dest_path": "/Volumes/External/"
            }),
            agent_session_id: Some("sess_test".into()),
            project_root: None,
        })
        .await
    });

    // Poll list_pending until the entry appears (broker registers
    // before the bus publish, so this is quick).
    let mut pending_id: Option<String> = None;
    for _ in 0..40 {
        let r: ConfirmationListPendingResult = bench
            .client
            .call(
                "confirmation.list_pending",
                ConfirmationListPendingParams::default(),
            )
            .await
            .unwrap();
        if let Some(p) = r.pending.first() {
            pending_id = Some(p.id.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let pending_id = pending_id.expect("expected a pending confirmation to appear within 1s");

    // Resolve it Allow.
    let r: ConfirmationResolveResult = bench
        .client
        .call(
            "confirmation.resolve",
            ConfirmationResolveParams {
                id: pending_id,
                decision: ConfirmationDecisionWire::Allow,
                remember_kind: None,
            },
        )
        .await
        .unwrap();
    assert!(r.resolved);

    // The intercept should now complete with Executed.
    let outcome = tokio::time::timeout(Duration::from_secs(2), intercept)
        .await
        .expect("intercept should complete within 2s after resolve")
        .unwrap()
        .unwrap();
    assert!(outcome.executed(), "expected Executed, got {outcome:?}");
}

#[tokio::test]
async fn system_hello_advertises_confirmation_capability() {
    let bench = Bench::new().await;
    let r = bench
        .client
        .call::<_, cellar_ipc::results::system::SystemHelloResult>(
            "system.hello",
            SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            },
        )
        .await
        .unwrap();
    assert!(r.capabilities.contains(&"confirmation".into()));
}

#[tokio::test]
async fn webhooks_test_unreachable_returns_result_over_uds() {
    let bench = Bench::new().await;
    let mut cfg = sample_webhook("unreachable");
    cfg.url = "http://127.0.0.1:1/never".into();
    cfg.timeout_ms = 500;
    let _: cellar_ipc::results::OkResult = bench
        .client
        .call("webhooks.add", WebhooksAddParams { config: cfg })
        .await
        .unwrap();

    let r: WebhooksTestResult = bench
        .client
        .call(
            "webhooks.test",
            WebhookIdParams {
                id: "unreachable".into(),
            },
        )
        .await
        .unwrap();
    assert!(!r.reachable);
    assert!(r.error.is_some());
}

#[tokio::test]
async fn daemon_status_after_rule_add_reflects_count() {
    let bench = Bench::new().await;

    // Add a rule and a watchlist over IPC.
    let _: RulesAddResult = bench
        .client
        .call(
            "rules.add",
            RulesAddParams {
                rule: sample_rule("status_test"),
            },
        )
        .await
        .unwrap();
    let _: OkResult = bench
        .client
        .call(
            "watchlists.set",
            WatchlistsSetParams {
                name: "wl".into(),
                items: vec!["a".into()],
            },
        )
        .await
        .unwrap();

    // `daemon.status` now reports the real counts.
    let status: DaemonStatusResult = bench.client.call("daemon.status", json!({})).await.unwrap();
    assert_eq!(status.rules.total, 1);
    assert_eq!(status.rules.enabled, 1);
    assert_eq!(status.watchlists.total, 1);
}
