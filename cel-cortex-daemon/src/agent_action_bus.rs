//! Broadcast bus and ring for `agent_actions.*` IPC frames.
//!
//! Every `cel_act` call that flows through the gateway ends up here via
//! [`DaemonAgentActionHook`] so the `agent_actions.subscribe` and
//! `agent_actions.recent` IPC methods can serve real-time + backfill data
//! to the Tauri activity tab or CLI watchers.
//!
//! The wire shape: each frame carries a `Value` with the serialized
//! action entry, consistent with [`cellar_ipc::subscription::StreamPayload::AgentAction`].

use std::sync::Arc;

use async_trait::async_trait;
use cel_act_gateway::{ActionOutcome, AgentActionHook, ProposedAction};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast::{self, Receiver, Sender};

use crate::recent::Ring;

// ─────────────────────────────────────────────────────────────────────────────
// Wire type
// ─────────────────────────────────────────────────────────────────────────────

/// One published frame on the agent-action bus. `payload` is the
/// [`cellar_ipc::subscription::StreamPayload::AgentAction`] `action` value —
/// a JSON object with `caller`, `action_type`, `outcome`, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentActionFrame {
    /// Serialised action entry (stable V1 shape).
    pub action: Value,
}

impl AgentActionFrame {
    /// Build a frame from a completed gateway intercept.
    pub fn from_outcome(action: &ProposedAction, outcome: &ActionOutcome) -> Self {
        let (outcome_str, result) = match outcome {
            ActionOutcome::Executed { result } => ("executed", result.clone()),
            ActionOutcome::Vetoed {
                rule_id, rule_name, ..
            } => (
                "vetoed",
                json!({ "rule_id": rule_id, "rule_name": rule_name }),
            ),
            ActionOutcome::ConfirmationDenied { rule_id, rule_name } => (
                "denied",
                json!({ "rule_id": rule_id, "rule_name": rule_name }),
            ),
            ActionOutcome::ConfirmationTimedOut {
                rule_id,
                rule_name,
                timeout_s,
            } => (
                "timed_out",
                json!({ "rule_id": rule_id, "rule_name": rule_name, "timeout_s": timeout_s }),
            ),
        };
        Self {
            action: json!({
                "caller": action.caller,
                "action_type": action.action_type,
                "action_args": action.action_args,
                "agent_session_id": action.agent_session_id,
                "project_root": action.project_root,
                "outcome": outcome_str,
                "result": result,
                "timestamp": Utc::now().to_rfc3339(),
            }),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Bus
// ─────────────────────────────────────────────────────────────────────────────

const BUS_CAPACITY: usize = 2048;

/// Broadcast bus for [`AgentActionFrame`]s. Cheap to clone.
#[derive(Clone)]
pub struct AgentActionBus {
    tx: Sender<AgentActionFrame>,
}

impl AgentActionBus {
    /// New bus with the default capacity.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Publish one frame. No subscribers = silent drop.
    pub fn publish(&self, frame: AgentActionFrame) {
        let _ = self.tx.send(frame);
    }

    /// Subscribe.
    pub fn subscribe(&self) -> Receiver<AgentActionFrame> {
        self.tx.subscribe()
    }
}

impl Default for AgentActionBus {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ring alias
// ─────────────────────────────────────────────────────────────────────────────

/// Bounded ring for `agent_actions.recent` backfill.
pub type AgentActionRing = Ring<AgentActionFrame>;

// ─────────────────────────────────────────────────────────────────────────────
// Hook implementation
// ─────────────────────────────────────────────────────────────────────────────

/// [`AgentActionHook`] that publishes to an [`AgentActionBus`] and pushes
/// to an [`AgentActionRing`] on every gateway intercept. Constructed once at
/// daemon startup and handed to the gateway via `with_action_hook`.
pub struct DaemonAgentActionHook {
    bus: AgentActionBus,
    ring: Arc<AgentActionRing>,
}

impl DaemonAgentActionHook {
    /// Construct the hook.
    pub fn new(bus: AgentActionBus, ring: Arc<AgentActionRing>) -> Self {
        Self { bus, ring }
    }
}

#[async_trait]
impl AgentActionHook for DaemonAgentActionHook {
    async fn on_action(&self, action: &ProposedAction, outcome: &ActionOutcome) {
        let frame = AgentActionFrame::from_outcome(action, outcome);
        self.ring.push(frame.clone());
        self.bus.publish(frame);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cel_act_gateway::ActionOutcome;
    use serde_json::json;

    fn sample_action() -> ProposedAction {
        ProposedAction {
            caller: "embedded".into(),
            action_type: "ax.click".into(),
            action_args: json!({"target": "button"}),
            agent_session_id: Some("sess_1".into()),
            project_root: None,
        }
    }

    #[test]
    fn frame_from_executed_outcome() {
        let action = sample_action();
        let outcome = ActionOutcome::Executed {
            result: json!({"clicked": true}),
        };
        let frame = AgentActionFrame::from_outcome(&action, &outcome);
        assert_eq!(frame.action["caller"], "embedded");
        assert_eq!(frame.action["outcome"], "executed");
        assert_eq!(frame.action["result"]["clicked"], true);
    }

    #[test]
    fn frame_from_vetoed_outcome() {
        let action = sample_action();
        let outcome = ActionOutcome::Vetoed {
            rule_id: "rule_1".into(),
            rule_name: "block moves".into(),
            soft_block: false,
        };
        let frame = AgentActionFrame::from_outcome(&action, &outcome);
        assert_eq!(frame.action["outcome"], "vetoed");
        assert_eq!(frame.action["result"]["rule_name"], "block moves");
    }

    #[tokio::test]
    async fn hook_publishes_to_bus_and_ring() {
        let bus = AgentActionBus::new();
        let ring: Arc<AgentActionRing> = Arc::new(AgentActionRing::new());
        let hook = DaemonAgentActionHook::new(bus.clone(), ring.clone());
        let mut rx = bus.subscribe();

        let action = sample_action();
        let outcome = ActionOutcome::Executed { result: json!({}) };
        hook.on_action(&action, &outcome).await;

        let frame = rx.recv().await.unwrap();
        assert_eq!(frame.action["outcome"], "executed");

        let recent = ring.filtered(10, |_| true);
        assert_eq!(recent.len(), 1);
    }
}
