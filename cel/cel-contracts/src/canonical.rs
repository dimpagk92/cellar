//! Canonical agent contract — the boundary types every caller (CLI, MCP server,
//! eval harness, benchmarks) speaks.
//!
//! These types live at the cortex/planner boundary so neither side has to
//! depend on the other. See `docs/canonical-agent-plan.md` for the original
//! motivation.
//!
//! Deliberately minimal. Each new field would be a new knob that the
//! CLI/eval/MCP paths could drift on. Add one only when a caller genuinely
//! needs it, and add it in one place.

use serde::{Deserialize, Serialize};

use crate::actions::PlannedAction;

/// What the reactive planner decides to do next, given the current
/// state.
///
/// Each turn of the agent loop is: observe → ask planner for the next
/// move → execute it → observe again. The planner never commits past
/// the next batch; if the first step of a batch reveals something
/// surprising the planner will see it on the next call and pivot.
///
/// The old "upfront Plan with a fixed list of SubGoals" is gone — it
/// forced the LLM to commit to a structure before it had perception,
/// which meant Numbers-on-launch-shows-Open-dialog and similar
/// surprises broke whole runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NextMove {
    /// Execute this batch of steps one after another, then ask the
    /// planner again. A batch is "commit to this short sequence" —
    /// typically 1–5 steps. Larger commitments are a smell; use
    /// smaller batches and trust the loop to re-plan.
    Batch {
        /// Short natural-language description of what this batch is
        /// trying to accomplish. Surfaced in logs and failure reports.
        purpose: String,
        steps: Vec<Step>,
    },
    /// Goal achieved — return success.
    Done {
        summary: String,
        #[serde(default)]
        extracted_data: serde_json::Value,
    },
    /// Give up — goal can't be completed given the state.
    Fail { reason: String },
    /// Refuse to act — the goal is too ambiguous or destructive to
    /// attempt safely, and the planner is asking the user for
    /// clarification instead of guessing.
    ///
    /// Terminal like `Fail`, but distinct in intent: the agent isn't
    /// stuck, it's deliberately declining to act on insufficient
    /// information. Surfaced as `GoalOutcome::Refused` with `question`
    /// in the summary so the eval harness, CLI, and MCP server can
    /// display it back to the user without rendering it as an error.
    ///
    /// Use cases (see `NEXT_MOVE_SYSTEM_PROMPT` for the prompt rule):
    /// - "Delete it" with no clear referent on screen.
    /// - "Send the email" with no recipient identified.
    /// - Destructive prompts (delete / overwrite / send / pay) without
    ///   explicit confirmation in the goal text.
    Clarify { question: String },
}

/// Snapshot of what tools / channels the runtime has wired up this
/// turn. Handed to the planner on every `decide_next` call so the
/// LLM picks actions that actually go somewhere — e.g. emitting
/// `cdp_eval` only when a CDP client is bound, and steering away
/// from Safari when our bound browser is Chrome.
///
/// All fields are optional / best-effort; a Cortex that doesn't know
/// its capabilities returns [`RuntimeCaps::default()`] and the
/// planner just runs with less context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeCaps {
    /// True when the cortex has a CDP client bound (i.e. `cdp_eval` /
    /// `navigate` will actually dispatch). When false the planner
    /// should not emit those actions.
    pub cdp_bound: bool,
    /// Human-readable name of the CDP-controlled browser (e.g.
    /// "Google Chrome"). Rendered to the planner so it can prefer
    /// this browser over any other running one.
    pub cdp_browser: Option<String>,
    /// Current URL of the CDP-controlled page, if known. Helps the
    /// planner decide between "already on the right page, extract
    /// now" vs "navigate first".
    pub cdp_url: Option<String>,
    /// True when native-input dispatch is unlocked (mouse, keyboard,
    /// ax_action, activate_app). When false the planner must stick
    /// to browser-only actions.
    pub native_input: bool,
    /// Steps already consumed on this run. Zero on the first call.
    /// Rendered to the planner so it can pace itself — e.g. stop
    /// polishing extraction once most of the budget is spent and
    /// commit to the terminal phase of the goal.
    pub steps_used: u32,
    /// Hard cap on total steps for this run. `steps_remaining =
    /// max_steps - steps_used`. Rendered alongside `steps_used` so
    /// the planner knows how much runway it has.
    pub max_steps: u32,
}

/// What happened when an earlier step ran. The planner sees the full
/// history on every decide_next call so it can reason about what's
/// already been tried.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// Short description of what the step was trying to do (taken
    /// from Step.purpose so the planner sees its own intent).
    pub step_purpose: String,
    /// Serialized action that was dispatched.
    pub action: PlannedAction,
    /// True = executor reported success; false = reported failure.
    pub succeeded: bool,
    /// Error message on failure, Ok data on success (truncated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
    /// Categorical recovery hint promoted from the verify_done grader
    /// (or other runtime checks) up into the AttemptRecord so the
    /// planner sees it as a top-level field rather than buried in the
    /// `error` string. None for ordinary action-failure attempts;
    /// populated for `verify_done`-rejected Done attempts and similar
    /// runtime-grader rejections. See [`crate::NextActionHint`] for
    /// the variants and their planner-side contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action_hint: Option<crate::NextActionHint>,
}

/// One executable unit inside a batch.
///
/// A step wraps exactly one [`PlannedAction`] plus the metadata the
/// agent loop needs: why we're doing it (for failure reports) and
/// whether the LLM is required to execute it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Natural-language description of what this step accomplishes.
    /// Used in `FailureReport.failing_step` and in LLM-assisted retry
    /// prompts as "you are trying to: {purpose}".
    pub purpose: String,

    /// Tells the executor whether it can run the step without an LLM
    /// call at all. `Deterministic` steps (e.g. a navigate to a known
    /// URL, a wait, a shell-style `activate_app`) skip the LLM. That is
    /// the biggest latency win — every deterministic step saves a full
    /// plan round-trip.
    ///
    /// Tolerant to LLM drift: if the model emits an unknown value
    /// (e.g. confuses the step-level `kind` with the action-level
    /// `type` and writes `"kind": "cdp_eval"`), we default to
    /// `Deterministic` rather than failing the whole batch parse.
    /// That's right almost always — the action itself carries the
    /// real semantics — and it's robust to prompt drift.
    #[serde(default, deserialize_with = "deserialize_step_kind_lenient")]
    pub kind: StepKind,

    /// The action to execute.
    pub action: PlannedAction,
}

/// Whether a step needs the LLM to execute it.
///
/// Mostly advisory today — the executor can fall back to an LLM call
/// if a "deterministic" step errors in a way that needs reasoning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// The action is fully specified; run it as-is. Example:
    /// `Navigate { url: "https://finance.yahoo.com/..." }` or `Wait`.
    #[default]
    Deterministic,

    /// The action's parameters need to be filled in at execute time,
    /// given the live perception and the history of prior attempts.
    /// Example: an `Extract` whose selector depends on what the page
    /// currently renders.
    LlmAssisted,
}

fn deserialize_step_kind_lenient<'de, D>(deserializer: D) -> Result<StepKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(deserializer)?;
    Ok(match raw.as_deref() {
        Some("llm_assisted") | Some("LlmAssisted") => StepKind::LlmAssisted,
        _ => StepKind::Deterministic,
    })
}

/// Outcome of a single step attempt.
///
/// On failure, the agent loop will retry up to 3 times per step before
/// producing a [`FailureReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepResult {
    /// The step completed. `data` is a free-form JSON blob that the
    /// agent writes into the plan's `shared_memory`. `discovered_sub_goal`
    /// is how mid-execution surprises (consent walls, auth prompts) get
    /// injected into the current sub-goal's remaining steps.
    Ok {
        #[serde(default)]
        data: serde_json::Value,
        /// Reserved: legacy hook for mid-execution sub-goal injection.
        /// Kept as untyped JSON in cel-contracts so the planner-side
        /// `SubGoal` type stays out of the boundary surface.
        #[serde(default)]
        discovered_sub_goal: Option<serde_json::Value>,
    },

    /// The step failed. `recoverable=true` means the agent may retry;
    /// `recoverable=false` means retry would be pointless (e.g. the
    /// LLM refused, or an invariant was violated) and the 3-strike
    /// budget should skip straight to the failure report.
    Err {
        message: String,
        #[serde(default = "default_recoverable")]
        recoverable: bool,
    },
}

fn default_recoverable() -> bool {
    true
}

/// Terminal outcome of a whole agent run.
///
/// Every caller — CLI, MCP, eval harness — consumes this exact shape.
/// There is no "timeout" or "max steps" variant: those are budget
/// limits, and exhausting one produces a [`FailureReport`] whose
/// `attempts` describe *why* the agent used up the budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GoalOutcome {
    /// The agent reached a satisfying end state. `summary` is the
    /// natural-language recap (what the CLI prints after `=== Result ===`,
    /// what the MCP response shows). `extracted_data` is the plan's
    /// final `shared_memory` — prices, URLs, confirmation numbers, etc.
    Succeeded {
        summary: String,
        #[serde(default)]
        extracted_data: serde_json::Value,
    },

    /// The agent gave up. `report` names which sub-goal and step died,
    /// and carries the last three error messages.
    Failed(FailureReport),

    /// The agent refused to act because the goal was too ambiguous or
    /// destructive to attempt safely, and asked the user for
    /// clarification instead.
    ///
    /// Terminal like `Failed` but semantically distinct: the agent
    /// isn't broken, the goal is. `summary` carries the clarification
    /// question — verbatim what the planner emitted in
    /// [`NextMove::Clarify`]. Callers (CLI, MCP server, eval harness)
    /// render it back to the user as a prompt, not an error.
    Refused {
        /// The clarification question (or refusal explanation) the
        /// planner produced. Free-form text; the runner does not
        /// re-format it. Callers may choose to wrap it in a "the agent
        /// asked: …" frame.
        summary: String,
    },
}

/// Structured explanation for why the agent stopped before success.
///
/// Always surfaced verbatim to the caller — the CLI prints it, the
/// eval harness matches on it, the MCP response includes it. No lossy
/// formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureReport {
    /// Natural-language purpose of the sub-goal that was active when we
    /// gave up. Matches `SubGoal.purpose` exactly.
    pub failing_sub_goal: String,

    /// Natural-language purpose of the specific step that exhausted
    /// its retry budget. Matches `Step.purpose` exactly.
    pub failing_step: String,

    /// Error messages from each attempt, oldest first. Capped at the
    /// configured step-retry budget (default 3). Each entry is the
    /// `StepResult::Err { message }` from one attempt.
    pub attempts: Vec<String>,
}

/// Budget limits for a single agent run.
///
/// These are the *only* knobs the agent loop itself consumes. Callers
/// that used to set `enable_vision` / `enable_decomposition` / `self_heal`
/// / `enable_notebook` are expected to stop — those behaviors are now
/// implicit in the canonical agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLimits {
    /// Total step budget across all sub-goals. A step that retries 3
    /// times still counts as 1 step.
    pub max_steps: u32,

    /// Wall-clock deadline in milliseconds. Measured from
    /// `GoalRunner::run` start, not per sub-goal.
    pub timeout_ms: u64,

    /// Per-step retry cap. Default: 3. See the 3-strike rule in
    /// `docs/canonical-agent-plan.md`.
    pub max_step_retries: u32,

    /// App name the goal should ultimately land its output in (e.g.
    /// `"Numbers"` for a spreadsheet scenario). When set, the runner
    /// enforces a phase-gate: once `steps_used >= max_steps / 2` with
    /// no terminal-app work recorded (no `write_cells`/`save_document`
    /// action, and frontmost app != `terminal_app`), it injects a
    /// synthetic history record telling the planner its next batch
    /// MUST begin with `activate_app(terminal_app)`. On a second
    /// ignore the runner auto-dispatches the activation itself.
    ///
    /// `None` = no phase-gate enforcement (legacy behaviour).
    #[serde(default)]
    pub terminal_app: Option<String>,

    /// PR2 opt-in: when set together with `memory_db_path`, the canonical
    /// runner writes a final outcome memory under this `workflow_id`
    /// after the run terminates (success or failure). Defaults to `None`
    /// — no memory writes happen if the caller doesn't ask for them.
    ///
    /// Mirrors the `cel_perceive start { enable_memory, workflow_id }`
    /// opt-in for sessions; here it's at the runner level so any caller
    /// of `CanonicalGoalRunner::run` (CLI, MCP, eval harness, worker
    /// daemon) can opt in independently.
    #[serde(default)]
    pub workflow_id_for_memory: Option<String>,

    /// PR2 opt-in: SQLite path the canonical runner uses to write the
    /// final outcome memory. Required alongside `workflow_id_for_memory`
    /// for the auto-write to fire. Typically `~/.cellar/cel-store.db`.
    #[serde(default)]
    pub memory_db_path: Option<String>,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_steps: 80,
            timeout_ms: 900_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: None,
            memory_db_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_limits_default_is_the_documented_default() {
        // docs/canonical-agent-plan.md calls out: 3 attempts per step,
        // 80 steps, 15 minute wall clock. Keep them wired to the same
        // constants so the doc and code cannot silently disagree.
        let l = RunLimits::default();
        assert_eq!(l.max_step_retries, 3);
        assert_eq!(l.max_steps, 80);
        assert_eq!(l.timeout_ms, 900_000);
    }

    #[test]
    fn step_result_tags_are_snake_case() {
        // The LLM will emit these exact strings in its plan responses;
        // lock them down so a serde rename does not silently break
        // prompt compatibility.
        let ok = StepResult::Ok {
            data: serde_json::json!({"x": 1}),
            discovered_sub_goal: None,
        };
        let err = StepResult::Err {
            message: "selector returned null".into(),
            recoverable: true,
        };
        assert!(serde_json::to_string(&ok)
            .unwrap()
            .contains("\"status\":\"ok\""));
        assert!(serde_json::to_string(&err)
            .unwrap()
            .contains("\"status\":\"err\""));
    }

    #[test]
    fn clarify_next_move_round_trips_through_json() {
        // The planner emits `{"kind":"clarify","question":"..."}`; lock
        // down the serde tag so a rename can't silently break the
        // prompt contract.
        let mv = NextMove::Clarify {
            question: "What should I delete?".into(),
        };
        let json = serde_json::to_string(&mv).unwrap();
        assert!(json.contains("\"kind\":\"clarify\""));
        assert!(json.contains("\"question\":\"What should I delete?\""));
        let back: NextMove = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, NextMove::Clarify { .. }));
    }

    #[test]
    fn refused_outcome_serializes_with_summary() {
        // Eval harness, CLI, and MCP server all consume the JSON tag —
        // they discriminate on `"status":"refused"`. Lock it down.
        let outcome = GoalOutcome::Refused {
            summary: "Which item should I delete? Please clarify.".into(),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("\"status\":\"refused\""));
        assert!(json.contains("clarify"));
        let back: GoalOutcome = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, GoalOutcome::Refused { .. }));
    }

    #[test]
    fn failure_report_surfaces_last_three_attempts() {
        // 3-strike rule from the plan doc: the report carries exactly
        // the errors the caller needs to debug — no extra, no loss.
        let report = FailureReport {
            failing_sub_goal: "gather BTC price".into(),
            failing_step: "extract price via innerText regex".into(),
            attempts: vec![
                "selector returned null".into(),
                "consent wall blocked script".into(),
                "network error".into(),
            ],
        };
        let outcome = GoalOutcome::Failed(report.clone());
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("\"status\":\"failed\""));
        assert!(json.contains("gather BTC price"));
        assert!(json.contains("network error"));
    }
}
