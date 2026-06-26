//! `PlanProducer` trait — the planner side of the canonical agent contract.
//!
//! One method, one decision: given the goal, the full history of what the
//! agent has done so far, the live perception + screenshot, return the next
//! move (a batch of steps, or Done, or Fail, or Clarify).
//!
//! Lives in `cel-contracts` so a runner can describe the planner contract
//! without depending on a specific planner implementation, and so any planner
//! runtime can implement it as a peer.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::canonical::{AttemptRecord, NextMove};
use crate::view::PlanningView;

/// Decides the next thing the agent should do.
///
/// Called by the canonical runner once per turn. Implementations must
/// be safe to call repeatedly from async contexts.
///
/// Takes a [`PlanningView`] that folds together perception, adapter facts,
/// capabilities, selected memory/events, and run progress in one budgeted
/// shape. Planners are pure consumers of this contract; they do not need to own
/// context selection logic.
#[async_trait]
pub trait PlanProducer: Send + Sync {
    /// Return the next move.
    ///
    /// Inputs:
    /// * `goal` — the original natural-language goal (unchanged across turns).
    /// * `history` — every step that has run, oldest first. Contains the
    ///   planner's own prior intents plus what actually happened
    ///   (success/error + any extracted data).
    /// * `shared_memory` — free-form JSON bag of extracted data the runner
    ///   has accumulated (e.g. prices scraped from a page).
    /// * `view` — the budgeted `PlanningView` built from current runtime
    ///   context. Includes the goal-relevant elements, adapter facts,
    ///   capabilities, run progress, and selected memories / knowledge /
    ///   events.
    /// * `screenshot_png` — optional PNG bytes of the current screen for
    ///   vision-capable models. `None` on headless / no-capture
    ///   environments; planners should still produce a reasonable answer
    ///   from the view alone.
    ///
    /// On success, returns one of:
    /// * `NextMove::Batch { purpose, steps }` — run these steps in
    ///   order, then call again.
    /// * `NextMove::Done { summary, extracted_data }` — terminate as
    ///   `GoalOutcome::Succeeded`.
    /// * `NextMove::Fail { reason }` — terminate as
    ///   `GoalOutcome::Failed`.
    /// * `NextMove::Clarify { question }` — refuse to act because the
    ///   goal is too ambiguous or destructive without confirmation;
    ///   terminate as `GoalOutcome::Refused` with `question` in the
    ///   summary.
    ///
    /// Errors propagate to the runner as a planner-layer failure
    /// (e.g. LLM down, parse failure) and terminate the run.
    async fn decide_next(
        &self,
        goal: &str,
        history: &[AttemptRecord],
        shared_memory: &serde_json::Value,
        view: &PlanningView,
        screenshot_png: Option<&[u8]>,
    ) -> Result<NextMove, String>;

    /// Validate a `Done` claim against the latest planning view + screenshot.
    ///
    /// When the planner emits `NextMove::Done { summary, .. }`, the runner
    /// calls this once with the current state before accepting the
    /// terminal. A `verified = false` verdict rejects the Done — the
    /// runner records it as a failed attempt (so the planner sees it in
    /// history) and loops again.
    ///
    /// Default implementation accepts any Done (`verified = true`).
    /// Concrete producers (e.g. an LLM-backed one) override this to make
    /// a cheap grader call against the goal.
    async fn verify_done(
        &self,
        _goal: &str,
        _summary: &str,
        _shared_memory: &serde_json::Value,
        _view: &PlanningView,
        _screenshot_png: Option<&[u8]>,
    ) -> Result<DoneVerdict, String> {
        Ok(DoneVerdict {
            verified: true,
            reason: String::new(),
            next_action_hint: None,
        })
    }
}

/// Outcome of a runtime Done-validation check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneVerdict {
    /// True if the current perception/screenshot supports the claimed
    /// summary. False rejects the terminal and makes the runner loop.
    pub verified: bool,
    /// Human-readable reason — shown to the planner as the failure
    /// record on rejection, empty string when verified.
    pub reason: String,
    /// Optional categorical signal from the grader to the planner about
    /// what to do next when `verified = false`. Closes the gap where
    /// the planner reads a free-form `reason` like "Send Message
    /// button still present and accessible" and infers "I should
    /// verify state" rather than the right action ("re-click submit").
    /// Defaults to `None` for back-compat: graders that don't emit a
    /// hint behave exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action_hint: Option<NextActionHint>,
}

/// Categorical recovery suggestion from the verify_done grader to the
/// planner. The grader looks at the rejection's root cause and emits
/// the most useful next move, so the planner doesn't have to infer it
/// from the prose `reason`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NextActionHint {
    /// The agent's last action looks correct in shape but the page
    /// didn't react — re-running it should fire. Most common when a
    /// click handler dropped its event due to a transient state
    /// (modal closing animation, focus loss, race with re-render).
    /// Planner contract: if the previous AttemptRecord's hint is
    /// `RetryLastAction`, the next batch should re-emit that exact
    /// action — possibly with `expect_after` if it lacked one — and
    /// NOT emit a "verify state" batch (the runtime already verified
    /// it didn't happen).
    RetryLastAction,
    /// The agent's last action targeted the right intent but the
    /// approach is wrong — same goal, different shape (e.g. switch
    /// from `click` to `cdp_eval`-with-trusted-event, or fall back to
    /// a key shortcut).
    DifferentAction,
    /// The action shape is right but the `target_id` is wrong — the
    /// element it landed on isn't the one the goal needs. Most
    /// common when the planner hallucinated a `dom:role:slug` from
    /// the visible label and dispatch landed on a different
    /// candidate.
    DifferentTarget,
    /// The grader believes the goal is genuinely unachievable from
    /// here — emit Fail rather than burning more budget. Use
    /// sparingly; the planner can ignore this and try once more.
    GiveUp,
}
