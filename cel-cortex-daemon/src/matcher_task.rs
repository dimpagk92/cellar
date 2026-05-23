//! The matcher consumer task — the link between the event bus and rule fires.
//!
//! This task runs forever in a spawned tokio task. Every ambient event
//! published on the [`crate::bus::EventBus`] flows through it:
//!
//! 1. Subscribe to the bus.
//! 2. For each event: ignore [`EventKind::AgentActionAttempted`] /
//!    [`EventKind::AgentActionCompleted`] / [`EventKind::AgentActionDenied`]
//!    — those flow through the gateway, not this task.
//! 3. Read a [`RuleSource::snapshot`] and run [`Matcher::evaluate`].
//! 4. For each matched rule, write a `Fire`-kind [`MemoryChunk`] via the
//!    [`MemoryProvider`]. The activity tab, retrieval, and future analytics
//!    all read from there.
//! 5. *(Phase 1.x)* Webhook fan-out and cooldown enforcement will hook in
//!    here. The matcher task is the right place for both because it
//!    centralises ambient-event rule processing.
//!
//! The task ends when the bus closes (all senders dropped). It logs and
//! continues on individual write errors so a transient SQLite hiccup
//! doesn't take the whole daemon down.

use std::sync::Arc;

use cel_act_gateway::{CooldownTracker, FiredRuleSnapshot, RuleSource, WebhookHook};
use cel_memory::{ChunkKind, ChunkSource, MemoryProvider, NewMemoryChunk};
use cellar_types::{matcher::WatchlistLookup, ActionType, Event, EventKind, Matcher, Rule};
use chrono::Utc;
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bus::EventBus;
use crate::fire_bus::{FireBus, FireFrame};

/// Stable `caller_id` written into every `Fire` chunk this task produces.
/// Lets retrievals filter or scope by "this came from the rule matcher,
/// not from the gateway or an external MCP client."
pub const MATCHER_CALLER_ID: &str = "matcher";

/// Spawn the matcher consumer task. Returns the [`JoinHandle`] so callers
/// can `.await` shutdown if desired; in normal operation the task lives
/// for the daemon's lifetime.
///
/// All deps are `Arc`-wrapped because the task is a `'static` future. The
/// optional `cooldown` is shared with the gateway so per-rule cooldown
/// windows count fires from either path.
pub fn spawn<R, W>(
    bus: &EventBus,
    rules: Arc<R>,
    watchlists: Arc<W>,
    memory: Arc<dyn MemoryProvider>,
    cooldown: Option<Arc<CooldownTracker>>,
    webhook_hook: Option<Arc<dyn WebhookHook>>,
    fire_bus: Option<FireBus>,
) -> JoinHandle<()>
where
    R: RuleSource + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    let mut rx = bus.subscribe();
    tokio::spawn(async move {
        tracing::info!("matcher consumer task started");
        loop {
            match rx.recv().await {
                Ok(event) => {
                    process_event(
                        &event,
                        rules.as_ref(),
                        watchlists.as_ref(),
                        memory.as_ref(),
                        cooldown.as_deref(),
                        webhook_hook.as_deref(),
                        fire_bus.as_ref(),
                    )
                    .await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "matcher consumer task lagged behind event bus");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::info!("event bus closed; matcher consumer task exiting");
                    break;
                }
            }
        }
    })
}

/// Test-friendly variant: same logic, but the caller passes the receiver in
/// directly. Used by integration tests that want to drive the loop without
/// spinning up a separate task.
pub async fn run_once<R, W>(event: &Event, rules: &R, watchlists: &W, memory: &dyn MemoryProvider)
where
    R: RuleSource,
    W: WatchlistLookup + Send + Sync,
{
    process_event(event, rules, watchlists, memory, None, None, None).await
}

/// Variant of [`run_once`] that also consults a [`CooldownTracker`]. Used
/// by integration tests verifying the suppress-second-fire behaviour.
pub async fn run_once_with_cooldown<R, W>(
    event: &Event,
    rules: &R,
    watchlists: &W,
    memory: &dyn MemoryProvider,
    cooldown: &CooldownTracker,
) where
    R: RuleSource,
    W: WatchlistLookup + Send + Sync,
{
    process_event(event, rules, watchlists, memory, Some(cooldown), None, None).await
}

/// Variant of [`run_once`] that also calls a [`WebhookHook`] for any
/// matched rule whose action is `Webhook`. Cooldown and the webhook hook
/// are both optional; passing `None` to either gives that-feature-off
/// behaviour for that one call.
pub async fn run_once_full<R, W>(
    event: &Event,
    rules: &R,
    watchlists: &W,
    memory: &dyn MemoryProvider,
    cooldown: Option<&CooldownTracker>,
    webhook_hook: Option<&dyn WebhookHook>,
    fire_bus: Option<&FireBus>,
) where
    R: RuleSource,
    W: WatchlistLookup + Send + Sync,
{
    process_event(
        event,
        rules,
        watchlists,
        memory,
        cooldown,
        webhook_hook,
        fire_bus,
    )
    .await
}

async fn process_event<R, W>(
    event: &Event,
    rules: &R,
    watchlists: &W,
    memory: &dyn MemoryProvider,
    cooldown: Option<&CooldownTracker>,
    webhook_hook: Option<&dyn WebhookHook>,
    fire_bus: Option<&FireBus>,
) where
    R: RuleSource,
    W: WatchlistLookup + Send + Sync,
{
    if is_gateway_event(&event.kind) {
        // Gateway events are handled by the gateway itself; the matcher
        // task only owns ambient sources (Cortex / process / fsevents / ...).
        return;
    }

    let rules_snapshot = rules.snapshot();
    let fired = Matcher::evaluate(event, &rules_snapshot, watchlists);

    for matched in fired {
        // Cooldown gate: a rule still within its `cooldown_seconds` window
        // is silently dropped — no Fire chunk, no webhook fan-out. The
        // gateway uses the same tracker for its own intercept path so a
        // rule that fires through one path can't be re-fired through the
        // other within the window.
        if let Some(cd) = cooldown {
            if !cd.try_fire(&matched.rule.id, matched.rule.cooldown_seconds) {
                tracing::trace!(
                    rule_id = %matched.rule.id,
                    cooldown_seconds = matched.rule.cooldown_seconds,
                    "matcher: cooldown suppressed fire"
                );
                continue;
            }
        }

        if let Err(e) = write_fire_chunk(memory, matched.rule, event).await {
            tracing::error!(
                error = %e,
                rule_id = %matched.rule.id,
                event_kind = ?event.kind,
                "failed to write Fire chunk; continuing"
            );
        }

        // Publish to the fire bus for `fires.subscribe` / `fires.recent`.
        if let Some(fb) = fire_bus {
            fb.publish(build_fire_frame(matched.rule, event));
        }

        // Webhook fan-out for ambient-event matches. The hook owns its
        // own retry queue, so this returns near-immediately; HTTP failures
        // are logged inside the hook, not propagated here.
        if let Some(hook) = webhook_hook {
            if matches!(matched.rule.action.action_type, ActionType::Webhook) {
                let snap = FiredRuleSnapshot {
                    rule_id: matched.rule.id.clone(),
                    rule_name: matched.rule.name.clone(),
                    rule_nl_original: matched.rule.nl_original.clone(),
                    action_type: matched.rule.action.action_type,
                    timeout_s: matched.rule.action.timeout_s,
                    webhook_id: matched.rule.action.webhook_id.clone(),
                };
                hook.deliver(&snap, event).await;
            }
        }
    }
}

fn build_fire_frame(rule: &Rule, event: &Event) -> FireFrame {
    FireFrame {
        id: format!("fire_{}", Uuid::now_v7()),
        fired_at: Utc::now(),
        rule_id: rule.id.clone(),
        rule_name: rule.name.clone(),
        rule_kind: serde_json::to_value(rule.kind)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
        event_kind: event_kind_str(&event.kind),
        event_source: serde_json::to_value(event.source)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
        event_data: serde_json::to_value(&event.data).unwrap_or(serde_json::Value::Null),
        is_blocking: false,
    }
}

fn is_gateway_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AgentActionAttempted
            | EventKind::AgentActionCompleted
            | EventKind::AgentActionDenied
    )
}

async fn write_fire_chunk(
    memory: &dyn MemoryProvider,
    rule: &Rule,
    event: &Event,
) -> Result<(), cel_memory::MemoryError> {
    let kind_str = event_kind_str(&event.kind);
    let content = format!(
        "Rule '{}' fired on {} ({}); action: {}",
        rule.name,
        kind_str,
        event_summary(event),
        action_type_str(&rule.action.action_type),
    );

    let metadata = json!({
        "rule_id": rule.id,
        "rule_name": rule.name,
        "rule_kind": rule.kind,
        "action_type": rule.action.action_type,
        "webhook_id": rule.action.webhook_id,
        "event_kind": event.kind,
        "event_source": event.source,
        "event_ts": event.ts.to_rfc3339(),
        "event_data": event.data,
        "is_blocking": false,
    });

    memory
        .write(NewMemoryChunk {
            kind: ChunkKind::Fire,
            source: ChunkSource::Matcher,
            session_id: None,
            project_root: None,
            caller_id: MATCHER_CALLER_ID.to_string(),
            content,
            metadata,
            importance: None,
            shareable: false,
            pinned: false,
        })
        .await?;

    Ok(())
}

fn event_kind_str(kind: &EventKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", kind))
}

fn action_type_str(action_type: &cellar_types::ActionType) -> &'static str {
    use cellar_types::ActionType;
    match action_type {
        ActionType::Allow => "allow",
        ActionType::Webhook => "webhook",
        ActionType::RequireConfirmation => "require_confirmation",
        ActionType::Veto => "veto",
        ActionType::SoftBlock => "soft_block",
        ActionType::LogOnly => "log_only",
    }
}

/// One-line human-readable summary of the event's most important data fields.
/// Mirrors the format the activity tab will use for the fire list — keep
/// stable so retrieval queries on chunk `content` match downstream search.
fn event_summary(event: &Event) -> String {
    let path = event.data.get("path").and_then(|v| v.as_str());
    let url = event.data.get("url").and_then(|v| v.as_str());
    let bundle_id = event.data.get("bundle_id").and_then(|v| v.as_str());
    let size = event.data.get("size_bytes").and_then(|v| v.as_u64());

    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = path {
        parts.push(format!("path={}", p));
    }
    if let Some(u) = url {
        parts.push(format!("url={}", u));
    }
    if let Some(b) = bundle_id {
        parts.push(format!("bundle_id={}", b));
    }
    if let Some(s) = size {
        parts.push(format!("size_bytes={}", s));
    }
    if parts.is_empty() {
        "no salient fields".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_act_gateway::traits::StaticRules;
    use cel_memory::BasicMemoryProvider;
    use cellar_types::expression::Operator;
    use cellar_types::rule::{Action, ActionType, RuleKind};
    use cellar_types::{EventKind, EventSource, Expression, InMemoryWatchlists};

    fn watcher_rule(id: &str, expr: Expression) -> Rule {
        watcher_rule_with_cooldown(id, expr, 0)
    }

    fn watcher_rule_with_cooldown(id: &str, expr: Expression, cooldown_seconds: u64) -> Rule {
        Rule {
            id: id.into(),
            name: format!("rule {id}"),
            nl_original: "n/a".into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: expr,
            action: Action {
                action_type: ActionType::Webhook,
                webhook_id: Some("default".into()),
                timeout_s: None,
            },
            cooldown_seconds,
            created_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn matching_event_writes_fire_chunk() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let rules = StaticRules(vec![watcher_rule(
            "rule_big_delete",
            Expression::all(vec![
                Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
                Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
            ]),
        )]);
        let watchlists = InMemoryWatchlists::default();

        let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("path", "~/Documents/big.pdf")
            .with_data("size_bytes", 2_147_483_648u64);

        run_once(&event, &rules, &watchlists, memory.as_ref()).await;

        // Verify a Fire chunk landed in memory.
        let stats = memory.stats().await.unwrap();
        // BasicMemoryProvider's stats includes per-kind counts via total_chunks.
        // We assert at least one chunk exists; the kind/source/content asserts
        // happen via retrieve below.
        assert!(
            stats.total_chunks >= 1,
            "expected at least one chunk written"
        );

        // Retrieve and verify the chunk's shape. Query text matches a token
        // we know is in the formatted content. CallerScope::Global so the
        // test sees the matcher's chunks (caller_id="matcher", not "test").
        let query = cel_memory::MemoryQuery {
            text: "file_deleted".into(),
            kinds: Some(vec![ChunkKind::Fire]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: cel_memory::CallerScope::Global,
            project_root_prefix: None,
            k: 5,
            include_rollups: true,
            min_importance: None,
            profile: cel_memory::RetrievalProfile::default(),
            caller_id: "test".into(),
        };
        let hits = memory.retrieve(query).await.unwrap();
        assert!(!hits.is_empty(), "fire chunk should be retrievable");
        let chunk = &hits[0];
        assert_eq!(chunk.source, ChunkSource::Matcher);
        assert_eq!(chunk.kind, ChunkKind::Fire);
        assert_eq!(chunk.caller_id, MATCHER_CALLER_ID);
        assert!(chunk.content.contains("Rule 'rule rule_big_delete' fired"));
        assert!(chunk.content.contains("file_deleted"));
        assert!(chunk.content.contains("size_bytes=2147483648"));
        // Metadata exposes structured fields for filtering.
        assert_eq!(chunk.metadata["rule_id"], "rule_big_delete");
        assert_eq!(chunk.metadata["event_kind"], "file_deleted");
        assert_eq!(chunk.metadata["is_blocking"], false);
    }

    #[tokio::test]
    async fn non_matching_event_writes_no_chunk() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let rules = StaticRules(vec![watcher_rule(
            "rule_big_delete",
            Expression::leaf("data.size_bytes", Operator::Gte, json!(1_073_741_824u64)),
        )]);
        let watchlists = InMemoryWatchlists::default();

        let small = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("path", "/tmp/x.txt")
            .with_data("size_bytes", 1024u64);
        run_once(&small, &rules, &watchlists, memory.as_ref()).await;

        let stats = memory.stats().await.unwrap();
        assert_eq!(
            stats.total_chunks, 0,
            "no fire expected for non-matching event"
        );
    }

    #[tokio::test]
    async fn gateway_events_are_skipped() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        // A rule that would match every event regardless of kind.
        let rules = StaticRules(vec![watcher_rule("rule_catchall", Expression::all(vec![]))]);
        let watchlists = InMemoryWatchlists::default();

        let gateway_event = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted);
        run_once(&gateway_event, &rules, &watchlists, memory.as_ref()).await;

        let stats = memory.stats().await.unwrap();
        assert_eq!(
            stats.total_chunks, 0,
            "gateway-sourced events must not be matched by the consumer task"
        );

        // Sanity: an ambient event with the same rule does fire.
        let ambient = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        run_once(&ambient, &rules, &watchlists, memory.as_ref()).await;
        let stats2 = memory.stats().await.unwrap();
        assert!(stats2.total_chunks >= 1);
    }

    #[tokio::test]
    async fn multiple_rules_fire_independent_chunks() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let rules = StaticRules(vec![
            watcher_rule(
                "rule_a",
                Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            ),
            watcher_rule(
                "rule_b",
                Expression::leaf("data.path", Operator::StartsWith, json!("~/Documents")),
            ),
        ]);
        let watchlists = InMemoryWatchlists::default();

        let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("path", "~/Documents/x.txt");
        run_once(&event, &rules, &watchlists, memory.as_ref()).await;

        let stats = memory.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 2, "both rules should have fired");
    }

    #[tokio::test]
    async fn cooldown_suppresses_second_fire_within_window() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let rules = StaticRules(vec![watcher_rule_with_cooldown(
            "rate_limited",
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            // 60 s cooldown — second fire within the window must drop.
            60,
        )]);
        let watchlists = InMemoryWatchlists::default();
        let cooldown = CooldownTracker::new();

        let event =
            Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/tmp/x");

        // First fire: lands a Fire chunk.
        run_once_with_cooldown(&event, &rules, &watchlists, memory.as_ref(), &cooldown).await;
        assert_eq!(memory.stats().await.unwrap().total_chunks, 1);

        // Second fire: suppressed by cooldown — still 1.
        run_once_with_cooldown(&event, &rules, &watchlists, memory.as_ref(), &cooldown).await;
        assert_eq!(
            memory.stats().await.unwrap().total_chunks,
            1,
            "cooldown should have suppressed the second fire"
        );

        // Different rule on the same event still fires (independent).
        let mut more_rules = vec![watcher_rule_with_cooldown(
            "rate_limited",
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            60,
        )];
        more_rules.push(watcher_rule_with_cooldown(
            "no_cooldown",
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            0,
        ));
        let rules2 = StaticRules(more_rules);
        run_once_with_cooldown(&event, &rules2, &watchlists, memory.as_ref(), &cooldown).await;
        assert_eq!(
            memory.stats().await.unwrap().total_chunks,
            2,
            "uncooled rule should still fire on the same event"
        );
    }

    #[tokio::test]
    async fn cooldown_zero_seconds_never_suppresses() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let rules = StaticRules(vec![watcher_rule_with_cooldown(
            "always_fires",
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            0, // no cooldown
        )]);
        let watchlists = InMemoryWatchlists::default();
        let cooldown = CooldownTracker::new();
        let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted);

        run_once_with_cooldown(&event, &rules, &watchlists, memory.as_ref(), &cooldown).await;
        run_once_with_cooldown(&event, &rules, &watchlists, memory.as_ref(), &cooldown).await;
        run_once_with_cooldown(&event, &rules, &watchlists, memory.as_ref(), &cooldown).await;
        assert_eq!(memory.stats().await.unwrap().total_chunks, 3);
    }

    #[tokio::test]
    async fn end_to_end_through_bus() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let rules = Arc::new(StaticRules(vec![watcher_rule(
            "rule_url",
            Expression::leaf("data.url", Operator::Contains, json!("example.com")),
        )]));
        let watchlists = Arc::new(InMemoryWatchlists::default());

        let bus = EventBus::new();
        let handle = spawn(&bus, rules, watchlists, memory.clone(), None, None, None);

        // Give the task a moment to subscribe before we publish.
        tokio::task::yield_now().await;
        bus.publish(
            Event::now(EventSource::CortexCdp, EventKind::UrlChanged)
                .with_data("url", "https://example.com/login"),
        );
        // Drop the bus → the task's recv() will see Closed and exit.
        drop(bus);

        // Wait for the task to drain and exit cleanly.
        handle.await.unwrap();

        let stats = memory.stats().await.unwrap();
        assert!(
            stats.total_chunks >= 1,
            "expected at least one fire via the bus"
        );
    }
}
