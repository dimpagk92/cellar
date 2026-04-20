//! Goal-runner configuration.

use serde::{Deserialize, Serialize};

/// Configuration for a goal execution run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalConfig {
    /// The natural language goal to achieve.
    pub goal: String,
    /// Maximum number of steps before giving up.
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// Delay between steps in milliseconds.
    #[serde(default = "default_step_delay")]
    pub step_delay_ms: u64,
    /// Total timeout for the goal in milliseconds.
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Maximum consecutive failures before stopping.
    #[serde(default = "default_max_failures")]
    pub max_consecutive_failures: u32,
    /// Maximum consecutive rate-limit (429/529) responses from the LLM
    /// before the runner bails out. Each HTTP 429 arriving at the planner
    /// has already survived the LLM client's internal 3-attempt backoff —
    /// repeated 429s mean we're out of quota, and continuing to retry
    /// burns steps + billed tokens with no chance of success. Phase 5.1.
    #[serde(default = "default_max_rate_limits")]
    pub max_consecutive_rate_limits: u32,
    /// Enable vision (screenshot) fallback for planning.
    #[serde(default = "default_true")]
    pub enable_vision: bool,
    /// Enable self-healing (retry with different approach on failure).
    #[serde(default = "default_true")]
    pub self_heal: bool,
    /// Enable milestone decomposition for complex goals.
    #[serde(default)]
    pub enable_decomposition: bool,
    /// Enable persistent notebook for data discovered during execution.
    #[serde(default = "default_true")]
    pub enable_notebook: bool,
    /// Workflow name for history scoping and knowledge persistence.
    #[serde(default)]
    pub workflow_name: Option<String>,
    /// LLM provider for planning (gemini, anthropic, openai).
    #[serde(default)]
    pub llm_provider: Option<String>,
    /// LLM model for planning.
    #[serde(default)]
    pub llm_model: Option<String>,
    /// Escalation model (used after consecutive failures).
    #[serde(default)]
    pub llm_escalation_model: Option<String>,
    /// URL to constrain navigation to (blocks search engine redirects).
    #[serde(default)]
    pub constrain_to_url: Option<String>,
    /// Deterministic mode: when set, planner uses temperature=0 for every
    /// LLM call, not just retries. The seed value is also passed to any
    /// non-LLM PRNG used by the runner. Trades exploration for repeatability.
    /// Used by the eval harness for PR-gate runs.
    #[serde(default)]
    pub deterministic_seed: Option<u64>,
}

fn default_max_steps() -> u32 { 30 }
fn default_step_delay() -> u64 { 500 }
fn default_timeout() -> u64 { 120_000 }
fn default_max_failures() -> u32 { 8 }
fn default_max_rate_limits() -> u32 { 3 }
fn default_true() -> bool { true }

impl Default for GoalConfig {
    fn default() -> Self {
        Self {
            goal: String::new(),
            max_steps: default_max_steps(),
            step_delay_ms: default_step_delay(),
            timeout_ms: default_timeout(),
            max_consecutive_failures: default_max_failures(),
            max_consecutive_rate_limits: default_max_rate_limits(),
            enable_vision: true,
            self_heal: true,
            enable_decomposition: false,
            enable_notebook: true,
            workflow_name: None,
            llm_provider: None,
            llm_model: None,
            llm_escalation_model: None,
            constrain_to_url: None,
            deterministic_seed: None,
        }
    }
}
