//! Reference implementations of the gateway's trait surfaces for tests and
//! the v1 daemon skeleton.
//!
//! These are intentionally simple. The production daemon swaps each one for
//! a real implementation (SQLite-backed rule source, IPC-backed broker,
//! `cel_act`-real actuator) without changing any of the gateway's logic.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::action::{ConfirmationDecision, ConfirmationRequest, ProposedAction};
use crate::traits::{Actuator, ConfirmationBroker};

/// Records every action it's asked to execute and returns a configurable
/// stock response. The v1 daemon's first end-to-end tests use this.
#[derive(Debug, Default)]
pub struct RecordingActuator {
    /// Every action this actuator has been asked to execute, in order.
    pub calls: Mutex<Vec<ProposedAction>>,
    /// JSON value returned by `execute`. Defaults to `null`.
    pub response: Value,
}

impl RecordingActuator {
    /// New actuator with `null` response.
    pub fn new() -> Self {
        Self::default()
    }

    /// New actuator with a custom JSON response.
    pub fn with_response(response: Value) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            response,
        }
    }

    /// Number of times `execute` has been called.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl Actuator for RecordingActuator {
    async fn execute(&self, action: &ProposedAction) -> Result<Value, String> {
        self.calls.lock().unwrap().push(action.clone());
        Ok(self.response.clone())
    }
}

/// Always returns `Allow`. Useful for tests that don't care about the
/// confirmation path.
#[derive(Debug, Default)]
pub struct AutoAllowBroker;

#[async_trait]
impl ConfirmationBroker for AutoAllowBroker {
    async fn request_confirmation(
        &self,
        _req: ConfirmationRequest,
    ) -> Result<ConfirmationDecision, String> {
        Ok(ConfirmationDecision::Allow)
    }
}

/// Always returns `Deny`.
#[derive(Debug, Default)]
pub struct AutoDenyBroker;

#[async_trait]
impl ConfirmationBroker for AutoDenyBroker {
    async fn request_confirmation(
        &self,
        _req: ConfirmationRequest,
    ) -> Result<ConfirmationDecision, String> {
        Ok(ConfirmationDecision::Deny)
    }
}

/// Returns decisions from a preprogrammed queue, exhausts to error.
#[derive(Debug)]
pub struct ScriptedBroker {
    queue: Mutex<Vec<ConfirmationDecision>>,
}

impl ScriptedBroker {
    /// Build a broker that hands out the supplied decisions in order.
    /// `decisions[0]` is returned to the first call, `decisions[1]` to the
    /// second, etc.
    pub fn new(decisions: Vec<ConfirmationDecision>) -> Self {
        Self {
            queue: Mutex::new(decisions.into_iter().rev().collect()),
        }
    }
}

#[async_trait]
impl ConfirmationBroker for ScriptedBroker {
    async fn request_confirmation(
        &self,
        _req: ConfirmationRequest,
    ) -> Result<ConfirmationDecision, String> {
        self.queue
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| "scripted broker exhausted".to_string())
    }
}

/// Tiny helper to build a [`ProposedAction`] with sensible defaults for tests.
pub fn fake_action(caller: &str, action_type: &str) -> ProposedAction {
    ProposedAction {
        caller: caller.into(),
        action_type: action_type.into(),
        action_args: json!({}),
        agent_session_id: None,
        project_root: None,
    }
}
