//! Callback trait for observability.
//!
//! The goal-runner handles perception (Cortex), planning (cel-planner), and
//! execution (Cortex dispatch) entirely in Rust. These callbacks are ONLY
//! for event notifications — live-view, benchmarks, analytics.

use serde::{Deserialize, Serialize};

/// Events emitted by the runner for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerEvent {
    pub event_type: RunnerEventType,
    pub step_index: u32,
    pub action: Option<String>,
    pub success: Option<bool>,
    pub details: Option<String>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerEventType {
    StepStarted,
    Planned,
    Executed,
    Verified,
    StepCompleted,
    Replanned,
    GoalCompleted,
    Error,
    /// Planner returned an action whose target_id(s) did not exist in the
    /// fresh pre-execute context — runner triggered a replan instead of
    /// dispatching against stale element bounds.
    StaleTarget,
    /// Runner invoked the vision-enhanced plan path (Phase 3C). Gate: the
    /// cortex flagged sparse context AND the prior step failed with a
    /// target-miss. Expensive relative to text-only plans.
    VisionInvoked,
}

/// Step outcome reported to observers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub step_index: u32,
    pub action_type: String,
    pub success: bool,
    pub verified: bool,
    pub error: Option<String>,
    pub reasoning: String,
    pub duration_ms: u64,
}

/// Observability callbacks — the ONLY interface crossing to TS.
///
/// Planning and execution are fully Rust-native.
/// These callbacks exist solely for TS consumers (live-view, benchmarks)
/// to observe what the runner is doing.
pub trait ExecutionCallbacks: Send + Sync {
    /// Event notification (fire-and-forget).
    fn on_event(&self, event: RunnerEvent);

    /// Step completion notification.
    fn on_step_complete(&self, report: StepReport);
}

/// No-op callbacks for testing or headless execution.
pub struct NoOpCallbacks;

impl ExecutionCallbacks for NoOpCallbacks {
    fn on_event(&self, _event: RunnerEvent) {}
    fn on_step_complete(&self, _report: StepReport) {}
}
