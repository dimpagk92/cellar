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

use cel_goal_runner::{resolve_runtime_backend, GoalConfig, GoalRunner, NoOpCallbacks, RuntimeBackend};
use cellar_worker::{SubmitGoalRequest, WorkerClient};

/// Run a goal using the Rust goal-runner.
///
/// When the runtime backend is `Local` (default), executes in-process via
/// the booted Cortex. When `Remote`, submits to the configured worker and
/// polls until completion.
///
/// Returns the GoalResult as a JSON string.
#[napi]
pub async fn run_goal_rust(config_json: String) -> napi::Result<String> {
    let config = parse_config(&config_json)?;

    match resolve_runtime_backend() {
        RuntimeBackend::Local => run_local(config).await,
        RuntimeBackend::Remote { url, token } => run_remote(url, token, config).await,
    }
}

/// Parse `GoalConfig` from the JS side, tolerating one layer of double-stringify.
fn parse_config(config_json: &str) -> napi::Result<GoalConfig> {
    serde_json::from_str::<GoalConfig>(config_json)
        .or_else(|_| {
            let unquoted: String = serde_json::from_str(config_json).map_err(|e| {
                napi::Error::from_reason(format!("Invalid config JSON (unwrap): {e}"))
            })?;
            serde_json::from_str::<GoalConfig>(&unquoted)
                .map_err(|e| napi::Error::from_reason(format!("Invalid config JSON (inner): {e}")))
        })
}

async fn run_local(config: GoalConfig) -> napi::Result<String> {
    let cortex = crate::cortex::get_cortex_handle().ok_or_else(|| {
        napi::Error::from_reason("Cortex not running — call boot_cortex() first")
    })?;
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new(config, cortex, callbacks);
    let result = runner.run().await;
    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("Result serialization failed: {e}")))
}

async fn run_remote(url: String, token: Option<String>, config: GoalConfig) -> napi::Result<String> {
    tracing::info!(%url, "dispatching goal to remote cellar-worker");
    let client = WorkerClient::new(&url, token);

    let config_value = serde_json::to_value(&config).ok();
    let goal = config.goal.clone();
    let req = SubmitGoalRequest {
        goal: goal.clone(),
        config: config_value,
    };

    let submit = client
        .submit_goal(req)
        .await
        .map_err(|e| napi::Error::from_reason(format!("remote submit failed: {e}")))?;

    // Cap wait at the runner's own timeout + buffer, to avoid hanging indefinitely
    // on a stuck worker.
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
