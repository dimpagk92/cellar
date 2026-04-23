//! Goal Runner — NAPI bindings for the full Rust execution loop.
//!
//! Planning and execution are fully Rust-native.
//! Events are logged to tracing (visible in stderr).
//! Returns GoalResult as JSON when complete.
//!
//! ## Runtime backend dispatch
//!
//! On each call, `resolve_runtime_backend()` decides whether to execute
//! in-process (`Local`) or delegate to a `cellar-worker` over HTTP (`Remote`).
//! Remote configuration comes from `CEL_RUNTIME_*` env vars or
//! `~/.cellar/config.toml` — see `cel-goal-runner::runtime_backend`.

use napi_derive::napi;
use std::sync::Arc;

use cel_goal_runner::{
    resolve_runtime_backend, CanonicalGoalRunner, CortexStepExecutor, GoalConfig, RuntimeBackend,
};
use cel_planner::{GoalOutcome, LlmPlanProducer, RunLimits};
use cellar_worker::{SubmitGoalRequest, WorkerClient};
use serde::Deserialize;

/// Minimal config the canonical agent actually consumes. Anything else the
/// JS caller sends is ignored — the canonical agent has no opt-in flags.
/// See `docs/canonical-agent-plan.md` for why.
#[derive(Debug, Clone, Deserialize)]
struct CanonicalJsConfig {
    goal: String,
    #[serde(default = "default_max_steps")]
    max_steps: u32,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

fn default_max_steps() -> u32 {
    80
}
fn default_timeout_ms() -> u64 {
    900_000
}

/// Run a goal through the canonical agent loop.
///
/// When the runtime backend is `Local` (default), executes in-process via
/// the booted Cortex + CanonicalGoalRunner. When `Remote`, submits to the
/// configured worker and polls until completion. Only `goal`, `max_steps`,
/// and `timeout_ms` are honored on the JS side; legacy flags
/// (`enable_vision`, `enable_decomposition`, `self_heal`, `enable_notebook`)
/// are silently ignored — the canonical agent does not expose them.
#[napi]
pub async fn run_goal_rust(config_json: String) -> napi::Result<String> {
    crate::ensure_tracing_init();
    let config = parse_canonical_config(&config_json)?;

    match resolve_runtime_backend() {
        RuntimeBackend::Local => run_local_canonical(config).await,
        RuntimeBackend::Remote { url, token } => run_remote_canonical(url, token, config).await,
    }
}

/// Parse the minimal canonical config from JS, tolerating double-stringify.
fn parse_canonical_config(config_json: &str) -> napi::Result<CanonicalJsConfig> {
    serde_json::from_str::<CanonicalJsConfig>(config_json)
        .or_else(|_| {
            let unquoted: String = serde_json::from_str(config_json).map_err(|e| {
                napi::Error::from_reason(format!("Invalid config JSON (unwrap): {e}"))
            })?;
            serde_json::from_str::<CanonicalJsConfig>(&unquoted)
                .map_err(|e| napi::Error::from_reason(format!("Invalid config JSON (inner): {e}")))
        })
}

async fn run_local_canonical(config: CanonicalJsConfig) -> napi::Result<String> {
    let cortex = crate::cortex::get_cortex_handle().ok_or_else(|| {
        napi::Error::from_reason("Cortex not running — call boot_cortex() first")
    })?;
    let llm_client = cel_llm::create_client().map_err(|e| {
        napi::Error::from_reason(format!(
            "LLM client not configured (set CEL_LLM_PROVIDER or ~/.cellar/config.toml): {e}"
        ))
    })?;
    let planner = LlmPlanProducer::new(Arc::new(llm_client));
    let executor = CortexStepExecutor::new(cortex);
    let runner = CanonicalGoalRunner::new(planner, executor);
    let limits = RunLimits {
        max_steps: config.max_steps,
        timeout_ms: config.timeout_ms,
        max_step_retries: 3,
    };
    let outcome = runner.run(&config.goal, limits).await;
    serde_json::to_string(&render_outcome_for_js(&outcome))
        .map_err(|e| napi::Error::from_reason(format!("Result serialization failed: {e}")))
}

/// Flatten a canonical [`GoalOutcome`] into the JSON shape the CLI and MCP
/// already know how to display. Keeps `status` / `summary` / `duration_ms`
/// stable so the TS side doesn't need a second schema migration.
fn render_outcome_for_js(outcome: &GoalOutcome) -> serde_json::Value {
    match outcome {
        GoalOutcome::Succeeded {
            summary,
            extracted_data,
        } => serde_json::json!({
            "status": "Achieved",
            "summary": summary,
            "extracted_data": extracted_data,
        }),
        GoalOutcome::Failed(report) => serde_json::json!({
            "status": "Failed",
            "summary": format!(
                "sub_goal `{}` / step `{}` failed after {} attempts",
                report.failing_sub_goal,
                report.failing_step,
                report.attempts.len(),
            ),
            "failure_report": report,
        }),
    }
}

async fn run_remote_canonical(
    url: String,
    token: Option<String>,
    config: CanonicalJsConfig,
) -> napi::Result<String> {
    tracing::info!(%url, "dispatching goal to remote cellar-worker");
    let client = WorkerClient::new(&url, token);

    // Wrap the canonical config in the legacy `GoalConfig` shape the
    // worker still speaks. Once the worker migrates to the canonical
    // agent too, this adapter goes away.
    let worker_config = GoalConfig {
        goal: config.goal.clone(),
        max_steps: config.max_steps,
        timeout_ms: config.timeout_ms,
        ..Default::default()
    };
    let config_value = serde_json::to_value(&worker_config).ok();
    let req = SubmitGoalRequest {
        goal: config.goal.clone(),
        config: config_value,
    };

    let submit = client
        .submit_goal(req)
        .await
        .map_err(|e| napi::Error::from_reason(format!("remote submit failed: {e}")))?;

    let wait_secs = (config.timeout_ms / 1000).saturating_add(30).max(60);
    let details = client
        .wait_for_job(&submit.job_id, wait_secs)
        .await
        .map_err(|e| napi::Error::from_reason(format!("remote wait failed: {e}")))?;

    match details.result {
        Some(result_value) => serde_json::to_string(&result_value).map_err(|e| {
            napi::Error::from_reason(format!("Result serialization failed: {e}"))
        }),
        None => Err(napi::Error::from_reason(format!(
            "remote worker returned no result for job {} (status={:?}, error={:?})",
            details.job_id, details.status, details.error
        ))),
    }
}
