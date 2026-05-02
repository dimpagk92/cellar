//! CEL Planner — LLM-driven step planning for goal execution.
//!
//! Takes a natural-language goal and produces one [`PlannedStep`] at a time
//! based on the current screen context, Cortex perception signals, and a
//! rolling step history. Adapter-agnostic — works with any source of
//! `ContextElement`s (browser DOM, desktop accessibility tree, native
//! APIs, vision).
//!
//! # Stable public API
//!
//! Callers that want to plan a step without going through the full goal
//! runner should depend on these items:
//!
//! - [`create_planner`] — build a [`Planner`] from env-configured LLM
//! - [`Planner::plan_step`] — the text-only plan call; takes context,
//!   signals, history, and a pre-rendered memory block
//! - [`Planner::plan_step_with_vision`] — same, with a screenshot data URL
//! - [`CortexSignals`] / [`LoadingSignal`] — perception signals struct the
//!   planner consumes (constructed by the goal runner from `MentalModel`)
//! - [`GoalConfig`] — planner configuration (max_steps, context tier, …)
//! - [`PlannedStep`] / [`PlannedAction`] — what the planner returns
//!
//! The [`prompt`] module is public for advanced callers who want to build
//! prompts without calling the LLM (e.g. for eval harnesses or cached
//! replay). [`history`] + [`loop_detector`] are public for the same reason.
//!
//! # Typical usage
//!
//! ```no_run
//! use cel_planner::{create_planner, CortexSignals, GoalConfig};
//! # async fn example(
//! #   context: cel_context::ScreenContext,
//! #   history: cel_planner::history::StepHistory,
//! #   backend: &dyn cel_planner::PlannerBackend,
//! # ) -> Result<(), cel_planner::PlannerError> {
//! let planner = create_planner(GoalConfig::new("Open Hacker News"))?;
//! let signals = CortexSignals::default();
//! let step = planner
//!     .plan_step(
//!         "system prompt",
//!         &context,
//!         &signals,
//!         "", // recent_memory block
//!         &history,
//!         0,   // step_index
//!         &None,
//!         backend,
//!     )
//!     .await?;
//! # Ok(())
//! # }
//! ```

pub mod canonical;
pub mod canonical_plan_producer;
pub mod decompose;
pub mod distiller;
mod error;
pub mod history;
pub mod llm_plan_producer;
mod planner;
pub mod prompt;
pub mod signals;
mod types;

pub use canonical::{
    AttemptRecord, FailureReport, GoalOutcome, NextMove, Plan, RunLimits, RuntimeCaps, Step,
    StepKind, StepResult, SubGoal,
};
pub use canonical_plan_producer::{DoneVerdict, PlanProducer};
pub use error::PlannerError;
pub use history::StepHistory;
pub use llm_plan_producer::{
    build_user_prompt as build_next_move_user_prompt, LlmPlanProducer, NEXT_MOVE_SYSTEM_PROMPT,
};
pub use planner::{find_blocking_error, validate_grounding, Planner, PlannerBackend};
pub use prompt::{build_user_prompt, PromptOptions, PromptResult};
pub use signals::{CortexSignals, LoadingSignal};
pub use types::{
    CellWrite, ContextDetail, ContextTier, GoalConfig, NotebookWrite, PlannedAction, PlannedPlan,
    PlannedStep, PlannerEvent, ProgressAssessment, StepRecord,
};

/// Create a planner from environment-configured LLM.
///
/// Reads `CEL_LLM_PROVIDER` (or `~/.cellar/config.toml`) plus provider-
/// specific env vars (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, …).
///
/// Returns `PlannerError::Llm(LlmError::NotConfigured)` when no provider
/// is configured — the goal runner treats this as "plan without LLM" and
/// falls back to deterministic paths where possible.
pub fn create_planner(config: GoalConfig) -> Result<Planner, PlannerError> {
    let llm = cel_llm::create_client()?;
    Ok(Planner::new(llm, config))
}
