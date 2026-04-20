//! State machine for the goal-runner execution loop.
//!
//! Each state transition is a pure function: (state, input) → (next_state, effects).
//! This makes the state machine fully unit-testable without I/O.

use serde::{Deserialize, Serialize};

/// The current phase of the goal-runner loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerPhase {
    /// Initial setup: validate config, prepare context.
    Init,
    /// Read from Cortex mental model (zero-copy, no FFI).
    Perceive,
    /// Call the Rust planner to decide the next action.
    Plan,
    /// Execute the planned action (may cross to TS for browser actions).
    Execute,
    /// Read fresh context and verify the action landed.
    Verify,
    /// Update cognitive trail, notebook, progress tracking.
    Reflect,
    /// Check budget, timeout, loop detection, escalation. Decide: continue or stop.
    Gate,
    /// Goal completed (success, failure, timeout, or max steps).
    Complete,
}

/// Full state of the runner at any point in time.
#[derive(Debug)]
pub struct RunnerState {
    pub phase: RunnerPhase,
    pub step_index: u32,
    pub max_steps: u32,
    pub consecutive_failures: u32,
    pub consecutive_scrolls: u32,
    /// Phase 5.1: consecutive LLM rate-limit (429/529) responses that
    /// reached the runner (i.e. survived the LLM client's internal
    /// backoff). Reset on any non-rate-limit error or successful step.
    pub consecutive_rate_limits: u32,
    pub start_time_ms: u64,
    pub timeout_ms: u64,
    pub last_action_type: Option<String>,
    pub last_action_success: bool,
    pub terminal: bool,
    pub cancel_requested: bool,
}

impl RunnerState {
    pub fn new(max_steps: u32, timeout_ms: u64) -> Self {
        Self {
            phase: RunnerPhase::Init,
            step_index: 0,
            max_steps,
            consecutive_failures: 0,
            consecutive_scrolls: 0,
            consecutive_rate_limits: 0,
            start_time_ms: now_ms(),
            timeout_ms,
            last_action_type: None,
            last_action_success: false,
            terminal: false,
            cancel_requested: false,
        }
    }

    /// Check if the runner should stop.
    pub fn should_stop(&self) -> bool {
        self.terminal
            || self.cancel_requested
            || self.step_index >= self.max_steps
            || self.elapsed_ms() >= self.timeout_ms
    }

    /// Milliseconds elapsed since start.
    pub fn elapsed_ms(&self) -> u64 {
        now_ms().saturating_sub(self.start_time_ms)
    }

    /// Transition to next phase. Returns the previous phase.
    pub fn transition(&mut self, next: RunnerPhase) -> RunnerPhase {
        let prev = self.phase;
        self.phase = next;
        prev
    }

    /// Record a successful action.
    pub fn record_success(&mut self, action_type: &str) {
        self.last_action_type = Some(action_type.to_string());
        self.last_action_success = true;
        self.consecutive_failures = 0;
        self.consecutive_rate_limits = 0;
        if action_type != "scroll" {
            self.consecutive_scrolls = 0;
        }
    }

    /// Record a failed action.
    pub fn record_failure(&mut self, action_type: &str) {
        self.last_action_type = Some(action_type.to_string());
        self.last_action_success = false;
        self.consecutive_failures += 1;
        if action_type == "scroll" {
            self.consecutive_scrolls += 1;
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let state = RunnerState::new(30, 120_000);
        assert_eq!(state.phase, RunnerPhase::Init);
        assert_eq!(state.step_index, 0);
        assert!(!state.should_stop());
    }

    #[test]
    fn test_max_steps_stop() {
        let mut state = RunnerState::new(5, 120_000);
        state.step_index = 5;
        assert!(state.should_stop());
    }

    #[test]
    fn test_cancel_stop() {
        let mut state = RunnerState::new(30, 120_000);
        state.cancel_requested = true;
        assert!(state.should_stop());
    }

    #[test]
    fn test_transition() {
        let mut state = RunnerState::new(30, 120_000);
        let prev = state.transition(RunnerPhase::Perceive);
        assert_eq!(prev, RunnerPhase::Init);
        assert_eq!(state.phase, RunnerPhase::Perceive);
    }

    #[test]
    fn test_record_success_resets_failures() {
        let mut state = RunnerState::new(30, 120_000);
        state.consecutive_failures = 3;
        state.record_success("click");
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.last_action_success);
    }

    #[test]
    fn test_record_failure_increments() {
        let mut state = RunnerState::new(30, 120_000);
        state.record_failure("click");
        state.record_failure("click");
        assert_eq!(state.consecutive_failures, 2);
        assert!(!state.last_action_success);
    }

    #[test]
    fn test_scroll_tracking() {
        let mut state = RunnerState::new(30, 120_000);
        state.record_failure("scroll");
        state.record_failure("scroll");
        state.record_failure("scroll");
        assert_eq!(state.consecutive_scrolls, 3);
        state.record_success("click");
        assert_eq!(state.consecutive_scrolls, 0);
    }
}
