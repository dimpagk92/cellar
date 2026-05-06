//! Planner-internal multi-step plan types (`Plan`, `SubGoal`).
//!
//! The boundary types — `NextMove`, `Step`, `StepKind`, `StepResult`,
//! `RuntimeCaps`, `RunLimits`, `AttemptRecord`, `GoalOutcome`, `FailureReport`
//! — moved to `cel-contracts` so the cortex/runner side does not need to
//! depend on `cel-planner`. They are re-exported below for backward
//! compatibility; prefer importing them from `cel_contracts` in new code.

use serde::{Deserialize, Serialize};

pub use cel_contracts::{
    AttemptRecord, FailureReport, GoalOutcome, NextMove, RunLimits, RuntimeCaps, Step, StepKind,
    StepResult,
};

/// What the planner returns for a natural-language goal.
///
/// A plan is always a list of sub-goals, even for a "simple" goal. The
/// agent loop iterates sub-goals in order; earlier sub-goals can leave
/// data in `shared_memory` for later ones to read.
///
/// Note: `Plan` is the legacy upfront-plan shape. The reactive loop in the
/// canonical runner uses [`NextMove::Batch`] instead and never materializes
/// a `Plan` value. Kept here for the few callers (older tests, eval
/// fixtures) that still construct one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Sub-goals in execution order. `sub_goals[i].depends_on` may only
    /// reference indices `j < i`.
    pub sub_goals: Vec<SubGoal>,

    /// Free-form JSON bag the agent writes into as sub-goals complete,
    /// and that later sub-goals can read from. Typical contents:
    /// extracted prices, URLs visited, form values. Kept free-form so we
    /// do not re-introduce a typed notebook schema (one knob we want to
    /// avoid).
    #[serde(default)]
    pub shared_memory: serde_json::Value,
}

/// One ordered chunk of work inside a [`Plan`].
///
/// A sub-goal is the unit the planner reasons about at the "what part
/// of the problem am I on?" level. Example goal:
///
/// > "Get BTC/ETH/SOL prices from Yahoo Finance, put them in Numbers,
/// >  draw a chart, save the brief."
///
/// becomes four sub-goals: gather prices, open Numbers, draw chart,
/// save. Each sub-goal's `steps` are the concrete actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGoal {
    /// Natural-language description — surfaced in failure reports and
    /// logs. Keep it short; this is what a human sees when debugging a
    /// run, not prompt engineering for the LLM.
    pub purpose: String,

    /// Indices of earlier sub-goals whose `shared_memory` contributions
    /// this one consumes. Used by the agent loop to decide whether
    /// planning state needs to be refreshed between sub-goals.
    #[serde(default)]
    pub depends_on: Vec<usize>,

    /// Ordered executable units. Steps within a sub-goal run
    /// sequentially; each one has its own 3-strike retry budget.
    pub steps: Vec<Step>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_contracts::PlannedAction;

    #[test]
    fn plan_roundtrips_through_json() {
        let plan = Plan {
            sub_goals: vec![SubGoal {
                purpose: "gather BTC price".into(),
                depends_on: vec![],
                steps: vec![Step {
                    purpose: "navigate to Yahoo Finance BTC-USD".into(),
                    kind: StepKind::Deterministic,
                    action: PlannedAction::Navigate {
                        url: "https://finance.yahoo.com/quote/BTC-USD/".into(),
                    },
                }],
            }],
            shared_memory: serde_json::json!({}),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sub_goals.len(), 1);
        assert_eq!(back.sub_goals[0].steps.len(), 1);
    }
}
