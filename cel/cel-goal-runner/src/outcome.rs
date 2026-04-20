//! Goal execution outcomes.

use serde::{Deserialize, Serialize};

/// Status of a completed goal execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Achieved,
    Failed,
    MaxSteps,
    Timeout,
    Cancelled,
}

/// Result of a goal execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalResult {
    pub status: GoalStatus,
    pub summary: String,
    pub total_steps: u32,
    pub duration_ms: u64,
    pub metrics: GoalMetrics,
    /// Per-step record of actions taken. Populated when the runner is configured
    /// to capture trace data (default on; eval/replay/observability use it).
    #[serde(default)]
    pub action_log: Vec<ActionRecord>,
}

/// Metrics collected during goal execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoalMetrics {
    pub llm_calls: u32,
    pub vision_calls: u32,
    pub context_reads: u32,
    pub action_successes: u32,
    pub action_failures: u32,
    pub replans: u32,
    pub total_llm_tokens: u64,
    /// Count of actions that were dropped pre-execute because their planned
    /// target no longer existed in the fresh context (Phase 2). Each stale
    /// target causes a replan on the next step — tracked separately from
    /// `replans` so we can tell "LLM wanted to try again" vs "target gone".
    #[serde(default)]
    pub stale_targets: u32,
    /// Count of out-of-band `Cortex::refresh_now` calls the runner made
    /// across Perceive and pre-Execute phases (Phase 2). Roughly `2 *
    /// total_steps` for normal runs; useful for spotting tick-latency
    /// regressions.
    #[serde(default)]
    pub refreshes: u32,
}

/// Outcome of a single step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub action_type: String,
    pub success: bool,
    pub verified: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// A single action taken during goal execution.
/// Captures the kind, target (when applicable), and outcome — enough for
/// downstream eval/replay to reconstruct what the agent did and assert on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRecord {
    pub step_index: u32,
    /// Lowercase action discriminant: "click", "type", "set_value",
    /// "ax_action", "cdp_eval", "key_combo", "scroll", "drag", "wait",
    /// "extract", "act", "activate_app", "select", "done", "fail", "batch",
    /// "custom:<name>", "notebook_writes".
    pub kind: String,
    /// AxAction subtype when kind == "ax_action" ("press", "activate", etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// Element ID when the action targets a specific element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    /// Free-form arg payload (text typed, JS evaluated, etc.). Truncated
    /// to a reasonable length to keep traces small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// Planner self-reported confidence for the step that produced this action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_confidence: Option<f64>,
    /// Whether the action was reported successful (cortex success OR verified diff).
    pub succeeded: bool,
    /// Whether a context diff was observed after the action.
    pub verified: bool,
    /// Wall-clock latency for this step.
    pub latency_ms: u64,
    /// Error string when the action failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
