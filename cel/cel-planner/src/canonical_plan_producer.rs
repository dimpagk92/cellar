//! `PlanProducer` trait — the planner side of the canonical agent
//! contract.
//!
//! One method, one decision: given the goal, the full history of what
//! the agent has done so far, the live perception + screenshot, return
//! the next move (a batch of steps, or Done, or Fail). The runner
//! calls this every turn, never commits to a plan past the next
//! batch, and never replans a fixed structure — because there is no
//! fixed structure.
//!
//! This replaces the earlier upfront-plan-plus-replan_sub_goal design
//! (`plan` + `revise_step` + `replan_sub_goal` in previous versions
//! of this file). See `docs/canonical-agent-plan.md` for the
//! motivation; the short version: predefined sub-goal lists trap the
//! agent into racing through a skeleton that doesn't match reality.

use async_trait::async_trait;
use cel_context::ScreenContext;

use crate::canonical::{AttemptRecord, NextMove, RuntimeCaps};

/// Decides the next thing the agent should do.
///
/// Called by the canonical runner once per turn. Implementations must
/// be safe to call repeatedly from async contexts; concrete versions
/// like [`super::llm_plan_producer::LlmPlanProducer`] internally hold
/// an `Arc<LlmClient>`.
#[async_trait]
pub trait PlanProducer: Send + Sync {
    /// Return the next move.
    ///
    /// Inputs:
    /// * `goal` — the original natural-language goal (unchanged across turns).
    /// * `history` — every step that has run, oldest first. Contains
    ///   the planner's own prior intents plus what actually happened
    ///   (success/error + any extracted data).
    /// * `shared_memory` — free-form JSON bag of extracted data the
    ///   runner has accumulated (e.g. prices scraped from a page).
    /// * `perception` — fresh accessibility-tree snapshot.
    /// * `screenshot_png` — optional PNG bytes of the current screen
    ///   for vision-capable models. `None` on headless / no-capture
    ///   environments; planners should still produce a reasonable
    ///   answer from perception alone in that case.
    ///
    /// On success, returns one of:
    /// * `NextMove::Batch { purpose, steps }` — run these steps in
    ///   order, then call again.
    /// * `NextMove::Done { summary, extracted_data }` — terminate as
    ///   `GoalOutcome::Succeeded`.
    /// * `NextMove::Fail { reason }` — terminate as
    ///   `GoalOutcome::Failed`.
    ///
    /// Errors propagate to the runner as a planner-layer failure
    /// (e.g. LLM down, parse failure) and terminate the run.
    async fn decide_next(
        &self,
        goal: &str,
        history: &[AttemptRecord],
        shared_memory: &serde_json::Value,
        perception: &ScreenContext,
        screenshot_png: Option<&[u8]>,
        caps: &RuntimeCaps,
    ) -> Result<NextMove, String>;

    /// Validate a `Done` claim against fresh perception + screenshot.
    ///
    /// When the planner emits `NextMove::Done { summary, .. }`, the
    /// runner calls this once with the current state before accepting
    /// the terminal. A `verified = false` verdict rejects the Done —
    /// the runner records it as a failed attempt (so the planner sees
    /// it in history) and loops again.
    ///
    /// Default implementation accepts any Done (`verified = true`).
    /// The LLM-backed producer overrides this to make a cheap grader
    /// call against the goal.
    async fn verify_done(
        &self,
        _goal: &str,
        _summary: &str,
        _shared_memory: &serde_json::Value,
        _perception: &ScreenContext,
        _screenshot_png: Option<&[u8]>,
    ) -> Result<DoneVerdict, String> {
        Ok(DoneVerdict { verified: true, reason: String::new() })
    }
}

/// Outcome of a runtime Done-validation check.
#[derive(Debug, Clone)]
pub struct DoneVerdict {
    /// True if the current perception/screenshot supports the claimed
    /// summary. False rejects the terminal and makes the runner loop.
    pub verified: bool,
    /// Human-readable reason — shown to the planner as the failure
    /// record on rejection, empty string when verified.
    pub reason: String,
}
