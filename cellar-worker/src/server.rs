//! axum HTTP handlers for the worker protocol.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use crate::protocol::{
    ErrorDetail, ErrorResponse, HealthResponse, JobDetails, JobStatus, SubmitGoalRequest,
    SubmitGoalResponse,
};
use crate::state::{generate_job_id, now_epoch, JobStore};

/// Shared server state — passed into each handler via `State(...)`.
#[derive(Clone)]
pub struct ServerState {
    pub store: JobStore,
    /// Expected bearer token. `None` disables auth (localhost / trusted network).
    pub auth_token: Option<String>,
    pub version: String,
    /// Booted Cortex handle. `None` = stub mode (tests, CI, Docker without X).
    pub cortex: Option<Arc<cel_cortex::Cortex>>,
    /// Serializes real goal executions — one goal at a time per worker
    /// (a single machine can only automate one UI coherently).
    pub exec_lock: Arc<tokio::sync::Mutex<()>>,
}

impl ServerState {
    /// Test/CI helper — produces a state with no Cortex (so execution stays stubbed).
    pub fn stub(version: impl Into<String>) -> Self {
        Self {
            store: JobStore::new(),
            auth_token: None,
            version: version.into(),
            cortex: None,
            exec_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

/// Build the axum router. Keep this public so tests can mount it on an ephemeral port.
pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/goals", post(submit_goal))
        .route("/v1/jobs/{id}", get(get_job))
        .with_state(state)
}

async fn health(State(state): State<ServerState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: state.version,
    })
}

async fn submit_goal(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(req): Json<SubmitGoalRequest>,
) -> Result<(StatusCode, Json<SubmitGoalResponse>), ErrorReply> {
    check_auth(&state, &headers)?;

    let job_id = generate_job_id();
    let created_at = now_epoch();
    let details = JobDetails {
        job_id: job_id.clone(),
        status: JobStatus::Queued,
        created_at,
        updated_at: created_at,
        result: None,
        error: None,
    };
    state.store.insert(details);

    if state.cortex.is_some() {
        spawn_real_execution(&state, job_id.clone(), req.goal, req.config);
    } else {
        spawn_stub_execution(&state, &job_id, &req.goal);
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitGoalResponse {
            job_id,
            status: JobStatus::Queued,
            created_at,
        }),
    ))
}

async fn get_job(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<JobDetails>, ErrorReply> {
    check_auth(&state, &headers)?;
    match state.store.get(&id) {
        Some(job) => Ok(Json(job)),
        None => Err(ErrorReply::new(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("unknown job_id: {id}"),
        )),
    }
}

fn check_auth(state: &ServerState, headers: &HeaderMap) -> Result<(), ErrorReply> {
    let Some(expected) = &state.auth_token else {
        return Ok(());
    };
    let supplied = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if supplied == Some(expected.as_str()) {
        Ok(())
    } else {
        Err(ErrorReply::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        ))
    }
}

/// Stub execution — used when no Cortex is booted (tests, Docker without X,
/// worker started before permissions are granted). Returns a placeholder result.
fn spawn_stub_execution(state: &ServerState, job_id: &str, goal: &str) {
    let store = state.store.clone();
    let id = job_id.to_string();
    let submitted_goal = goal.to_string();
    tokio::spawn(async move {
        store.update_status(&id, JobStatus::Running, None, None);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let stub_result = serde_json::json!({
            "status": "stubbed",
            "submitted_goal": submitted_goal,
            "note": "worker running without Cortex — real execution requires a booted Cortex (see worker logs)",
        });
        store.update_status(&id, JobStatus::Succeeded, Some(stub_result), None);
    });
}

/// Real execution — dispatches through `cel-goal-runner::GoalRunner` using the
/// booted Cortex. Serialized via `exec_lock` so concurrent submits queue up.
fn spawn_real_execution(
    state: &ServerState,
    job_id: String,
    goal: String,
    config_override: Option<serde_json::Value>,
) {
    let store = state.store.clone();
    let exec_lock = state.exec_lock.clone();
    let cortex = state
        .cortex
        .clone()
        .expect("caller verifies cortex is Some before calling spawn_real_execution");

    tokio::spawn(async move {
        // One goal at a time per worker — UI automation cannot parallelize
        // against a single machine's screen/input.
        let _guard = exec_lock.lock().await;
        store.update_status(&job_id, JobStatus::Running, None, None);

        let config = merge_goal_config(goal.clone(), config_override);
        let limits = cel_contracts::RunLimits {
            max_steps: config.max_steps,
            timeout_ms: config.timeout_ms,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: None,
            memory_db_path: None,
        };

        // Canonical path: LLM produces a Plan upfront, CortexStepExecutor
        // dispatches. Same contract as CLI and eval harness.
        let llm_client = match cel_llm::create_client() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                store.update_status(
                    &job_id,
                    JobStatus::Failed,
                    None,
                    Some(format!("LLM not configured: {e}")),
                );
                return;
            }
        };
        let planner = cel_planner::LlmPlanProducer::new(llm_client);
        let executor = cel_goal_runner::CortexStepExecutor::new(cortex);
        let runner = cel_goal_runner::CanonicalGoalRunner::new(planner, executor);
        let outcome = runner.run(&goal, limits).await;

        let result_json = match serde_json::to_value(&outcome) {
            Ok(v) => v,
            Err(e) => {
                store.update_status(
                    &job_id,
                    JobStatus::Failed,
                    None,
                    Some(format!("result serialization failed: {e}")),
                );
                return;
            }
        };

        // Canonical GoalOutcome → worker JobStatus.
        //
        // `Refused` is the agent's "ask the user to clarify" terminal
        // (added with `NextMove::Clarify`). The worker protocol doesn't
        // model a distinct refused status, so it surfaces as a `Failed`
        // job with the clarification question in the error field.
        // Clients that need to distinguish read the structured `result`
        // JSON, which round-trips `GoalOutcome::Refused` verbatim via
        // `serde_json::to_value(&outcome)` a few lines above.
        let (job_status, error) = match &outcome {
            cel_contracts::GoalOutcome::Succeeded { .. } => (JobStatus::Succeeded, None),
            cel_contracts::GoalOutcome::Failed(report) => (
                JobStatus::Failed,
                Some(format!(
                    "{}/{}: {}",
                    report.failing_sub_goal,
                    report.failing_step,
                    report.attempts.last().cloned().unwrap_or_default()
                )),
            ),
            cel_contracts::GoalOutcome::Refused { summary } => (
                JobStatus::Failed,
                Some(format!("clarification needed: {summary}")),
            ),
        };
        store.update_status(&job_id, job_status, Some(result_json), error);
    });
}

/// Build a `GoalConfig` from the submitted goal + optional request-side overrides.
/// The `goal` field from `config_override` is discarded — the top-level `goal`
/// is the authoritative value.
fn merge_goal_config(
    goal: String,
    config_override: Option<serde_json::Value>,
) -> cel_goal_runner::GoalConfig {
    let mut config = match config_override {
        Some(value) => {
            serde_json::from_value::<cel_goal_runner::GoalConfig>(value).unwrap_or_else(|e| {
                tracing::warn!("submitted `config` failed to parse as GoalConfig: {e}");
                cel_goal_runner::GoalConfig::default()
            })
        }
        None => cel_goal_runner::GoalConfig::default(),
    };
    config.goal = goal;
    config
}

/// Typed error returned from handlers — implements `IntoResponse` so axum can render it.
pub struct ErrorReply {
    status: StatusCode,
    body: ErrorResponse,
}

impl ErrorReply {
    fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            body: ErrorResponse {
                error: ErrorDetail {
                    code: code.into(),
                    message: message.into(),
                },
            },
        }
    }
}

impl IntoResponse for ErrorReply {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}
