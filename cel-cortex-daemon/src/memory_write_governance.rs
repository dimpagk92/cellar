//! `MatcherWriteHook` — bridges the rule matcher into the
//! [`MemoryWriteHook`] seam.
//!
//! Every memory write produces a synthetic [`EventKind::MemoryWriteAttempted`]
//! event. The matcher runs over the current rule snapshot; if any matched
//! rule has `action.type == Veto` **or** `action.type == RedactMemory`, the
//! write is redacted (the chunk never lands in storage; a `<redacted: …>`
//! marker is returned to the caller instead). Other action types are
//! ignored — those two variants are the only governance modes v1 supports
//! on memory writes.
//!
//! `RedactMemory` is the named-action sugar layer over `Veto`: both produce
//! the same `WriteDecision::Redact` here, but `RedactMemory` is what the NL
//! compiler emits when the user's phrasing implies "don't persist memory
//! about X" (e.g. *"never persist chunks mentioning bank.example.com"*).
//! Users authoring rules via the JSON path can still use `Veto` directly
//! and get identical behaviour.
//!
//! This is the daemon-side glue that delivers the trust-and-execution
//! thesis for memory writes: rules can govern what gets persisted with
//! the same schema and matcher that govern `cel_act` calls.
//!
//! See `cellar-memory-manager.md` §10.5 / §11.5.

use std::sync::Arc;

use async_trait::async_trait;
use cel_memory::{MemoryWriteHook, NewMemoryChunk, Result as MemoryResult, WriteDecision};
use cellar_types::{ActionType, Event, EventKind, EventSource, Matcher, Rule, WatchlistLookup};
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeMap;

/// What the daemon plugs into the memory provider's [`MemoryWriteHook`]
/// slot to give rules veto power over individual writes.
///
/// Holds the same `Arc<RuleSource>` + `Arc<WatchlistLookup>` the gateway
/// and matcher consumer hold, so any add / update / remove through
/// `daemon.rules_store` is visible here on the next write — no reload
/// signal needed (same hot-reload model as the gateway).
pub struct MatcherWriteHook<R, W>
where
    R: cel_act_gateway::traits::RuleSource + Send + Sync + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    rules: Arc<R>,
    watchlists: Arc<W>,
}

impl<R, W> MatcherWriteHook<R, W>
where
    R: cel_act_gateway::traits::RuleSource + Send + Sync + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    /// Build a hook backed by the daemon's shared rule + watchlist sources.
    pub fn new(rules: Arc<R>, watchlists: Arc<W>) -> Self {
        Self { rules, watchlists }
    }
}

#[async_trait]
impl<R, W> MemoryWriteHook for MatcherWriteHook<R, W>
where
    R: cel_act_gateway::traits::RuleSource + Send + Sync + 'static,
    W: WatchlistLookup + Send + Sync + 'static,
{
    async fn before_write(&self, chunk: &NewMemoryChunk) -> MemoryResult<WriteDecision> {
        let event = make_memory_event(chunk);
        let rules: Vec<Rule> = self.rules.snapshot();
        let matches = Matcher::evaluate(&event, &rules, self.watchlists.as_ref());

        // First Veto / RedactMemory wins. The matcher returns rules in
        // declaration order; we honor that for deterministic redaction
        // reasons. Both action types map to the same `WriteDecision::Redact`:
        // `RedactMemory` is the named-action sugar the NL compiler emits;
        // `Veto` is the original variant a hand-authored JSON rule uses.
        // Keeping both paths live here means user-facing language and
        // hand-rolled rules behave identically.
        for m in &matches {
            if matches!(
                m.rule.action.action_type,
                ActionType::Veto | ActionType::RedactMemory
            ) {
                tracing::info!(
                    rule_id = %m.rule.id,
                    rule_name = %m.rule.name,
                    caller = %chunk.caller_id,
                    kind = ?chunk.kind,
                    action = ?m.rule.action.action_type,
                    "memory write redacted by rule"
                );
                return Ok(WriteDecision::Redact {
                    reason: m.rule.name.clone(),
                });
            }
        }
        Ok(WriteDecision::Allow)
    }
}

/// Synthesise a `MemoryWriteAttempted` event from a chunk. Carries enough
/// metadata for rules to match meaningfully: caller, kind, source, plus
/// a `content_preview` (first 256 chars) so content-based redaction rules
/// can use `contains` / `regex` operators without us shipping the full
/// chunk through the matcher (which evaluates per-event many times per
/// second in the worst case).
fn make_memory_event(chunk: &NewMemoryChunk) -> Event {
    let mut data: BTreeMap<String, Value> = BTreeMap::new();
    data.insert("caller".into(), Value::String(chunk.caller_id.clone()));
    data.insert("kind".into(), Value::String(kind_str(chunk.kind).into()));
    data.insert(
        "source".into(),
        Value::String(source_str(chunk.source).into()),
    );
    if let Some(sid) = &chunk.session_id {
        data.insert("session_id".into(), Value::String(sid.clone()));
    }
    if let Some(root) = &chunk.project_root {
        data.insert("project_root".into(), Value::String(root.clone()));
    }
    let preview: String = chunk.content.chars().take(256).collect();
    data.insert("content_preview".into(), Value::String(preview));
    Event {
        ts: Utc::now(),
        source: EventSource::Memory,
        kind: EventKind::MemoryWriteAttempted,
        data,
    }
}

fn kind_str(k: cel_memory::ChunkKind) -> &'static str {
    use cel_memory::ChunkKind::*;
    match k {
        Chat => "chat",
        Action => "action",
        Fire => "fire",
        Observation => "observation",
        Correction => "correction",
        JobSummary => "job_summary",
        Context => "context",
        Rollup => "rollup",
    }
}

fn source_str(s: cel_memory::ChunkSource) -> &'static str {
    use cel_memory::ChunkSource::*;
    match s {
        Embedded => "embedded",
        Mcp => "mcp",
        Gateway => "gateway",
        Matcher => "matcher",
        Perception => "cortex",
        System => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_act_gateway::traits::StaticRules;
    use cel_memory::{ChunkKind, ChunkSource};
    use cellar_types::{
        expression::{Expression, Operator},
        rule::{Action, RuleKind},
        watchlist::InMemoryWatchlists,
    };
    use serde_json::json;

    fn redact_rule(name: &str, match_expr: Expression) -> Rule {
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

    fn chunk(caller: &str, content: &str) -> NewMemoryChunk {
        NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: caller.into(),
            content: content.into(),
            metadata: json!({}),
            importance: None,
            shareable: false,
            pinned: false,
        }
    }

    #[tokio::test]
    async fn no_rules_allows_writes() {
        let hook = MatcherWriteHook::new(
            Arc::new(StaticRules(vec![])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let d = hook.before_write(&chunk("embedded", "x")).await.unwrap();
        assert_eq!(d, WriteDecision::Allow);
    }

    #[tokio::test]
    async fn veto_on_caller_redacts() {
        let rule = redact_rule(
            "no_cursor_memory",
            Expression::leaf("data.caller", Operator::Eq, json!("mcp:cursor")),
        );
        let hook = MatcherWriteHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let allowed = hook.before_write(&chunk("embedded", "x")).await.unwrap();
        assert_eq!(allowed, WriteDecision::Allow);
        let redacted = hook.before_write(&chunk("mcp:cursor", "x")).await.unwrap();
        match redacted {
            WriteDecision::Redact { reason } => assert_eq!(reason, "no_cursor_memory"),
            _ => panic!("expected Redact"),
        }
    }

    #[tokio::test]
    async fn veto_on_content_substring_redacts() {
        // The classic trust-layer demo: never persist chunks mentioning a
        // banned domain.
        let rule = redact_rule(
            "no_bank_memory",
            Expression::leaf(
                "data.content_preview",
                Operator::Contains,
                json!("bank.example.com"),
            ),
        );
        let hook = MatcherWriteHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let innocent = hook
            .before_write(&chunk("embedded", "discussing the weather"))
            .await
            .unwrap();
        assert_eq!(innocent, WriteDecision::Allow);
        let suspicious = hook
            .before_write(&chunk(
                "embedded",
                "I just logged into bank.example.com and saw…",
            ))
            .await
            .unwrap();
        assert!(matches!(suspicious, WriteDecision::Redact { .. }));
    }

    #[tokio::test]
    async fn non_veto_rules_dont_redact() {
        // A LogOnly rule on the same match doesn't redact. We only honor
        // Veto on memory writes — other action types are no-ops here.
        let mut rule = redact_rule(
            "audit_only",
            Expression::leaf("data.caller", Operator::Eq, json!("embedded")),
        );
        rule.action.action_type = ActionType::LogOnly;
        let hook = MatcherWriteHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let d = hook.before_write(&chunk("embedded", "x")).await.unwrap();
        assert_eq!(d, WriteDecision::Allow);
    }

    #[tokio::test]
    async fn redact_memory_action_redacts_like_veto() {
        // The named-action sugar. Same behaviour as Veto, but emitted by the
        // NL compiler when the user phrases the intent as
        // "never persist chunks mentioning <substring>". A rule on a chunk
        // containing the matched substring suppresses persistence.
        let rule = Rule {
            id: "rule_no_bank".into(),
            name: "no_bank_memory".into(),
            nl_original: "never persist chunks mentioning bank.example.com".into(),
            kind: RuleKind::Audit,
            enabled: true,
            match_expr: Expression::leaf(
                "data.content_preview",
                Operator::Contains,
                json!("bank.example.com"),
            ),
            action: Action {
                action_type: ActionType::RedactMemory,
                webhook_id: None,
                timeout_s: None,
            },
            cooldown_seconds: 0,
            created_at: Utc::now(),
        };
        let hook = MatcherWriteHook::new(
            Arc::new(StaticRules(vec![rule])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let innocent = hook
            .before_write(&chunk("embedded", "discussing the weather"))
            .await
            .unwrap();
        assert_eq!(innocent, WriteDecision::Allow);
        let suspicious = hook
            .before_write(&chunk(
                "embedded",
                "I just logged into bank.example.com and saw…",
            ))
            .await
            .unwrap();
        match suspicious {
            WriteDecision::Redact { reason } => assert_eq!(reason, "no_bank_memory"),
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn first_veto_in_match_order_wins() {
        let r1 = redact_rule(
            "alpha",
            Expression::leaf("data.caller", Operator::Eq, json!("embedded")),
        );
        let r2 = redact_rule(
            "beta",
            Expression::leaf("data.caller", Operator::Eq, json!("embedded")),
        );
        let hook = MatcherWriteHook::new(
            Arc::new(StaticRules(vec![r1, r2])),
            Arc::new(InMemoryWatchlists::default()),
        );
        let d = hook.before_write(&chunk("embedded", "x")).await.unwrap();
        match d {
            WriteDecision::Redact { reason } => assert_eq!(reason, "alpha"),
            _ => panic!("expected Redact(alpha)"),
        }
    }
}
