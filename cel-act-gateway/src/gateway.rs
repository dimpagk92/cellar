//! The `Gateway` type.
//!
//! Wraps every `cel_act` call: synthesises an event, runs the matcher,
//! decides what to do, drives confirmation if needed, executes through
//! the actuator, and writes the audit trail to memory.

use std::collections::BTreeMap;
use std::sync::Arc;

use cel_memory::{ChunkKind, ChunkSource, MemoryProvider, NewMemoryChunk};
use cellar_types::{Event, EventKind, EventSource, Matcher, WatchlistLookup};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::action::{ActionOutcome, ConfirmationDecision, ConfirmationRequest, ProposedAction};
use crate::cooldown::CooldownTracker;
use crate::decision::{Decision, FiredRuleSnapshot};
use crate::error::GatewayError;
use crate::traits::{Actuator, ConfirmationBroker, RuleSource};

/// Default confirmation timeout when a rule doesn't specify one.
pub const DEFAULT_CONFIRMATION_TIMEOUT_S: u64 = 300;

/// The gateway. Constructed once at daemon startup and held as
/// `Arc<Gateway<A, B, R, W>>` by every caller of `cel_act`.
///
/// Generic parameters keep the layout monomorphised and let the daemon
/// pass its concrete actuator / broker / rule source / watchlist source
/// without trait-object indirection at the hot path.
pub struct Gateway<A, B, R, W>
where
    A: Actuator,
    B: ConfirmationBroker,
    R: RuleSource,
    W: WatchlistLookup + Send + Sync,
{
    actuator: A,
    broker: B,
    rules: R,
    watchlists: W,
    memory: Arc<dyn MemoryProvider>,
    /// Optional webhook fan-out hook. When set, the gateway invokes
    /// `webhook_hook.deliver(...)` for every fired rule with
    /// `action_type == Webhook`. When unset, webhook fires are still
    /// logged in memory but no delivery happens — useful for tests
    /// and for daemons that aren't yet wiring the webhook subsystem.
    webhook_hook: Option<Arc<dyn crate::traits::WebhookHook>>,
    /// Optional agent-action fan-out hook. When set, called once per
    /// `intercept()` with the final `(action, outcome)` pair. Used by
    /// the daemon to drive the `agent_actions.*` subscribe/recent IPC
    /// surface. Best-effort: failure inside the hook is swallowed.
    action_hook: Option<Arc<dyn crate::traits::AgentActionHook>>,
    /// Per-rule cooldown tracker. Filters matched rules whose
    /// `cooldown_seconds` window hasn't elapsed since their last fire.
    /// Shared with the matcher consumer task so a fire through either path
    /// counts against the same window. When `None`, every match fires (no
    /// cooldown enforcement) — the test-support default for compactness.
    cooldown: Option<Arc<CooldownTracker>>,
    default_confirmation_timeout_s: u64,
}

impl<A, B, R, W> Gateway<A, B, R, W>
where
    A: Actuator,
    B: ConfirmationBroker,
    R: RuleSource,
    W: WatchlistLookup + Send + Sync,
{
    /// Construct a new gateway. The default confirmation timeout is
    /// [`DEFAULT_CONFIRMATION_TIMEOUT_S`]; override via
    /// [`Gateway::with_default_timeout`].
    pub fn new(
        actuator: A,
        broker: B,
        rules: R,
        watchlists: W,
        memory: Arc<dyn MemoryProvider>,
    ) -> Self {
        Self {
            actuator,
            broker,
            rules,
            watchlists,
            memory,
            webhook_hook: None,
            action_hook: None,
            cooldown: None,
            default_confirmation_timeout_s: DEFAULT_CONFIRMATION_TIMEOUT_S,
        }
    }

    /// Attach a [`CooldownTracker`] (shared with the matcher consumer task)
    /// so per-rule `cooldown_seconds` is enforced on the gateway path.
    /// Without a tracker, every match fires (legacy / test default).
    pub fn with_cooldown(mut self, tracker: Arc<CooldownTracker>) -> Self {
        self.cooldown = Some(tracker);
        self
    }

    /// Override the default confirmation timeout (seconds). Rules with an
    /// explicit `timeout_s` still take precedence.
    pub fn with_default_timeout(mut self, secs: u64) -> Self {
        self.default_confirmation_timeout_s = secs;
        self
    }

    /// Attach a [`WebhookHook`](crate::WebhookHook) that the gateway invokes
    /// for every fired rule with `action_type == Webhook`. Without a hook,
    /// webhook fires are recorded in memory but no delivery happens.
    pub fn with_webhook_hook(mut self, hook: Arc<dyn crate::traits::WebhookHook>) -> Self {
        self.webhook_hook = Some(hook);
        self
    }

    /// Attach an [`AgentActionHook`](crate::AgentActionHook) called once per
    /// `intercept()` with the resolved `(action, outcome)`. The daemon uses
    /// this to drive the `agent_actions.*` IPC subscribe/recent surface.
    pub fn with_action_hook(mut self, hook: Arc<dyn crate::traits::AgentActionHook>) -> Self {
        self.action_hook = Some(hook);
        self
    }

    /// Borrow the underlying actuator. Useful for tests; the production
    /// daemon never needs this because the actuator is fully encapsulated.
    pub fn actuator(&self) -> &A {
        &self.actuator
    }

    /// Intercept a proposed action. Runs the matcher, decides, drives
    /// confirmation when needed, executes the actuator, and writes the
    /// audit trail to memory.
    ///
    /// Memory side-effects per call:
    /// - One `Action`-kind chunk recording the attempt and outcome.
    /// - One `Fire`-kind chunk per matched rule (regardless of whether the
    ///   rule intervened or just logged).
    ///
    /// The `Action` chunk's outcome reflects what actually happened
    /// (`Executed`, `Vetoed`, `ConfirmationDenied`, `ConfirmationTimedOut`).
    ///
    /// Failure modes: memory-write errors from `write_fires` short-circuit
    /// before execution (no action runs). Memory-write errors from
    /// `write_action` happen *after* execution and propagate to the caller —
    /// the action has run but the audit trail is incomplete. The caller
    /// receives the memory error rather than the outcome; v1 callers treat
    /// memory failure as fatal. Future work tracks the outcome alongside a
    /// fallible audit channel so the outcome is still surfaced.
    pub async fn intercept(&self, action: ProposedAction) -> Result<ActionOutcome, GatewayError> {
        // The caller's tracing span (typically the IPC handler's
        // `ipc.request` span with `trace_id` attached) is current here
        // by virtue of async-await context propagation — every
        // `tracing::info!` inside this body and the called subsystems
        // (matcher, actuator, broker) inherits it. So an end-to-end
        // request — IPC handler → agent runtime → `cel_act` gateway →
        // matcher → actuator — shares one `trace_id` across log lines
        // with no extra wiring inside the gateway itself.
        tracing::debug!(
            caller = %action.caller,
            action_type = %action.action_type,
            session = ?action.agent_session_id,
            "gateway intercept start"
        );

        // Synthesise the agent_action_attempted event.
        let event = make_event(&action);

        // Run the matcher synchronously over the current rule set.
        let rules = self.rules.snapshot();
        let raw_matches = Matcher::evaluate(&event, &rules, &self.watchlists);

        // Filter by cooldown: rules whose `cooldown_seconds` window hasn't
        // elapsed since their last fire are silently dropped — no Fire
        // chunk, no webhook, no decision contribution. `try_fire` is the
        // atomic check-and-record so two concurrent intercepts can't both
        // pass through the cooldown for the same rule.
        let matches: Vec<_> = if let Some(cd) = self.cooldown.as_ref() {
            let total = raw_matches.len();
            let kept: Vec<_> = raw_matches
                .into_iter()
                .filter(|m| cd.try_fire(&m.rule.id, m.rule.cooldown_seconds))
                .collect();
            let suppressed = total - kept.len();
            if suppressed > 0 {
                tracing::debug!(
                    suppressed,
                    fired = kept.len(),
                    "gateway: cooldown suppressed matches"
                );
            }
            kept
        } else {
            raw_matches
        };
        let decision = Decision::from_matches(&matches);

        tracing::debug!(
            matched = matches.len(),
            decision = match &decision {
                Decision::Allow { .. } => "allow",
                Decision::Veto {
                    soft_block: true, ..
                } => "soft_block",
                Decision::Veto { .. } => "veto",
                Decision::RequireConfirmation { .. } => "require_confirmation",
            },
            "gateway matcher decision"
        );

        // Write Fire chunks for every matched rule (precedes the outcome write
        // so it's still recorded even if the action errors later).
        self.write_fires(&action, &decision).await?;

        // Fan out webhook deliveries for any matched watcher rules. Best
        // effort: failures inside the hook are logged + dropped, never
        // propagated. Runs after the audit write so missing webhook delivery
        // doesn't lose the fire record.
        self.fan_out_webhooks(&event, &decision).await;

        // Drive the decision.
        let outcome = match decision {
            Decision::Allow { .. } => self.execute(&action).await?,
            Decision::Veto {
                rule, soft_block, ..
            } => {
                tracing::info!(
                    rule_id = %rule.rule_id,
                    rule_name = %rule.rule_name,
                    soft_block,
                    caller = %action.caller,
                    action_type = %action.action_type,
                    "gateway vetoed action"
                );
                ActionOutcome::Vetoed {
                    rule_id: rule.rule_id.clone(),
                    rule_name: rule.rule_name.clone(),
                    soft_block,
                }
            }
            Decision::RequireConfirmation { rule, .. } => {
                tracing::info!(
                    rule_id = %rule.rule_id,
                    rule_name = %rule.rule_name,
                    caller = %action.caller,
                    action_type = %action.action_type,
                    "gateway pausing for confirmation"
                );
                self.drive_confirmation(&action, rule).await?
            }
        };

        // Write the Action chunk capturing the attempt and final outcome.
        self.write_action(&action, &outcome).await?;

        // Fan out to the agent-action hook (activity-tab bus in the daemon).
        // Best-effort: hook failures are silently swallowed.
        if let Some(hook) = &self.action_hook {
            hook.on_action(&action, &outcome).await;
        }

        Ok(outcome)
    }

    async fn execute(&self, action: &ProposedAction) -> Result<ActionOutcome, GatewayError> {
        let result = self
            .actuator
            .execute(action)
            .await
            .map_err(GatewayError::Actuator)?;
        Ok(ActionOutcome::Executed { result })
    }

    async fn drive_confirmation(
        &self,
        action: &ProposedAction,
        rule: FiredRuleSnapshot,
    ) -> Result<ActionOutcome, GatewayError> {
        let timeout_s = rule
            .timeout_s
            .unwrap_or(self.default_confirmation_timeout_s);
        let now = Utc::now();
        let expires_at = now + Duration::seconds(timeout_s as i64);
        let req = ConfirmationRequest {
            id: format!("conf_{}", Uuid::now_v7()),
            created_at: now,
            expires_at,
            rule_id: rule.rule_id.clone(),
            rule_name: rule.rule_name.clone(),
            rule_nl_original: rule.rule_nl_original.clone(),
            action: action.clone(),
        };
        let decision = self
            .broker
            .request_confirmation(req)
            .await
            .map_err(GatewayError::Broker)?;
        match decision {
            ConfirmationDecision::Allow => self.execute(action).await,
            ConfirmationDecision::Deny => Ok(ActionOutcome::ConfirmationDenied {
                rule_id: rule.rule_id,
                rule_name: rule.rule_name,
            }),
            ConfirmationDecision::TimedOut => Ok(ActionOutcome::ConfirmationTimedOut {
                rule_id: rule.rule_id,
                rule_name: rule.rule_name,
                timeout_s,
            }),
        }
    }

    async fn fan_out_webhooks(&self, event: &Event, decision: &Decision) {
        let Some(hook) = self.webhook_hook.as_ref() else {
            return;
        };
        for snap in collect_all_fires(decision) {
            if !matches!(snap.action_type, cellar_types::ActionType::Webhook) {
                continue;
            }
            // Best-effort fan-out. The hook is responsible for enqueueing
            // and returning immediately so the gateway's hot path isn't
            // blocked on the actual HTTP call.
            hook.deliver(&snap, event).await;
        }
    }

    async fn write_fires(
        &self,
        action: &ProposedAction,
        decision: &Decision,
    ) -> Result<(), GatewayError> {
        for snap in collect_all_fires(decision) {
            // Phrasing: every gateway-written chunk starts with `agent {caller}`
            // so audit retrievals can match on a stable prefix.
            let content = format!(
                "agent {} attempted {}; rule '{}' fired",
                action.caller, action.action_type, snap.rule_name
            );
            let chunk = NewMemoryChunk {
                kind: ChunkKind::Fire,
                source: ChunkSource::Matcher,
                session_id: action.agent_session_id.clone(),
                project_root: action.project_root.clone(),
                caller_id: action.caller.clone(),
                content,
                metadata: json!({
                    "rule_id": snap.rule_id,
                    "rule_name": snap.rule_name,
                    "action_type": format!("{:?}", snap.action_type),
                }),
                importance: Some(0.5),
                shareable: false,
                pinned: false,
            };
            self.memory.write(chunk).await?;
        }
        Ok(())
    }

    async fn write_action(
        &self,
        action: &ProposedAction,
        outcome: &ActionOutcome,
    ) -> Result<(), GatewayError> {
        let summary = match outcome {
            ActionOutcome::Executed { .. } => format!(
                "agent {} executed {} (gateway: allowed)",
                action.caller, action.action_type
            ),
            ActionOutcome::Vetoed {
                rule_name,
                soft_block,
                ..
            } => format!(
                "agent {} attempted {}; gateway vetoed via rule '{}' ({})",
                action.caller,
                action.action_type,
                rule_name,
                if *soft_block { "soft_block" } else { "veto" }
            ),
            ActionOutcome::ConfirmationDenied { rule_name, .. } => format!(
                "agent {} attempted {}; user denied via rule '{}'",
                action.caller, action.action_type, rule_name
            ),
            ActionOutcome::ConfirmationTimedOut {
                rule_name,
                timeout_s,
                ..
            } => {
                format!(
                    "agent {} attempted {}; rule '{}' confirmation timed out after {}s",
                    action.caller, action.action_type, rule_name, timeout_s
                )
            }
        };
        // Receipt-Backed Run Timeline: emit a canonical ExecutionReceipt for
        // this governed dispatch (the gateway is the daemon's chokepoint) →
        // run timeline file + the memory chunk below.
        let receipt = crate::receipt::build_receipt(action, outcome);
        crate::receipt::record_receipt(&receipt);

        let chunk = NewMemoryChunk {
            kind: ChunkKind::Action,
            source: ChunkSource::Gateway,
            session_id: action.agent_session_id.clone(),
            project_root: action.project_root.clone(),
            caller_id: action.caller.clone(),
            content: summary,
            metadata: json!({
                "action_type": action.action_type,
                "action_args": action.action_args,
                "outcome": serde_json::to_value(outcome)
                    .unwrap_or(Value::Null),
                "receipt": serde_json::to_value(&receipt).unwrap_or(Value::Null),
            }),
            importance: Some(if outcome.executed() { 0.5 } else { 0.7 }),
            shareable: false,
            pinned: false,
        };
        self.memory.write(chunk).await?;
        Ok(())
    }
}

fn make_event(action: &ProposedAction) -> Event {
    let mut data = BTreeMap::new();
    data.insert("caller".to_string(), Value::String(action.caller.clone()));
    data.insert(
        "action_type".to_string(),
        Value::String(action.action_type.clone()),
    );
    data.insert("action_args".to_string(), action.action_args.clone());
    if let Some(sid) = &action.agent_session_id {
        data.insert("agent_session_id".to_string(), Value::String(sid.clone()));
    }
    if let Some(root) = &action.project_root {
        data.insert("project_root".to_string(), Value::String(root.clone()));
    }
    Event {
        ts: Utc::now(),
        source: EventSource::CelActGateway,
        kind: EventKind::AgentActionAttempted,
        data,
    }
}

fn collect_all_fires(decision: &Decision) -> Vec<FiredRuleSnapshot> {
    match decision {
        Decision::Allow { passthrough_fires } => passthrough_fires.clone(),
        Decision::Veto {
            rule,
            passthrough_fires,
            ..
        } => {
            let mut v = passthrough_fires.clone();
            v.insert(0, rule.clone());
            v
        }
        Decision::RequireConfirmation {
            rule,
            passthrough_fires,
        } => {
            let mut v = passthrough_fires.clone();
            v.insert(0, rule.clone());
            v
        }
    }
}
