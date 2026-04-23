//! Canonical goal-runner loop — reactive.
//!
//! One entry point: `CanonicalGoalRunner::run(goal, limits)`. The loop
//! is:
//!
//! ```text
//! history = []
//! shared  = {}
//! loop:
//!   if budget exhausted: return FailureReport
//!   perception = executor.perceive()
//!   screenshot = executor.screenshot_png()
//!   move = planner.decide_next(goal, &history, &shared, &perception, screenshot)
//!   match move:
//!     Done { summary, data }  -> return Succeeded
//!     Fail { reason }         -> return Failed
//!     Batch { purpose, steps }:
//!       for step in steps:
//!         record = executor.execute(step)
//!         history.push(record)
//!         if step.is_terminal_fail: return Failed
//!         if step.is_terminal_done: return Succeeded
//! ```
//!
//! No upfront plan. No pre-committed sub-goal list. Every turn the
//! planner sees what actually happened and picks the next small
//! batch. Failures are recorded in `history` so the planner can
//! reason about them and pivot on the next call — we no longer have
//! a per-step 3-strike retry because retries are now the planner's
//! decision, not the runner's.
//!
//! The runner still does two guardrails so a malfunctioning planner
//! can't spin forever:
//!   1. `max_steps` / `timeout_ms` budgets.
//!   2. Same-action-loop detection: if the planner emits three
//!      identical actions in a row that all fail, we terminate with
//!      a FailureReport.
//!
//! The executor is behind a trait so tests can inject scripted
//! responses. The production impl [`CortexStepExecutor`] dispatches
//! through `cel_cortex::Cortex::execute`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use cel_context::ScreenContext;
use cel_cortex::Cortex;
use cel_planner::{
    AttemptRecord, FailureReport, GoalOutcome, NextMove, PlanProducer, PlannedAction, RunLimits,
    RuntimeCaps, Step, StepResult,
};

use crate::outcome::ActionRecord;

/// Runs one step of an agent plan against the outside world.
#[async_trait]
pub trait StepExecutor: Send + Sync {
    /// Execute a step once. `attempt` is always 1 under the reactive
    /// runner (retries are the planner's decision) but kept on the
    /// signature for backward compatibility with tests.
    async fn execute(&self, step: &Step, attempt: u32) -> StepResult;

    /// Latest perception. Default empty for tests.
    async fn perceive(&self) -> ScreenContext {
        empty_context()
    }

    /// Optional screenshot (PNG) for vision-capable planners.
    async fn screenshot_png(&self) -> Option<Vec<u8>> {
        None
    }

    /// What tools are wired up this turn — the planner uses this to
    /// pick actions that will actually dispatch somewhere. Default:
    /// empty caps. Production impls fill in CDP / native-input state.
    async fn capabilities(&self) -> RuntimeCaps {
        RuntimeCaps::default()
    }
}

fn empty_context() -> ScreenContext {
    ScreenContext {
        app: String::new(),
        window: String::new(),
        elements: vec![],
        network_events: vec![],
        http_events: vec![],
        timestamp_ms: 0,
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: vec![],
        audio: None,
        power: None,
        running_apps: vec![],
        recent_files: vec![],
        transcripts: vec![],
    }
}

pub struct CanonicalGoalRunner<P: PlanProducer, X: StepExecutor> {
    planner: P,
    executor: X,
}

impl<P: PlanProducer, X: StepExecutor> CanonicalGoalRunner<P, X> {
    pub fn new(planner: P, executor: X) -> Self {
        Self { planner, executor }
    }

    /// Run `goal` to completion or structured failure.
    pub async fn run(&self, goal: &str, limits: RunLimits) -> GoalOutcome {
        info!(goal, max_steps = limits.max_steps, "Canonical runner started");
        let start = Instant::now();
        let mut history: Vec<AttemptRecord> = Vec::new();
        let mut shared_memory = serde_json::json!({});
        let mut steps_used: u32 = 0;
        let mut last_batch_purpose = String::from("<goal entry>");
        // Loop-detection: same action fired consecutively with errors.
        let mut consecutive_repeat: u32 = 0;
        let mut last_action_hash: u64 = 0;

        loop {
            if steps_used >= limits.max_steps {
                return budget_exhausted("max_steps", &last_batch_purpose, steps_used);
            }
            let elapsed_ms = start.elapsed().as_millis() as u64;
            if elapsed_ms >= limits.timeout_ms {
                return budget_exhausted("timeout_ms", &last_batch_purpose, steps_used);
            }

            let perception = self.executor.perceive().await;
            let screenshot = self.executor.screenshot_png().await;
            let mut caps = self.executor.capabilities().await;
            caps.steps_used = steps_used;
            caps.max_steps = limits.max_steps;
            debug!(
                steps_used,
                perception_elements = perception.elements.len(),
                screenshot_bytes = screenshot.as_ref().map(|s| s.len()).unwrap_or(0),
                cdp_bound = caps.cdp_bound,
                native_input = caps.native_input,
                history_len = history.len(),
                "Asking planner for next move"
            );

            let next = match self
                .planner
                .decide_next(
                    goal,
                    &history,
                    &shared_memory,
                    &perception,
                    screenshot.as_deref(),
                    &caps,
                )
                .await
            {
                Ok(nm) => nm,
                Err(message) => {
                    tracing::error!(
                        steps_used,
                        error = %message,
                        "Planner decide_next failed"
                    );
                    return GoalOutcome::Failed(FailureReport {
                        failing_sub_goal: last_batch_purpose,
                        failing_step: "<planner decide_next>".into(),
                        attempts: vec![message],
                    });
                }
            };

            match next {
                NextMove::Done {
                    summary,
                    extracted_data,
                } => {
                    // Runtime Done-validation: before returning
                    // success, ask the planner to grade its own claim
                    // against fresh perception + screenshot. The
                    // prompt for the grader is stricter than the
                    // planner's own self-check (required parts of
                    // multi-part goals, rejects partial credit, etc.),
                    // so a Done that slips through the planner rules
                    // still gets caught if the UI doesn't match.
                    //
                    // On reject we don't terminate — we record the
                    // rejection as a failed attempt so the next
                    // decide_next sees it in history and either does
                    // more work or emits Fail with an honest reason.
                    let verdict = self
                        .planner
                        .verify_done(
                            goal,
                            &summary,
                            &shared_memory,
                            &perception,
                            screenshot.as_deref(),
                        )
                        .await;
                    match verdict {
                        Ok(v) if v.verified => {
                            info!(summary = %summary, "Planner signaled Done — verified");
                            if !extracted_data.is_null() {
                                merge_into_shared_memory(
                                    &mut shared_memory,
                                    "final",
                                    extracted_data.clone(),
                                );
                            }
                            return GoalOutcome::Succeeded {
                                summary,
                                extracted_data: shared_memory,
                            };
                        }
                        Ok(v) => {
                            warn!(
                                summary = %summary,
                                reason = %v.reason,
                                "Done rejected — evidence does not support claim"
                            );
                            history.push(AttemptRecord {
                                step_purpose: "verify_done".into(),
                                action: PlannedAction::Done {
                                    summary: summary.clone(),
                                    evidence_ids: vec![],
                                },
                                succeeded: false,
                                error: Some(format!(
                                    "runtime rejected Done: {}. Either gather the missing evidence and emit Done again, or emit Fail honestly.",
                                    v.reason
                                )),
                                data: serde_json::Value::Null,
                            });
                            steps_used += 1;
                            continue;
                        }
                        Err(err) => {
                            // Verification failed to run (LLM down,
                            // parse error, etc.). Fail-open: accept
                            // the Done rather than trapping the agent
                            // behind a broken grader. Log loudly so
                            // this shows up in eval traces.
                            tracing::error!(
                                error = %err,
                                "verify_done call failed — accepting Done on fail-open"
                            );
                            info!(summary = %summary, "Planner signaled Done (verification unavailable)");
                            if !extracted_data.is_null() {
                                merge_into_shared_memory(
                                    &mut shared_memory,
                                    "final",
                                    extracted_data.clone(),
                                );
                            }
                            return GoalOutcome::Succeeded {
                                summary,
                                extracted_data: shared_memory,
                            };
                        }
                    }
                }
                NextMove::Fail { reason } => {
                    warn!(reason = %reason, "Planner signaled Fail");
                    return GoalOutcome::Failed(FailureReport {
                        failing_sub_goal: last_batch_purpose,
                        failing_step: "<planner fail>".into(),
                        attempts: vec![reason],
                    });
                }
                NextMove::Batch { purpose, steps } => {
                    last_batch_purpose = purpose.clone();
                    info!(
                        purpose = %purpose,
                        step_count = steps.len(),
                        "Executing batch"
                    );
                    // Reject any step whose action matches one already
                    // known to fail. Defense in depth against a planner
                    // that ignores the BANNED ACTIONS block — we don't
                    // even dispatch; we record a synthetic failure so
                    // the LLM sees "blocked by runtime" next turn and
                    // has no option but to pivot.
                    //
                    // BUT: only ban actions whose outcome is stable
                    // across contexts. A Key("Down") failing once in
                    // Chrome must still be callable in Numbers — the
                    // keypress's effect depends on the frontmost app,
                    // not on the action JSON alone. If we blanket-ban
                    // context-dependent actions we trap the agent
                    // (exactly what happened in the crypto scenario
                    // when the agent concluded "arrow keys are
                    // banned" and couldn't move to cell D3).
                    let mut steps_iter = steps.into_iter();
                    let mut remaining: Vec<Step> = Vec::new();
                    while let Some(s) = steps_iter.next() {
                        if !should_ban_on_repeat(&s.action) {
                            remaining.push(s);
                            continue;
                        }
                        let sig = hash_action(&s.action);
                        if history
                            .iter()
                            .any(|r| !r.succeeded && hash_action(&r.action) == sig)
                        {
                            warn!(
                                purpose = %s.purpose,
                                "Runtime blocked repeat of previously-failed action"
                            );
                            history.push(AttemptRecord {
                                step_purpose: s.purpose.clone(),
                                action: s.action.clone(),
                                succeeded: false,
                                error: Some(
                                    "runtime refused: this exact action failed earlier \
                                     and is BANNED — pick a different approach"
                                        .into(),
                                ),
                                data: serde_json::Value::Null,
                            });
                            steps_used += 1;
                            // Don't run remaining steps in the batch —
                            // they were planned against the state this
                            // blocked step was supposed to produce.
                            break;
                        } else {
                            remaining.push(s);
                        }
                    }
                    for step in remaining {
                        if steps_used >= limits.max_steps {
                            return budget_exhausted("max_steps", &purpose, steps_used);
                        }
                        steps_used += 1;

                        // Terminal actions embedded in a batch are
                        // honored the same as top-level Done / Fail.
                        // The planner sometimes emits them this way.
                        if let PlannedAction::Done { summary, .. } = &step.action {
                            info!(summary = %summary, "Batch step = Done (terminal)");
                            return GoalOutcome::Succeeded {
                                summary: summary.clone(),
                                extracted_data: shared_memory,
                            };
                        }
                        if let PlannedAction::Fail { reason } = &step.action {
                            warn!(reason = %reason, "Batch step = Fail (terminal)");
                            return GoalOutcome::Failed(FailureReport {
                                failing_sub_goal: purpose.clone(),
                                failing_step: step.purpose.clone(),
                                attempts: vec![reason.clone()],
                            });
                        }

                        let action_hash = hash_action(&step.action);
                        let result = self.executor.execute(&step, 1).await;
                        let (succeeded, error, data) = match result {
                            StepResult::Ok {
                                data,
                                discovered_sub_goal: _,
                            } => (true, None, data),
                            StepResult::Err {
                                message,
                                recoverable,
                            } => (false, Some((message, recoverable)), serde_json::Value::Null),
                        };

                        if succeeded {
                            merge_into_shared_memory(&mut shared_memory, &step.purpose, data.clone());
                            consecutive_repeat = 0;
                        } else if action_hash == last_action_hash {
                            consecutive_repeat += 1;
                        } else {
                            consecutive_repeat = 1;
                        }
                        last_action_hash = action_hash;

                        let error_msg = error.as_ref().map(|(m, _)| m.clone());
                        let non_recoverable = error.as_ref().is_some_and(|(_, r)| !r);

                        history.push(AttemptRecord {
                            step_purpose: step.purpose.clone(),
                            action: step.action.clone(),
                            succeeded,
                            error: error_msg.clone(),
                            data,
                        });

                        if let Some(ref msg) = error_msg {
                            warn!(
                                purpose = %step.purpose,
                                error = %msg,
                                "Step failed"
                            );
                        }

                        if non_recoverable {
                            return GoalOutcome::Failed(FailureReport {
                                failing_sub_goal: purpose,
                                failing_step: step.purpose,
                                attempts: vec![error_msg.unwrap_or_default()],
                            });
                        }

                        if consecutive_repeat >= 3 {
                            return GoalOutcome::Failed(FailureReport {
                                failing_sub_goal: purpose,
                                failing_step: step.purpose,
                                attempts: vec![format!(
                                    "same action failed {} times consecutively; planner did not pivot — giving up",
                                    consecutive_repeat
                                )],
                            });
                        }
                    }
                }
            }
        }
    }
}

fn budget_exhausted(kind: &str, last_purpose: &str, steps_used: u32) -> GoalOutcome {
    GoalOutcome::Failed(FailureReport {
        failing_sub_goal: last_purpose.to_string(),
        failing_step: "<budget>".into(),
        attempts: vec![format!(
            "{kind} budget exhausted after {steps_used} steps"
        )],
    })
}

fn merge_into_shared_memory(memory: &mut serde_json::Value, key: &str, data: serde_json::Value) {
    if data.is_null() {
        return;
    }
    let obj = match memory.as_object_mut() {
        Some(obj) => obj,
        None => {
            *memory = serde_json::json!({});
            memory.as_object_mut().expect("just set to {}")
        }
    };
    obj.insert(key.to_string(), data);
}

fn hash_action(action: &PlannedAction) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    // Serialize the action to its JSON form for a stable identity —
    // two identical-looking actions with different internal ordering
    // still hash the same.
    serde_json::to_string(action)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

/// Should this action be refused on repeat after a prior failure?
///
/// `true` for actions whose identity is self-contained (a specific
/// `ax:` target, a specific URL, a specific shell-style command) —
/// repeating them after failure really is wasted work.
///
/// `false` for actions whose outcome depends on ambient context like
/// which app is frontmost. Keypresses, typed text without a target,
/// and waits fall here: a Key("Down") that failed in Chrome can still
/// be the right move in Numbers. Banning them globally traps the
/// agent (seen in the crypto scenario where the agent Failed because
/// it had concluded arrow keys were off-limits).
fn should_ban_on_repeat(action: &PlannedAction) -> bool {
    match action {
        PlannedAction::Key { .. }
        | PlannedAction::KeyCombo { .. }
        | PlannedAction::Wait { .. } => false,
        PlannedAction::Type { target_id, .. } => target_id.is_some(),
        _ => true,
    }
}

/// Production [`StepExecutor`] backed by a real [`Cortex`].
pub struct CortexStepExecutor {
    cortex: Arc<Cortex>,
    log: Arc<Mutex<Vec<ActionRecord>>>,
    step_counter: Arc<AtomicU32>,
}

impl CortexStepExecutor {
    pub fn new(cortex: Arc<Cortex>) -> Self {
        Self {
            cortex,
            log: Arc::new(Mutex::new(Vec::new())),
            step_counter: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn log_handle(&self) -> Arc<Mutex<Vec<ActionRecord>>> {
        self.log.clone()
    }

    pub fn snapshot_log(&self) -> Vec<ActionRecord> {
        self.log.lock().expect("action log poisoned").clone()
    }
}

#[async_trait]
impl StepExecutor for CortexStepExecutor {
    async fn execute(&self, step: &Step, _attempt: u32) -> StepResult {
        let context = self.perceive().await;
        let started = Instant::now();
        let (result, err_str): (StepResult, Option<String>) =
            match self.cortex.execute(&step.action, &context).await {
                Ok(r) if r.success => (
                    StepResult::Ok {
                        data: r.data.clone().unwrap_or(serde_json::Value::Null),
                        discovered_sub_goal: None,
                    },
                    None,
                ),
                Ok(r) => {
                    let msg = r
                        .error
                        .unwrap_or_else(|| "execute returned success=false".into());
                    (
                        StepResult::Err {
                            message: msg.clone(),
                            recoverable: true,
                        },
                        Some(msg),
                    )
                }
                Err(e) => {
                    let msg = e.to_string();
                    (
                        StepResult::Err {
                            message: msg.clone(),
                            recoverable: !is_unrecoverable(&step.action),
                        },
                        Some(msg),
                    )
                }
            };

        let step_index = self.step_counter.fetch_add(1, Ordering::SeqCst);
        let succeeded = matches!(&result, StepResult::Ok { .. });
        let record = ActionRecord {
            step_index,
            kind: action_kind(&step.action),
            subtype: ax_action_subtype(&step.action),
            target_id: action_target_id(&step.action),
            args: action_args_summary(&step.action),
            planner_confidence: None,
            succeeded,
            verified: succeeded,
            latency_ms: started.elapsed().as_millis() as u64,
            error: err_str,
        };
        if let Ok(mut log) = self.log.lock() {
            log.push(record);
        }

        result
    }

    async fn perceive(&self) -> ScreenContext {
        let model = self.cortex.model();
        let guard = model.read().await;
        guard.current_context.clone()
    }

    async fn screenshot_png(&self) -> Option<Vec<u8>> {
        tokio::task::spawn_blocking(|| {
            let mut capture = cel_display::create_capture();
            capture.init().ok()?;
            let frame = capture.capture_frame().ok()?;
            cel_display::encode_png(&frame).ok()
        })
        .await
        .ok()
        .flatten()
    }

    async fn capabilities(&self) -> RuntimeCaps {
        // Introspect the wired Cortex so the planner knows which
        // tools actually dispatch. Resolving the current CDP url is a
        // ~50ms round-trip; worth it because it lets the planner skip
        // a redundant `navigate` when we're already on the target page.
        let cdp_bound = self.cortex.has_cdp_client();
        let cdp_url = if cdp_bound {
            self.cortex.cdp_current_url().await
        } else {
            None
        };
        RuntimeCaps {
            cdp_bound,
            cdp_browser: if cdp_bound { Some("Google Chrome".into()) } else { None },
            cdp_url,
            native_input: self.cortex.native_input_allowed(),
            steps_used: 0,
            max_steps: 0,
        }
    }
}

fn is_unrecoverable(action: &PlannedAction) -> bool {
    matches!(action, PlannedAction::Done { .. } | PlannedAction::Fail { .. })
}

fn action_kind(action: &PlannedAction) -> String {
    match action {
        PlannedAction::Click { .. } => "click".into(),
        PlannedAction::Type { .. } => "type".into(),
        PlannedAction::Key { .. } => "key".into(),
        PlannedAction::KeyCombo { .. } => "key_combo".into(),
        PlannedAction::SetValue { .. } => "set_value".into(),
        PlannedAction::Scroll { .. } => "scroll".into(),
        PlannedAction::Drag { .. } => "drag".into(),
        PlannedAction::Wait { .. } => "wait".into(),
        PlannedAction::Custom { action, .. } => format!("custom:{action}"),
        PlannedAction::Extract { .. } => "extract".into(),
        PlannedAction::ExtractWithFallback { .. } => "extract_with_fallback".into(),
        PlannedAction::Batch { .. } => "batch".into(),
        PlannedAction::Act { .. } => "act".into(),
        PlannedAction::Done { .. } => "done".into(),
        PlannedAction::Fail { .. } => "fail".into(),
        PlannedAction::AxAction { .. } => "ax_action".into(),
        PlannedAction::ActivateApp { .. } => "activate_app".into(),
        PlannedAction::Select { .. } => "select".into(),
        PlannedAction::CdpEval { .. } => "cdp_eval".into(),
        PlannedAction::Navigate { .. } => "navigate".into(),
        PlannedAction::NotebookWrites { .. } => "notebook_writes".into(),
        PlannedAction::WriteCells { .. } => "write_cells".into(),
    }
}

fn ax_action_subtype(action: &PlannedAction) -> Option<String> {
    match action {
        PlannedAction::AxAction { action, .. } => Some(action.clone()),
        _ => None,
    }
}

fn action_target_id(action: &PlannedAction) -> Option<String> {
    match action {
        PlannedAction::Click { target_id }
        | PlannedAction::SetValue { target_id, .. }
        | PlannedAction::AxAction { target_id, .. } => Some(target_id.clone()),
        PlannedAction::Type { target_id, .. } => target_id.clone(),
        PlannedAction::Drag { from_target_id, .. } => Some(from_target_id.clone()),
        _ => None,
    }
}

fn action_args_summary(action: &PlannedAction) -> Option<String> {
    fn truncate(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            format!("{}…", &s[..max])
        }
    }
    match action {
        PlannedAction::Type { text, .. } => Some(truncate(text, 200)),
        PlannedAction::SetValue { value, .. } => Some(truncate(value, 200)),
        PlannedAction::CdpEval { expression } => Some(truncate(expression, 200)),
        PlannedAction::Navigate { url } => Some(truncate(url, 200)),
        PlannedAction::Key { key } => Some(key.clone()),
        PlannedAction::KeyCombo { keys } => Some(keys.join("+")),
        PlannedAction::Wait { ms } => Some(ms.to_string()),
        PlannedAction::ActivateApp { app_name } => Some(app_name.clone()),
        PlannedAction::Done { summary, .. } => Some(truncate(summary, 200)),
        PlannedAction::Fail { reason } => Some(truncate(reason, 200)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_planner::{Step, StepKind};
    use std::sync::atomic::AtomicU32;

    /// Scripted planner: returns pre-seeded NextMove values in order.
    /// Last element is sticky (used for any call past the end).
    struct ScriptedPlanner {
        moves: std::sync::Mutex<Vec<NextMove>>,
    }

    impl ScriptedPlanner {
        fn new(moves: Vec<NextMove>) -> Self {
            Self {
                moves: std::sync::Mutex::new(moves),
            }
        }
    }

    #[async_trait]
    impl PlanProducer for ScriptedPlanner {
        async fn decide_next(
            &self,
            _goal: &str,
            _history: &[AttemptRecord],
            _shared: &serde_json::Value,
            _perception: &ScreenContext,
            _shot: Option<&[u8]>,
            _caps: &RuntimeCaps,
        ) -> Result<NextMove, String> {
            let mut g = self.moves.lock().unwrap();
            if g.len() > 1 {
                Ok(g.remove(0))
            } else {
                Ok(g.first().cloned().unwrap_or(NextMove::Fail {
                    reason: "script exhausted".into(),
                }))
            }
        }
    }

    /// Scripted executor: step.purpose encodes the script.
    /// * "ok:<value>" succeeds.
    /// * "err:<msg>" fails (recoverable).
    /// * "unrecov:<msg>" fails (non-recoverable).
    struct ScriptedExecutor {
        attempts: AtomicU32,
    }

    impl ScriptedExecutor {
        fn new() -> Self {
            Self {
                attempts: AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl StepExecutor for ScriptedExecutor {
        async fn execute(&self, step: &Step, _attempt: u32) -> StepResult {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let p = &step.purpose;
            if let Some(rest) = p.strip_prefix("ok:") {
                return StepResult::Ok {
                    data: serde_json::json!({ "value": rest }),
                    discovered_sub_goal: None,
                };
            }
            if let Some(rest) = p.strip_prefix("err:") {
                return StepResult::Err {
                    message: rest.into(),
                    recoverable: true,
                };
            }
            if let Some(rest) = p.strip_prefix("unrecov:") {
                return StepResult::Err {
                    message: rest.into(),
                    recoverable: false,
                };
            }
            StepResult::Err {
                message: format!("unknown scripted step: {p}"),
                recoverable: false,
            }
        }
    }

    fn noop() -> PlannedAction {
        PlannedAction::Wait { ms: 0 }
    }

    fn step(purpose: &str) -> Step {
        Step {
            purpose: purpose.into(),
            kind: StepKind::Deterministic,
            action: noop(),
        }
    }

    fn batch(purpose: &str, steps: Vec<Step>) -> NextMove {
        NextMove::Batch {
            purpose: purpose.into(),
            steps,
        }
    }

    #[tokio::test]
    async fn happy_path_batch_then_done() {
        let planner = ScriptedPlanner::new(vec![
            batch("gather", vec![step("ok:a"), step("ok:b")]),
            NextMove::Done {
                summary: "got it".into(),
                extracted_data: serde_json::Value::Null,
            },
        ]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner.run("x", RunLimits::default()).await;
        match outcome {
            GoalOutcome::Succeeded { summary, .. } => assert_eq!(summary, "got it"),
            other => panic!("expected Succeeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn planner_fail_propagates() {
        let planner = ScriptedPlanner::new(vec![NextMove::Fail {
            reason: "impossible".into(),
        }]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner.run("x", RunLimits::default()).await;
        match outcome {
            GoalOutcome::Failed(r) => assert!(r.attempts[0].contains("impossible")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn three_consecutive_identical_failures_auto_fail() {
        // Planner keeps emitting the same failing step — runner breaks
        // the loop after 3 in a row.
        let planner = ScriptedPlanner::new(vec![batch(
            "retry forever",
            vec![step("err:boom"), step("err:boom"), step("err:boom")],
        )]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner.run("x", RunLimits::default()).await;
        match outcome {
            GoalOutcome::Failed(r) => assert!(r.attempts[0].contains("same action failed")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_recoverable_error_fails_immediately() {
        let planner = ScriptedPlanner::new(vec![batch(
            "try",
            vec![step("unrecov:refused"), step("ok:never")],
        )]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner.run("x", RunLimits::default()).await;
        match outcome {
            GoalOutcome::Failed(r) => assert!(r.attempts[0].contains("refused")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn max_steps_budget_respected() {
        // Emit batches forever so the runner runs steps until it hits
        // the budget. Exhaustion returns Failed.
        let planner = ScriptedPlanner::new(vec![batch(
            "loop",
            vec![step("ok:a"), step("ok:b"), step("ok:c")],
        )]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner
            .run(
                "x",
                RunLimits {
                    max_steps: 5,
                    timeout_ms: 30_000,
                    max_step_retries: 3,
                },
            )
            .await;
        match outcome {
            GoalOutcome::Failed(r) => assert!(r.attempts[0].contains("max_steps")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn history_carries_prior_error_across_turns() {
        // This test would need a ScriptedPlanner that inspects history —
        // for now, the scripted one is oblivious. The real LlmPlanProducer
        // is tested integration-style. This placeholder documents intent.
        let planner = ScriptedPlanner::new(vec![
            batch("try", vec![step("err:first fail")]),
            batch("retry differently", vec![step("ok:recovered")]),
            NextMove::Done {
                summary: "ok".into(),
                extracted_data: serde_json::Value::Null,
            },
        ]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner.run("x", RunLimits::default()).await;
        assert!(matches!(outcome, GoalOutcome::Succeeded { .. }));
    }
}
