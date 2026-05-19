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
            default_confirmation_timeout_s: DEFAULT_CONFIRMATION_TIMEOUT_S,
        }
    }

    /// Override the default confirmation timeout (seconds). Rules with an
    /// explicit `timeout_s` still take precedence.
    pub fn with_default_timeout(mut self, secs: u64) -> Self {
        self.default_confirmation_timeout_s = secs;
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
        let matches = Matcher::evaluate(&event, &rules, &self.watchlists);
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
