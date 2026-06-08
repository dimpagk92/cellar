//! `MatcherOffdeviceHook` — bridges the rule matcher into the
//! [`OffdeviceCallHook`] seam.
//!
//! Every off-device memory call (cloud summarizer, cloud embedder)
//! produces a synthetic [`EventKind::MemoryOffdeviceCallAttempted`]
//! event. The matcher runs over the current rule snapshot; if any
//! matched rule has `action.type == Veto`, the call is blocked (the
//! producer surfaces a `Provider`-style error to its caller; the
//! network round-trip never happens). Other action types are ignored
//! for v1 — we don't surface a `RequireConfirmation` round-trip from
//! the memory subsystem yet (Phase 5 of the memory plan).
//!
//! This is the daemon-side glue that delivers the trust-and-execution
//! thesis for off-device memory calls: the same rule schema that
//! governs `cel_act` calls also governs which calls leave the device.
//!
//! See `cellar-memory-manager.md` §11 and §16 "Privacy footgun."
//!
//! On the event bus side, the hook also publishes the synthetic
//! event so any audit subscriber (Activity tab, telemetry) sees the
//! attempt. This lets users see "Cellar tried to call Anthropic" even
//! on calls the matcher allowed.

use std::sync::Arc;

use async_trait::async_trait;
use cel_memory::offdevice_hook::{OffdeviceCallDescriptor, OffdeviceCallHook, OffdeviceDecision};
use cellar_types::{ActionType, Event, EventKind, EventSource, Matcher, Rule, WatchlistLookup};
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::bus::EventBus;

/// Hook plugged into the memory subsystem's off-device call seam.
///
/// Holds the same `Arc<RuleSource>` + `Arc<WatchlistLookup>` the
/// gateway and matcher consumer hold, so any add / update / remove
/// through `daemon.rules_store` is visible here on the next call — no
/// reload signal needed (same hot-reload model as the gateway).
///
/// Also holds an optional [`EventBus`] handle. When set, every
/// off-device call publishes a synthetic
/// [`EventKind::MemoryOffdeviceCallAttempted`] event so subscribers
/// (Activity tab, telemetry) can audit attempts. When unset, the
/// matcher still runs but the bus is bypassed — useful for unit
/// tests that don't care about the audit trail.
pub struct MatcherOffdeviceHook<R, W>
where
    R: cel_act_gateway::traits::RuleSource + Send + Sync + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    rules: Arc<R>,
    watchlists: Arc<W>,
    bus: Option<EventBus>,
}

impl<R, W> MatcherOffdeviceHook<R, W>
where
    R: cel_act_gateway::traits::RuleSource + Send + Sync + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    /// Build a hook backed by the daemon's shared rule + watchlist
    /// sources. No event-bus attachment — calls are governed but the
    /// audit trail is suppressed.
    pub fn new(rules: Arc<R>, watchlists: Arc<W>) -> Self {
        Self {
            rules,
            watchlists,
            bus: None,
        }
    }

    /// Attach an event bus so every off-device call also publishes a
    /// synthetic event for subscribers. Builder-style.
    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }
}

#[async_trait]
impl<R, W> OffdeviceCallHook for MatcherOffdeviceHook<R, W>
where
    R: cel_act_gateway::traits::RuleSource + Send + Sync + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    async fn before_call(&self, descriptor: &OffdeviceCallDescriptor) -> OffdeviceDecision {
        let event = make_offdevice_event(descriptor);
        // Publish first so subscribers see every attempt, including
        // vetoed ones — the audit trail must record what was blocked.
        if let Some(bus) = &self.bus {
            bus.publish(event.clone());
        }
        let rules: Vec<Rule> = self.rules.snapshot();
        let matches = Matcher::evaluate(&event, &rules, self.watchlists.as_ref());

        for m in &matches {
            if matches!(m.rule.action.action_type, ActionType::Veto) {
                tracing::info!(
                    rule_id = %m.rule.id,
                    rule_name = %m.rule.name,
                    provider = %descriptor.provider,
                    model = %descriptor.model,
                    subsystem = %descriptor.subsystem,
                    "off-device memory call vetoed by rule"
                );
                return OffdeviceDecision::Veto {
                    reason: m.rule.name.clone(),
                };
            }
        }
        OffdeviceDecision::Allow
    }
}

/// Synthesise a `MemoryOffdeviceCallAttempted` event from a
/// descriptor. The matcher addresses `data.kind`, `data.provider`,
/// `data.model`, `data.subsystem`, plus every metadata key under
/// `data.metadata.*` (so rules can match e.g.
/// `data.metadata.session_id == "s1"`).
fn make_offdevice_event(descriptor: &OffdeviceCallDescriptor) -> Event {
    let mut data: BTreeMap<String, Value> = BTreeMap::new();
    data.insert("kind".into(), Value::String(descriptor.kind.clone()));
    data.insert(
        "provider".into(),
        Value::String(descriptor.provider.clone()),
    );
    data.insert("model".into(), Value::String(descriptor.model.clone()));
    data.insert(
        "subsystem".into(),
        Value::String(descriptor.subsystem.clone()),
    );
    if !descriptor.metadata.is_empty() {
        data.insert(
            "metadata".into(),
            Value::Object(
                descriptor
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
    }
    Event {
        ts: Utc::now(),
        source: EventSource::Memory,
        kind: EventKind::MemoryOffdeviceCallAttempted,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_act_gateway::traits::StaticRules;
    use cellar_types::{
        expression::{Expression, Operator},
        rule::{Action, RuleKind},
        watchlist::InMemoryWatchlists,
    };
    use serde_json::json;

    fn veto_rule(name: &str, match_expr: Expression) -> Rule {
        Rule {
            id: format!("rule_{name}"),
            name: name.into(),
            nl_original: format!("rule {name}"),
            kind: RuleKind::Audit,
            enabled: true,
            match_expr,
            action: Action {
                action_type: ActionType::Veto,
                webhook_id: None,
                timeout_s: None,
            },
            cooldown_seconds: 0,
            created_at: Utc::now(),
        }
    }

    fn desc() -> OffdeviceCallDescriptor {
        OffdeviceCallDescriptor::new(
            "summarizer",
            "anthropic",
            "claude-haiku-4-5",
            "memory_summarizer",
        )
    }

    #[tokio::test]
    async fn no_rules_allows_calls() {
        let hook = MatcherOffdeviceHook::new(
            Arc::new(StaticRules(vec![])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let d = hook.before_call(&desc()).await;
        assert_eq!(d, OffdeviceDecision::Allow);
    }

    #[tokio::test]
    async fn veto_on_provider_blocks_call() {
        let rule = veto_rule(
            "no_anthropic",
            Expression::leaf("data.provider", Operator::Eq, json!("anthropic")),
        );
        let hook = MatcherOffdeviceHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        match hook.before_call(&desc()).await {
            OffdeviceDecision::Veto { reason } => assert_eq!(reason, "no_anthropic"),
            other => panic!("expected Veto, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_anthropic_call_passes_through() {
        let rule = veto_rule(
            "no_anthropic",
            Expression::leaf("data.provider", Operator::Eq, json!("anthropic")),
        );
        let hook = MatcherOffdeviceHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let openai_call = OffdeviceCallDescriptor::new(
            "summarizer",
            "openai",
            "gpt-4o-mini",
            "memory_summarizer",
        );
        assert_eq!(
            hook.before_call(&openai_call).await,
            OffdeviceDecision::Allow
        );
    }

    #[tokio::test]
    async fn veto_on_subsystem_blocks_call() {
        // Rule: never let the memory subsystem make off-device calls
        // (use-case: privacy-conscious user disables cloud rollups).
        let rule = veto_rule(
            "no_memory_offdevice",
            Expression::leaf("data.subsystem", Operator::Eq, json!("memory_summarizer")),
        );
        let hook = MatcherOffdeviceHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        match hook.before_call(&desc()).await {
            OffdeviceDecision::Veto { reason } => assert_eq!(reason, "no_memory_offdevice"),
            other => panic!("expected Veto, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_veto_rules_dont_block() {
        let mut rule = veto_rule(
            "audit_only",
            Expression::leaf("data.provider", Operator::Eq, json!("anthropic")),
        );
        rule.action.action_type = ActionType::LogOnly;
        let hook = MatcherOffdeviceHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        assert_eq!(hook.before_call(&desc()).await, OffdeviceDecision::Allow);
    }

    #[tokio::test]
    async fn event_bus_sees_every_attempt() {
        // Even vetoed calls publish an event so the audit trail
        // records "tried to call, was blocked."
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let rule = veto_rule(
            "no_anthropic",
            Expression::leaf("data.provider", Operator::Eq, json!("anthropic")),
        );
        let hook = MatcherOffdeviceHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        )
        .with_event_bus(bus);
        let _ = hook.before_call(&desc()).await;
        let event = rx.recv().await.expect("event published");
        assert_eq!(event.source, EventSource::Memory);
        assert_eq!(event.kind, EventKind::MemoryOffdeviceCallAttempted);
        assert_eq!(
            event.data.get("provider").and_then(|v| v.as_str()),
            Some("anthropic")
        );
    }
}
