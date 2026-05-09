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

use cel_contracts::{
    AttemptRecord, GoalOutcome, PlanProducer, PlanningBudget, RunLimits, RuntimeCaps, Step,
};
use cel_cortex::{build_planning_view, PlanningViewInputs};
use cel_goal_runner::{
    resolve_runtime_backend, CanonicalGoalRunner, CortexStepExecutor, GoalConfig, RuntimeBackend,
    StepExecutor,
};
use cel_planner::LlmPlanProducer;
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
    serde_json::from_str::<CanonicalJsConfig>(config_json).or_else(|_| {
        let unquoted: String = serde_json::from_str(config_json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid config JSON (unwrap): {e}")))?;
        serde_json::from_str::<CanonicalJsConfig>(&unquoted)
            .map_err(|e| napi::Error::from_reason(format!("Invalid config JSON (inner): {e}")))
    })
}

async fn run_local_canonical(config: CanonicalJsConfig) -> napi::Result<String> {
    let cortex = crate::cortex::get_cortex_handle()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running — call boot_cortex() first"))?;
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
        terminal_app: None,
        workflow_id_for_memory: None,
        memory_db_path: None,
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

#[derive(Debug, serde::Serialize)]
struct CanonicalPerceptionSnapshot {
    perception: cel_context::ScreenContext,
    screenshot_base64: Option<String>,
    caps: RuntimeCaps,
}

fn parse_json_or_default<T>(json: &str) -> napi::Result<T>
where
    T: for<'de> serde::Deserialize<'de> + Default,
{
    if json.trim().is_empty() || json.trim() == "null" {
        return Ok(T::default());
    }
    serde_json::from_str(json).or_else(|_| {
        let unquoted: String = serde_json::from_str(json)
            .map_err(|e| napi::Error::from_reason(format!("Invalid JSON wrapper: {e}")))?;
        if unquoted.trim().is_empty() || unquoted.trim() == "null" {
            Ok(T::default())
        } else {
            serde_json::from_str(&unquoted)
                .map_err(|e| napi::Error::from_reason(format!("Invalid inner JSON: {e}")))
        }
    })
}

fn parse_screenshot_base64(screenshot_base64: Option<String>) -> napi::Result<Option<Vec<u8>>> {
    use base64::Engine as _;

    let Some(raw) = screenshot_base64 else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let payload = raw
        .split_once("base64,")
        .map(|(_, data)| data)
        .unwrap_or(raw.as_str());
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map(Some)
        .map_err(|e| napi::Error::from_reason(format!("Invalid screenshot base64: {e}")))
}

#[napi]
pub async fn canonical_perceive(capture_screenshot: Option<bool>) -> napi::Result<String> {
    crate::ensure_tracing_init();
    let cortex = crate::cortex::get_cortex_handle()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running — call boot_cortex() first"))?;
    let executor = CortexStepExecutor::new(cortex);
    let perception = executor.perceive().await;
    let screenshot_base64 = if capture_screenshot.unwrap_or(true) {
        executor
            .screenshot_png()
            .await
            .map(|png| cel_llm::base64_encode(&png))
    } else {
        None
    };
    let caps = executor.capabilities().await;
    serde_json::to_string(&CanonicalPerceptionSnapshot {
        perception,
        screenshot_base64,
        caps,
    })
    .map_err(|e| napi::Error::from_reason(format!("Snapshot serialization failed: {e}")))
}

/// Build a budgeted `PlanningView` from current cortex state (or from
/// caller-provided perception + caps for stateless mode).
///
/// **Stateful** (default): `perception_json` and `caps_json` are `None`.
/// Uses the booted cortex via [`canonical_perceive`] to read fresh
/// perception + caps, then builds the view. Errors if the cortex isn't
/// running.
///
/// **Stateless**: `perception_json` and `caps_json` are both provided.
/// Skips the cortex entirely and builds the view from the caller's
/// inputs. Useful for the eval harness, replay tooling, and any caller
/// that already has a perception snapshot.
///
/// `budget_json` is an optional serialized [`PlanningBudget`]. When
/// omitted, defaults are used (see `PlanningBudget::default`).
#[napi]
pub async fn canonical_build_planning_view(
    goal: String,
    budget_json: Option<String>,
    perception_json: Option<String>,
    caps_json: Option<String>,
) -> napi::Result<String> {
    crate::ensure_tracing_init();

    let budget: PlanningBudget = budget_json
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            serde_json::from_str(s)
                .map_err(|e| napi::Error::from_reason(format!("Invalid budget JSON: {e}")))
        })
        .transpose()?
        .unwrap_or_default();

    let view = match (perception_json, caps_json) {
        (Some(p_json), Some(c_json)) => {
            // Stateless path: caller supplies the inputs.
            let perception: cel_context::ScreenContext = serde_json::from_str(&p_json)
                .map_err(|e| napi::Error::from_reason(format!("Invalid perception JSON: {e}")))?;
            let caps: RuntimeCaps = parse_json_or_default(&c_json)?;
            build_planning_view(&PlanningViewInputs {
                goal: &goal,
                budget: &budget,
                perception: &perception,
                caps: &caps,
                memory_store: None,
                knowledge_store: None,
                workflow_id: None,
                goal_embedding: None,
                recent_events_store: None,
                cortex_anomalies: None,
                cortex_freshness: None,
                adapter_facts: None,
            })
        }
        (None, None) => {
            // Stateful path: use the booted cortex.
            let cortex = crate::cortex::get_cortex_handle().ok_or_else(|| {
                napi::Error::from_reason(
                    "Cortex not running — call boot_cortex() first or pass perception_json/caps_json for stateless mode",
                )
            })?;
            let executor = CortexStepExecutor::new(cortex);
            let perception = executor.perceive().await;
            let caps = executor.capabilities().await;
            build_planning_view(&PlanningViewInputs {
                goal: &goal,
                budget: &budget,
                perception: &perception,
                caps: &caps,
                memory_store: None,
                knowledge_store: None,
                workflow_id: None,
                goal_embedding: None,
                recent_events_store: None,
                cortex_anomalies: None,
                cortex_freshness: None,
                adapter_facts: None,
            })
        }
        _ => {
            return Err(napi::Error::from_reason(
                "Stateless mode requires BOTH perception_json and caps_json (or pass neither for stateful mode)",
            ));
        }
    };

    serde_json::to_string(&view)
        .map_err(|e| napi::Error::from_reason(format!("PlanningView serialization failed: {e}")))
}

/// Run the LLM planner's `decide_next` against a `PlanningView`.
///
/// Pure planner call — no cortex dependency. Caller is expected to have
/// built the view via [`canonical_build_planning_view`] (or any other
/// source that produces a valid `PlanningView` JSON).
///
/// **Signature changed in PR1b**: previously took `perception_json` +
/// `caps_json`. Now takes `view_json`. The cortex/planner separation
/// at the Rust contract is mirrored on the JS surface.
#[napi]
pub async fn canonical_decide_next(
    view_json: String,
    history_json: String,
    shared_memory_json: String,
    screenshot_base64: Option<String>,
) -> napi::Result<String> {
    crate::ensure_tracing_init();
    let llm_client = cel_llm::create_client().map_err(|e| {
        napi::Error::from_reason(format!(
            "LLM client not configured (set CEL_LLM_PROVIDER or ~/.cellar/config.toml): {e}"
        ))
    })?;
    let planner = LlmPlanProducer::new(Arc::new(llm_client));
    let view: cel_contracts::PlanningView = serde_json::from_str(&view_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid PlanningView JSON: {e}")))?;
    let history: Vec<AttemptRecord> = parse_json_or_default(&history_json)?;
    let shared_memory: serde_json::Value = parse_json_or_default(&shared_memory_json)?;
    let screenshot = parse_screenshot_base64(screenshot_base64)?;
    let next = planner
        .decide_next(
            &view.goal,
            &history,
            &shared_memory,
            &view,
            screenshot.as_deref(),
        )
        .await
        .map_err(napi::Error::from_reason)?;
    serde_json::to_string(&next)
        .map_err(|e| napi::Error::from_reason(format!("NextMove serialization failed: {e}")))
}

/// Run the LLM planner's `verify_done` against a `PlanningView`.
///
/// **Signature changed in PR1b**: previously took `perception_json`. Now
/// takes `view_json`. Caller builds the view via
/// [`canonical_build_planning_view`].
#[napi]
pub async fn canonical_verify_done(
    view_json: String,
    summary: String,
    shared_memory_json: String,
    screenshot_base64: Option<String>,
) -> napi::Result<String> {
    crate::ensure_tracing_init();
    let llm_client = cel_llm::create_client().map_err(|e| {
        napi::Error::from_reason(format!(
            "LLM client not configured (set CEL_LLM_PROVIDER or ~/.cellar/config.toml): {e}"
        ))
    })?;
    let planner = LlmPlanProducer::new(Arc::new(llm_client));
    let view: cel_contracts::PlanningView = serde_json::from_str(&view_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid PlanningView JSON: {e}")))?;
    let shared_memory: serde_json::Value = parse_json_or_default(&shared_memory_json)?;
    let screenshot = parse_screenshot_base64(screenshot_base64)?;
    let verdict = planner
        .verify_done(
            &view.goal,
            &summary,
            &shared_memory,
            &view,
            screenshot.as_deref(),
        )
        .await
        .map_err(napi::Error::from_reason)?;
    serde_json::to_string(&verdict)
        .map_err(|e| napi::Error::from_reason(format!("Done verdict serialization failed: {e}")))
}

#[napi]
pub async fn canonical_execute_step(step_json: String) -> napi::Result<String> {
    crate::ensure_tracing_init();
    let cortex = crate::cortex::get_cortex_handle()
        .ok_or_else(|| napi::Error::from_reason("Cortex not running — call boot_cortex() first"))?;
    let step: Step = serde_json::from_str(&step_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid step JSON: {e}")))?;
    let executor = CortexStepExecutor::new(cortex);
    let result = executor.execute(&step, 1).await;
    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("StepResult serialization failed: {e}")))
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
        Some(result_value) => serde_json::to_string(&result_value)
            .map_err(|e| napi::Error::from_reason(format!("Result serialization failed: {e}"))),
        None => Err(napi::Error::from_reason(format!(
            "remote worker returned no result for job {} (status={:?}, error={:?})",
            details.job_id, details.status, details.error
        ))),
    }
}
