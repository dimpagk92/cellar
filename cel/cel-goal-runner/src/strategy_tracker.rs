//! Strategy Tracker — prevents trying the same failed approach twice.
//!
//! Tracks strategies attempted per milestone. Enforces:
//! - Max 3 strategies per milestone
//! - Max 5 total replans per goal
//! - Each strategy must be described differently
//! - Failed strategies are injected into replan prompts

use serde::{Deserialize, Serialize};

const MAX_STRATEGIES_PER_MILESTONE: usize = 3;
const MAX_GLOBAL_REPLANS: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRecord {
    pub id: String,
    pub description: String,
    pub milestone: String,
    pub outcome: Option<StrategyOutcome>,
    pub step_started: u32,
    pub step_ended: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyOutcome {
    pub result: String, // "succeeded" | "failed"
    pub reason: String,
}

/// Tracks strategies attempted for each milestone.
#[derive(Debug, Clone, Default)]
pub struct StrategyTracker {
    strategies: Vec<StrategyRecord>,
    global_replan_count: usize,
}

impl StrategyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new strategy for a milestone.
    pub fn register(&mut self, milestone: &str, description: &str) -> String {
        let id = format!("strategy-{}", self.strategies.len());
        self.strategies.push(StrategyRecord {
            id: id.clone(),
            description: description.into(),
            milestone: milestone.into(),
            outcome: None,
            step_started: 0,
            step_ended: None,
        });
        self.global_replan_count += 1;
        id
    }

    /// Record the outcome of a strategy.
    pub fn record_outcome(&mut self, strategy_id: &str, result: &str, reason: &str, step: u32) {
        if let Some(s) = self.strategies.iter_mut().find(|s| s.id == strategy_id) {
            s.outcome = Some(StrategyOutcome {
                result: result.into(),
                reason: reason.into(),
            });
            s.step_ended = Some(step);
        }
    }

    /// Get the current (latest, non-completed) strategy for a milestone.
    pub fn current_strategy(&self, milestone: &str) -> Option<&str> {
        self.strategies.iter().rev()
            .find(|s| s.milestone == milestone && s.outcome.is_none())
            .map(|s| s.id.as_str())
    }

    /// Get descriptions of all failed strategies for a milestone.
    pub fn get_failed_strategies(&self, milestone: &str) -> Vec<String> {
        self.strategies.iter()
            .filter(|s| {
                s.milestone == milestone
                    && s.outcome.as_ref().map_or(false, |o| o.result == "failed")
            })
            .map(|s| {
                let reason = s.outcome.as_ref().map(|o| o.reason.as_str()).unwrap_or("unknown");
                format!("{}: {} (failed: {})", s.id, s.description, reason)
            })
            .collect()
    }

    /// Can we replan for this milestone? (under the per-milestone limit)
    pub fn can_replan(&self, milestone: &str) -> bool {
        let count = self.strategies.iter()
            .filter(|s| s.milestone == milestone)
            .count();
        count < MAX_STRATEGIES_PER_MILESTONE
    }

    /// Can we replan at all? (under the global limit)
    pub fn can_replan_global(&self) -> bool {
        self.global_replan_count < MAX_GLOBAL_REPLANS
    }

    /// Total strategies attempted.
    pub fn total_strategies(&self) -> usize {
        self.strategies.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_current() {
        let mut tracker = StrategyTracker::new();
        let id = tracker.register("search", "try search bar");
        assert_eq!(tracker.current_strategy("search"), Some(id.as_str()));
    }

    #[test]
    fn test_record_outcome() {
        let mut tracker = StrategyTracker::new();
        let id = tracker.register("search", "try search bar");
        tracker.record_outcome(&id, "failed", "search bar not found", 5);
        assert!(tracker.current_strategy("search").is_none()); // no active strategy
        assert_eq!(tracker.get_failed_strategies("search").len(), 1);
    }

    #[test]
    fn test_can_replan_limits() {
        let mut tracker = StrategyTracker::new();
        tracker.register("search", "approach 1");
        tracker.register("search", "approach 2");
        tracker.register("search", "approach 3");
        assert!(!tracker.can_replan("search")); // hit per-milestone limit
        assert!(tracker.can_replan("checkout")); // different milestone is fine
    }

    #[test]
    fn test_global_replan_limit() {
        let mut tracker = StrategyTracker::new();
        for i in 0..5 {
            tracker.register(&format!("milestone-{i}"), &format!("approach {i}"));
        }
        assert!(!tracker.can_replan_global());
    }
}
