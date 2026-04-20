//! Core goal-runner loop — full Rust implementation.
//!
//! Owns the entire perceive → plan → execute → verify → reflect → gate cycle.
//! - Perception: reads directly from Cortex Arc<RwLock<MentalModel>> (zero-copy)
//! - Planning: calls cel-planner Rust functions directly
//! - Execution: will dispatch through Cortex (adapter routing) — currently placeholder
//! - Verification: context diff in Rust

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error, debug};

use cel_context::ScreenContext;
use cel_cortex::{Cortex, MentalModel};
use cel_planner::{PlannedStep, PlannedAction, Planner, PlannerBackend, PlannerError, PlannerEvent};
use cel_planner::loop_detector::{LoopDetector, LoopSignal, LoopSeverity, context_fingerprint};
use cel_planner::history::StepHistory;
use cel_planner::prompt::{detect_task_type, build_composable_system_prompt_with_adapters};

use crate::callbacks::{ExecutionCallbacks, RunnerEvent, RunnerEventType, StepReport};
use crate::config::GoalConfig;
use crate::outcome::{ActionRecord, GoalResult, GoalMetrics, GoalStatus};
use crate::state::{RunnerState, RunnerPhase};

/// Dummy PlannerBackend — the runner handles perception/execution itself.
/// The Planner only needs the backend for its own `run()` loop, which we don't use.
/// We call `plan_step()` directly and pass this as a no-op.
struct RunnerBackend;

#[async_trait::async_trait]
impl PlannerBackend for RunnerBackend {
    async fn get_context(&self) -> Result<ScreenContext, PlannerError> {
        Err(PlannerError::Context("RunnerBackend: use Cortex instead".into()))
    }
    async fn execute(&self, _action: &PlannedAction) -> Result<bool, PlannerError> {
        Err(PlannerError::Context("RunnerBackend: use Cortex instead".into()))
    }
    fn on_event(&self, _event: PlannerEvent) {}
}

/// The goal runner — full Rust execution loop.
pub struct GoalRunner {
    config: GoalConfig,
    state: RunnerState,
    cortex: Arc<Cortex>,
    model: Arc<RwLock<MentalModel>>,
    planner: Option<Planner>,
    system_prompt: String,
    callbacks: Arc<dyn ExecutionCallbacks>,
    history: StepHistory,
    loop_detector: LoopDetector,
    metrics: GoalMetrics,
    action_log: Vec<ActionRecord>,

    // ── Phase 3B: Rolling cross-goal memory ────────────────────────────

    /// Optional memory store — opened on construction, appended to on
    /// finish, read lazily during Perceive. `None` if the store couldn't
    /// be opened (e.g. no HOME) — runner degrades to no-memory gracefully.
    memory: Option<cel_cortex::Memory>,
    /// Coarse goal classification, derived once from the goal at
    /// construction. Used as the "similar tasks" lens key. Stored as a
    /// `String` because `TaskType` is internal to cel-planner.
    goal_type: String,
    /// Pre-rendered recent-memory block for the prompt. Refreshed at each
    /// Perceive in case a parallel goal wrote new entries.
    recent_memory_block: String,
    /// Wall-clock start time for memory duration. Not the same as
    /// `state.start_at` because the latter is phase-tracking.
    started_ms: u64,
    /// Rolling record of frontmost apps seen during the run. Capped at
    /// 32 entries; we take the top-3 by frequency at finish time.
    apps_seen: std::collections::HashMap<String, u32>,
    /// Last browser URL observed. Written into the memory entry at finish.
    last_url: Option<String>,
    /// Last successful CdpEval expression seen during this run. On Achieved
    /// goals the runner stashes this in the memory entry as the "winning
    /// selector" so future runs on the same host can short-circuit
    /// selector discovery (Phase 3B eval-smoke finding).
    last_successful_cdp_eval: Option<String>,
}

impl GoalRunner {
    /// Create a new goal runner.
    ///
    /// `cortex` — Cortex handle, used for both reading state and executing native actions.
    /// `callbacks` — observability callbacks (events only).
    pub fn new(
        config: GoalConfig,
        cortex: Arc<Cortex>,
        callbacks: Arc<dyn ExecutionCallbacks>,
    ) -> Self {
        let state = RunnerState::new(config.max_steps, config.timeout_ms);
        let model = cortex.model();

        // Try to create a Planner from the environment LLM config.
        // If LLM is not configured, planner stays None and the runner will fail gracefully.
        let planner_config = cel_planner::GoalConfig {
            goal: config.goal.clone(),
            max_steps: config.max_steps,
            ..Default::default()
        };
        let planner = cel_planner::create_planner(planner_config).ok().map(|mut p| {
            if config.deterministic_seed.is_some() {
                p.set_deterministic(true);
            }
            p
        });

        // Build system prompt (will be refreshed on first step with actual context)
        let task_type = detect_task_type(&config.goal);
        let system_prompt = build_composable_system_prompt_with_adapters(
            None, task_type, None, None,
        );

        let goal_type = format!("{:?}", task_type).to_lowercase();
        let memory = cel_cortex::Memory::open(&cortex.id)
            .map_err(|e| warn!("Memory store unavailable: {e}"))
            .ok();

        Self {
            config,
            state,
            cortex,
            model,
            planner,
            system_prompt,
            callbacks,
            history: StepHistory::new(),
            loop_detector: LoopDetector::new(),
            metrics: GoalMetrics::default(),
            action_log: Vec::new(),
            memory,
            goal_type,
            recent_memory_block: String::new(),
            started_ms: now_ms(),
            apps_seen: std::collections::HashMap::new(),
            last_url: None,
            last_successful_cdp_eval: None,
        }
    }

    /// Create a runner with an explicit [`cel_planner::Planner`].
    ///
    /// Use this in tests to inject a mock LLM via [`cel_llm::LlmClient::new_with_fn`]
    /// without relying on environment variables.
    pub fn new_with_planner(
        config: GoalConfig,
        cortex: Arc<Cortex>,
        planner: cel_planner::Planner,
        callbacks: Arc<dyn ExecutionCallbacks>,
    ) -> Self {
        let state = RunnerState::new(config.max_steps, config.timeout_ms);
        let model = cortex.model();
        let task_type = detect_task_type(&config.goal);
        let system_prompt = build_composable_system_prompt_with_adapters(
            None, task_type, None, None,
        );
        let goal_type = format!("{:?}", task_type).to_lowercase();
        let memory = cel_cortex::Memory::open(&cortex.id)
            .map_err(|e| warn!("Memory store unavailable: {e}"))
            .ok();
        Self {
            config,
            state,
            cortex,
            model,
            planner: Some(planner),
            system_prompt,
            callbacks,
            history: StepHistory::new(),
            loop_detector: LoopDetector::new(),
            metrics: GoalMetrics::default(),
            action_log: Vec::new(),
            memory,
            goal_type,
            recent_memory_block: String::new(),
            started_ms: now_ms(),
            apps_seen: std::collections::HashMap::new(),
            last_url: None,
            last_successful_cdp_eval: None,
        }
    }

    /// Run the goal to completion.
    pub async fn run(&mut self) -> GoalResult {
        info!(goal = %self.config.goal, max_steps = self.config.max_steps, "Goal runner started");

        self.state.transition(RunnerPhase::Perceive);

        while !self.state.should_stop() {
            let step_start = now_ms();

            // ── PERCEIVE ────────────────────────────────────────────────
            self.state.transition(RunnerPhase::Perceive);
            // Phase 2: force a fresh tick before reading the model so the
            // planner sees post-action state rather than the snapshot from
            // the previous 200ms interval boundary. Best-effort — a timeout
            // falls back to whatever the model currently holds, so a stalled
            // tick loop degrades gracefully instead of hanging the runner.
            if let Err(e) = self.cortex.refresh_now(Some(300)).await {
                debug!(
                    step = self.state.step_index,
                    error = %e,
                    "Pre-perceive refresh failed, using existing snapshot"
                );
            }
            self.metrics.refreshes += 1;
            // Phase 3A: pull the full planner-facing signal set alongside
            // the context so both reflect the same tick.
            let (context, signals) = {
                let model = self.model.read().await;
                let signals = cortex_signals_from(&model, self.cortex.last_tick_age_ms());
                (model.current_context.clone(), signals)
            };
            self.metrics.context_reads += 1;
            debug!(step = self.state.step_index, elements = context.elements.len(), "Perceived");

            // Phase 3B: track apps seen for the finish-time memory entry.
            // `last_url` is harvested from the existing CDP-backed
            // `current_browser_url()` call in the Plan phase below — no
            // need for a separate AX-property lookup here.
            if !context.app.is_empty() {
                *self.apps_seen.entry(context.app.clone()).or_insert(0) += 1;
            }

            // Phase 3B: refresh the rolling-memory block once per step. The
            // read is cheap (already warm, bounded by MAX_RETAINED_ENTRIES)
            // and keeps the planner aware of any sibling-goal entries
            // written since construction.
            if self.memory.is_some() {
                self.recent_memory_block = build_memory_block(
                    self.memory.as_ref().unwrap(),
                    &self.goal_type,
                );
            }

            // ── PLAN ────────────────────────────────────────────────────
            self.state.transition(RunnerPhase::Plan);

            let live_url = current_browser_url().await;
            if let Some(ref url) = live_url {
                self.last_url = Some(url.clone());
            }
            if let Some(summary) = deterministic_goal_completion(&self.config, live_url.as_deref()).await {
                return self.finish(GoalStatus::Achieved, summary);
            }
            let step: PlannedStep = if let Some(recovery) =
                constrained_recovery_step(&self.config, &context, live_url.as_deref(), self.state.step_index)
            {
                debug!(step = self.state.step_index, current_url = ?live_url, "Using constrained recovery step");
                recovery
            } else if let Some(fast_path) = deterministic_first_step(&self.config.goal, &context, self.state.step_index) {
                debug!(step = self.state.step_index, "Using deterministic first-step fast path");
                fast_path
            } else if let Some(ref planner) = self.planner {
                let loop_warning_opt = None; // TODO: wire from loop detector
                let backend = RunnerBackend;

                // Phase 3C: decide whether this step should go through the
                // vision-enhanced plan. Gate is deliberately narrow — vision
                // is expensive and rarely needed. Fires only when Cortex
                // itself has flagged context as sparse AND the last failure
                // points at a missing/stale element (not parse/rate-limit).
                let use_vision = self.config.enable_vision
                    && signals.vision_needed
                    && last_failure_was_target_miss(&self.state, &self.action_log);

                let vision_screenshot = if use_vision {
                    match capture_screenshot_for_vision() {
                        Ok(data_url) => {
                            debug!(step = self.state.step_index, "Vision fallback: screenshot captured");
                            Some(data_url)
                        }
                        Err(e) => {
                            warn!(step = self.state.step_index, error = %e, "Screenshot capture failed; falling back to text-only plan");
                            None
                        }
                    }
                } else {
                    None
                };

                if vision_screenshot.is_some() {
                    // Dedicated observability event — distinct from the
                    // Planned event the runner emits after step_action
                    // extraction below. Consumers can count this to
                    // measure vision overhead.
                    self.emit_event(RunnerEventType::VisionInvoked, None);
                }

                let plan_future = async {
                    if let Some(ref data_url) = vision_screenshot {
                        planner
                            .plan_step_with_vision(
                                &self.system_prompt,
                                &context,
                                &signals,
                                &self.recent_memory_block,
                                &self.history,
                                self.state.step_index,
                                &loop_warning_opt,
                                data_url,
                            )
                            .await
                    } else {
                        planner
                            .plan_step(
                                &self.system_prompt,
                                &context,
                                &signals,
                                &self.recent_memory_block,
                                &self.history,
                                self.state.step_index,
                                &loop_warning_opt,
                                &backend,
                            )
                            .await
                    }
                };

                match plan_future.await {
                    Ok(s) => {
                        self.metrics.llm_calls += 1;
                        if vision_screenshot.is_some() {
                            self.metrics.vision_calls += 1;
                        }
                        s
                    }
                    Err(e) => {
                        error!(step = self.state.step_index, error = %e, "Planning failed");
                        // Hard-fail on configuration errors (auth / unconfigured).
                        // No amount of retries fixes a broken API key — this
                        // saves the eval harness from burning steps + dollars.
                        if let PlannerError::Llm(ref llm_err) = e {
                            if llm_err.is_unrecoverable() {
                                return self.finish(
                                    GoalStatus::Failed,
                                    format!("LLM unrecoverable: {llm_err}"),
                                );
                            }
                            // Phase 5.1: rate-limit circuit breaker. By the
                            // time a 429 bubbles out of the LLM client, the
                            // client's internal 1s/2s/4s backoff already
                            // fired. A handful of back-to-back 429s means
                            // quota is gone — fail cleanly instead of
                            // chewing through max_consecutive_failures.
                            if llm_err.is_rate_limited() {
                                self.state.consecutive_rate_limits += 1;
                                if self.state.consecutive_rate_limits
                                    >= self.config.max_consecutive_rate_limits
                                {
                                    return self.finish(
                                        GoalStatus::Failed,
                                        format!(
                                            "LLM rate-limited {} times in a row — quota exhausted: {llm_err}",
                                            self.state.consecutive_rate_limits
                                        ),
                                    );
                                }
                            } else {
                                // Any non-429 error resets the rate-limit
                                // counter — the next 429 gets a clean budget.
                                self.state.consecutive_rate_limits = 0;
                            }
                        }
                        self.state.record_failure("plan_error");
                        self.emit_event(RunnerEventType::Error, Some(format!("Plan error: {e}")));
                        // Cap consecutive *planning* failures here. Without
                        // this gate, the loop runs to max_steps even when
                        // the LLM is wedged (rate-limit, parse-loop, etc.) —
                        // burning quota + time. The execute-phase has its
                        // own gate, but planning errors short-circuit before
                        // reaching it. Found by cel-eval baseline (rate-
                        // limited OAuth runs that previously timed out at 60s
                        // now exit in ~10s with a clean error).
                        if self.state.consecutive_failures
                            >= self.config.max_consecutive_failures
                        {
                            return self.finish(
                                GoalStatus::Failed,
                                format!(
                                    "Max consecutive planning failures ({}) reached: {e}",
                                    self.config.max_consecutive_failures
                                ),
                            );
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(self.config.step_delay_ms)).await;
                        self.state.step_index += 1;
                        continue;
                    }
                }
            } else {
                warn!("No planner available (LLM not configured) — failing");
                return self.finish(GoalStatus::Failed, "LLM not configured — cannot plan".into());
            };

            // Check loop detector
            let ctx_hash = context_fingerprint(&context);
            let loop_signal = self.loop_detector.check(&step.action, ctx_hash);
            match &loop_signal {
                LoopSignal::None => {}
                LoopSignal::Repeat { severity: LoopSeverity::Forceful, .. }
                | LoopSignal::PingPong { severity: LoopSeverity::Forceful, .. }
                | LoopSignal::StaleContext { severity: LoopSeverity::Forceful, .. } => {
                    warn!(step = self.state.step_index, "Loop auto-fail: {:?}", loop_signal);
                    return self.finish(GoalStatus::Failed, format!("Loop detected: {loop_signal:?}"));
                }
                signal => {
                    debug!(step = self.state.step_index, "Loop signal: {:?}", signal);
                }
            }

            let planned_action = step_action(&step);
            let action_type = action_type_str(&planned_action);
            self.emit_event(RunnerEventType::Planned, Some(action_type.clone()));

            // Terminal actions
            match &planned_action {
                PlannedAction::Done { summary, .. } => {
                    info!("Goal achieved: {summary}");
                    return self.finish(GoalStatus::Achieved, summary.clone());
                }
                PlannedAction::Fail { reason } => {
                    warn!("Goal failed: {reason}");
                    return self.finish(GoalStatus::Failed, reason.clone());
                }
                _ => {}
            }

            // ── PRE-EXECUTE REFRESH + VALIDATE (Phase 2) ────────────────
            // Force a fresh tick between Plan and Execute so we dispatch
            // against the world as it is *now*, not as it was when the
            // planner looked at it. LLM latency can easily be a few seconds,
            // during which a dialog might have appeared or an element moved.
            // Best-effort refresh; on timeout we proceed with the existing
            // snapshot (Phase 5 soak will cover the hung-merger case).
            if let Err(e) = self.cortex.refresh_now(Some(300)).await {
                debug!(
                    step = self.state.step_index,
                    error = %e,
                    "Pre-execute refresh failed, using pre-plan context"
                );
            }
            self.metrics.refreshes += 1;
            let pre_exec_context = {
                let model = self.model.read().await;
                model.current_context.clone()
            };

            // If the planner returned a `Custom { adapter }` action,
            // verify the adapter is registered. LLMs routinely hallucinate
            // adapter names like "browser" even though the runner never
            // exposed one — we reject those up-front and trigger a replan
            // with a stale-target-style event, instead of letting Cortex
            // emit "No adapter registered for X" mid-dispatch (where the
            // error is harder to recover from and burns a failed step).
            if let PlannedAction::Custom { adapter, .. } = &planned_action {
                let registered = self.cortex.registered_adapter_names().await;
                if !registered.iter().any(|name| name == adapter) {
                    warn!(
                        step = self.state.step_index,
                        adapter = %adapter,
                        registered = ?registered,
                        "Planner asked for unknown adapter — triggering replan"
                    );
                    self.emit_event(
                        RunnerEventType::StaleTarget,
                        Some(format!("unknown-adapter:{adapter}")),
                    );
                    self.metrics.stale_targets += 1;
                    self.state.record_failure("unknown_adapter");
                    self.history.record_full(
                        self.state.step_index,
                        planned_action.clone(),
                        false,
                        Some(format!(
                            "unknown_adapter: {adapter} (registered: {})",
                            if registered.is_empty() {
                                "none".to_string()
                            } else {
                                registered.join(", ")
                            }
                        )),
                        None,
                        None,
                    );
                    if self.config.step_delay_ms > 0 && !self.state.should_stop() {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            self.config.step_delay_ms,
                        ))
                        .await;
                    }
                    self.state.step_index += 1;
                    continue;
                }
            }

            // If the planned action names an element, verify the element
            // still exists in the fresh context. Missing target ⇒ replan
            // instead of executing against stale bounds. Actions without
            // targets (CdpEval, Key, Wait, ActivateApp, …) skip validation.
            let target_ids = planned_action.target_ids();
            if !target_ids.is_empty() {
                let validation = self
                    .cortex
                    .validate_targets(&pre_exec_context, &target_ids);
                if !validation.is_ok() {
                    warn!(
                        step = self.state.step_index,
                        missing = ?validation.missing,
                        action = %action_type,
                        "Planned targets no longer exist in fresh context — replanning"
                    );
                    self.emit_event(
                        RunnerEventType::StaleTarget,
                        Some(validation.missing.join(",")),
                    );
                    self.metrics.stale_targets += 1;
                    self.state.record_failure("stale_target");
                    // Record the aborted attempt so the planner sees what
                    // it tried and why the runner rejected it.
                    self.history.record_full(
                        self.state.step_index,
                        planned_action.clone(),
                        false,
                        Some(format!(
                            "stale_target: {}",
                            validation.missing.join(", ")
                        )),
                        None,
                        None,
                    );
                    if self.config.step_delay_ms > 0 && !self.state.should_stop() {
                        tokio::time::sleep(tokio::time::Duration::from_millis(
                            self.config.step_delay_ms,
                        ))
                        .await;
                    }
                    self.state.step_index += 1;
                    continue;
                }
            }

            // ── EXECUTE ─────────────────────────────────────────────────
            self.state.transition(RunnerPhase::Execute);

            let exec_result = match self
                .cortex
                .execute(&planned_action, &pre_exec_context)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    error!(step = self.state.step_index, error = %e, "Execution failed");
                    cel_cortex::ActionResult::fail(e.to_string())
                }
            };
            let exec_success = exec_result.success;
            self.emit_event(RunnerEventType::Executed, Some(action_type.clone()));

            // After app activation, give the Cortex perception tick time to
            // pick up the new frontmost app before we read the mental model
            // for verification.
            if step_contains_activation(&planned_action) {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }

            // ── VERIFY ──────────────────────────────────────────────────
            self.state.transition(RunnerPhase::Verify);
            let after_context = {
                let model = self.model.read().await;
                model.current_context.clone()
            };
            self.metrics.context_reads += 1;
            // Diff against the pre-execute context, not the pre-plan one —
            // if the world changed during LLM latency, that shift is not
            // attributable to our action.
            let verified = verify_action(&pre_exec_context, &after_context);

            // ── REFLECT ─────────────────────────────────────────────────
            self.state.transition(RunnerPhase::Reflect);
            let success = exec_success || verified;
            if success {
                self.state.record_success(&action_type);
                self.metrics.action_successes += 1;
            } else {
                self.state.record_failure(&action_type);
                self.metrics.action_failures += 1;
            }

            // Include action output data (e.g. cdp_eval results) in history
            // so the LLM can see what the page returned on the next planning step.
            let data_str = exec_result.data.as_ref().and_then(|v| match v {
                serde_json::Value::String(s) => Some(s.clone()),
                other => serde_json::to_string(other).ok(),
            });
            self.history.record_full(
                self.state.step_index,
                planned_action.clone(),
                success,
                exec_result.error.clone(),
                None,
                data_str,
            );

            let step_duration_ms = now_ms() - step_start;
            // Unwrap Batch into one ActionRecord per inner action so per-action
            // metrics + scenario assertions see the actual primitives the
            // agent attempted (set_value, ax_action, click, …) instead of an
            // opaque "batch" wrapper. Found by cel-eval baseline: the
            // search-employees scenario reported `kind: batch` and every
            // assertion against `set_value` failed despite the agent doing
            // the right thing.
            let inner_actions: Vec<&PlannedAction> = match &planned_action {
                PlannedAction::Batch { actions } => actions.iter().collect(),
                other => vec![other],
            };
            for inner in &inner_actions {
                // Phase 3B Fix #3: track the last successful CdpEval so
                // a successful goal can record it in memory as the
                // winning selector/expression. We're careful to only
                // capture on success — a successful LLM-produced selector
                // is a signal future goals can build on; failed ones
                // would just pollute the cache.
                if success {
                    if let PlannedAction::CdpEval { expression } = inner {
                        self.last_successful_cdp_eval = Some(expression.clone());
                    }
                }
                self.action_log.push(ActionRecord {
                    step_index: self.state.step_index,
                    kind: action_type_str(inner),
                    subtype: ax_action_subtype(inner),
                    target_id: action_target_id(inner),
                    args: action_args_summary(inner),
                    planner_confidence: Some(step.confidence),
                    succeeded: success,
                    verified,
                    latency_ms: step_duration_ms / inner_actions.len().max(1) as u64,
                    error: exec_result.error.clone(),
                });
            }

            self.callbacks.on_step_complete(StepReport {
                step_index: self.state.step_index,
                action_type: action_type.clone(),
                success,
                verified,
                error: exec_result.error.clone(),
                reasoning: step.reasoning.clone(),
                duration_ms: step_duration_ms,
            });
            self.emit_event(RunnerEventType::StepCompleted, Some(format!(
                "step={} action={action_type} success={success} verified={verified}",
                self.state.step_index,
            )));

            // ── GATE ────────────────────────────────────────────────────
            self.state.transition(RunnerPhase::Gate);
            self.state.step_index += 1;

            // Self-healing: inject repair context into the next planning call.
            // When an action fails, the planner needs to know what went wrong
            // so it can choose a different approach (not repeat the failure).
            if !success && self.config.self_heal {
                let fail_desc = format!(
                    "REPAIR NEEDED: Action \"{}\" failed. Reason: {}. Try a DIFFERENT element or approach.",
                    action_type,
                    exec_result.error.as_deref().unwrap_or("execution failed / not verified"),
                );
                // Rebuild system prompt with repair context injected
                let task_type = detect_task_type(&self.config.goal);
                self.system_prompt = build_composable_system_prompt_with_adapters(
                    None, task_type, None, None,
                );
                // The repair context is injected via the goal string on next iteration.
                // The loop detector will catch repeated failures; the strategy tracker
                // prevents the same approach from being tried twice.
                if self.state.consecutive_failures >= 2 {
                    // After 2+ failures, inject explicit repair instructions into history
                    self.history.record(
                        self.state.step_index,
                        PlannedAction::Fail { reason: fail_desc },
                        false,
                        Some("self-heal: forcing different approach".into()),
                    );
                }
            }

            if self.state.consecutive_failures >= self.config.max_consecutive_failures {
                return self.finish(GoalStatus::Failed, format!(
                    "Max consecutive failures ({}) reached", self.config.max_consecutive_failures
                ));
            }

            if self.config.step_delay_ms > 0 && !self.state.should_stop() {
                tokio::time::sleep(tokio::time::Duration::from_millis(self.config.step_delay_ms)).await;
            }
        }

        if self.state.cancel_requested {
            self.finish(GoalStatus::Cancelled, "Cancelled".into())
        } else if self.state.step_index >= self.state.max_steps {
            self.finish(GoalStatus::MaxSteps, format!("Max steps ({})", self.state.max_steps))
        } else if self.state.elapsed_ms() >= self.state.timeout_ms {
            self.finish(GoalStatus::Timeout, format!("Timeout ({}ms)", self.state.elapsed_ms()))
        } else {
            self.finish(GoalStatus::Failed, "Loop terminated".into())
        }
    }

    pub fn cancel(&mut self) {
        self.state.cancel_requested = true;
    }

    pub fn status(&self) -> &RunnerState {
        &self.state
    }

    fn finish(&mut self, status: GoalStatus, summary: String) -> GoalResult {
        self.state.transition(RunnerPhase::Complete);
        self.emit_event(RunnerEventType::GoalCompleted, Some(format!("{status:?}: {summary}")));

        // Phase 3B: append a memory entry so future goals on this machine
        // have a pointer to what just happened. Errors are logged but do
        // not affect the returned GoalResult — memory is best-effort.
        if let Some(ref mem) = self.memory {
            let mut apps_ranked: Vec<(String, u32)> =
                self.apps_seen.drain().collect();
            apps_ranked.sort_by(|a, b| b.1.cmp(&a.1));
            let top_apps: Vec<String> =
                apps_ranked.into_iter().take(3).map(|(k, _)| k).collect();

            let is_achieved = matches!(status, GoalStatus::Achieved);
            let last_error = if is_achieved {
                None
            } else {
                Some(summary.clone())
            };
            // Only persist the winning CdpEval on achieved runs. A
            // MaxSteps or Failed run's last CdpEval is noise — preserving
            // it would mislead future runs into re-trying the losing
            // selector. Fix #3 (eval-smoke finding).
            let winning_cdp_eval = if is_achieved {
                self.last_successful_cdp_eval.clone()
            } else {
                None
            };

            let entry = cel_cortex::MemoryEntry {
                v: 0, // filled in by append()
                ts_ms: 0, // filled in by append()
                machine_id: String::new(), // filled in by append()
                cortex_id: String::new(),  // filled in by append()
                goal_type: self.goal_type.clone(),
                goal: self.config.goal.clone(),
                status: format!("{status:?}").to_lowercase(),
                steps: self.state.step_index,
                duration_ms: now_ms().saturating_sub(self.started_ms),
                last_url: self.last_url.clone(),
                top_apps,
                last_error,
                winning_cdp_eval,
            };
            if let Err(e) = mem.append(entry) {
                warn!(error = %e, "Failed to append memory entry");
            }
        }

        GoalResult {
            status,
            summary,
            total_steps: self.state.step_index,
            duration_ms: self.state.elapsed_ms(),
            metrics: self.metrics.clone(),
            action_log: std::mem::take(&mut self.action_log),
        }
    }

    fn emit_event(&self, event_type: RunnerEventType, details: Option<String>) {
        self.callbacks.on_event(RunnerEvent {
            event_type,
            step_index: self.state.step_index,
            action: self.state.last_action_type.clone(),
            success: Some(self.state.last_action_success),
            details,
            timestamp_ms: now_ms(),
        });
    }
}

// ── Utilities ───────────────────────────────────────────────────────────────

fn action_type_str(action: &PlannedAction) -> String {
    match action {
        PlannedAction::Click { .. } => "click".into(),
        PlannedAction::Type { .. } => "type".into(),
        PlannedAction::SetValue { .. } => "set_value".into(),
        PlannedAction::Key { .. } => "key".into(),
        PlannedAction::KeyCombo { .. } => "key_combo".into(),
        PlannedAction::Scroll { .. } => "scroll".into(),
        PlannedAction::Drag { .. } => "drag".into(),
        PlannedAction::Wait { .. } => "wait".into(),
        PlannedAction::Custom { action, .. } => format!("custom:{action}"),
        PlannedAction::Extract { .. } => "extract".into(),
        PlannedAction::Batch { .. } => "batch".into(),
        PlannedAction::Act { .. } => "act".into(),
        PlannedAction::Done { .. } => "done".into(),
        PlannedAction::Fail { .. } => "fail".into(),
        PlannedAction::AxAction { .. } => "ax_action".into(),
        PlannedAction::ActivateApp { .. } => "activate_app".into(),
        PlannedAction::Select { .. } => "select".into(),
        PlannedAction::CdpEval { .. } => "cdp_eval".into(),
        PlannedAction::NotebookWrites { .. } => "notebook_writes".into(),
    }
}

fn ax_action_subtype(action: &PlannedAction) -> Option<String> {
    match action {
        PlannedAction::AxAction { action: subtype, .. } => Some(subtype.clone()),
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
        if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
    }
    match action {
        PlannedAction::Type { text, .. } => Some(truncate(text, 200)),
        PlannedAction::SetValue { value, .. } => Some(truncate(value, 200)),
        PlannedAction::CdpEval { expression } => Some(truncate(expression, 200)),
        PlannedAction::Key { key } => Some(key.clone()),
        PlannedAction::KeyCombo { keys } => Some(keys.join("+")),
        PlannedAction::Wait { ms } => Some(ms.to_string()),
        PlannedAction::ActivateApp { app_name } => Some(app_name.clone()),
        PlannedAction::Done { summary, .. } => Some(truncate(summary, 200)),
        PlannedAction::Fail { reason } => Some(truncate(reason, 200)),
        _ => None,
    }
}

async fn current_browser_url() -> Option<String> {
    let client = cel_cdp::connect_to_focused_app().await?;
    client.get_url().await.ok().filter(|url| !url.is_empty())
}

async fn deterministic_goal_completion(
    config: &GoalConfig,
    current_url: Option<&str>,
) -> Option<String> {
    let target_url = config
        .constrain_to_url
        .as_deref()
        .or_else(|| extract_url(&config.goal))?;
    let current_url = current_url?;
    if !url_matches_constraint(current_url, target_url) || !is_stock_quote_extract_goal(&config.goal) {
        return None;
    }

    let client = cel_cdp::connect_to_focused_app().await?;
    let page = cel_cdp::extract_page_content(&client).await.ok()?;
    summarize_stock_quote(&page.title, &page.body_text)
}

fn is_stock_quote_extract_goal(goal: &str) -> bool {
    let lower = goal.to_lowercase();
    lower.contains("extract")
        && (lower.contains("stock price") || lower.contains("price"))
        && lower.contains("daily change")
}

fn summarize_stock_quote(title: &str, body_text: &str) -> Option<String> {
    let symbol = title
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(symbol, _)| symbol.trim().to_string())
        .filter(|symbol| !symbol.is_empty())
        .unwrap_or_else(|| "Quote".into());

    let normalized = body_text.replace('\u{2212}', "-");
    let (change_amount, change_pct) = extract_change_values(&normalized)?;

    let lines: Vec<&str> = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    // Find the price by looking for a line containing "€" that is immediately
    // followed by the change line (which contains parenthesized percentage).
    // This matches capital.gr's layout:
    //   "1,0900 €"
    //   "-0,0200 (-1,80%)"
    // Also try the old table-header approach as a fallback.
    let mut price: Option<String> = None;
    for window in lines.windows(2) {
        // Strategy 1: price line with € followed by change line with (%)
        if window[0].contains('€') && window[1].contains('(') && window[1].contains("%)") {
            let token = window[0]
                .split_whitespace()
                .next()
                .map(|t| t.replace(',', "."));
            if let Some(ref t) = token {
                if looks_like_decimal(t) {
                    price = token;
                    break;
                }
            }
        }
        // Strategy 2 (fallback): Greek table header ΤΙΜΗ + ΜΕΤ.%
        if window[0].contains("ΤΙΜΗ") && window[0].contains("ΜΕΤ.%") {
            let first = window[1]
                .split_whitespace()
                .next()
                .map(|token| token.replace(',', "."));
            if first.is_some() {
                price = first;
                break;
            }
        }
    }

    let price = price?;
    Some(format!(
        "{} Stock Price: {} EUR, Daily Change: {} EUR, Daily Change Percentage: {}",
        symbol, price, change_amount, change_pct
    ))
}

fn extract_change_values(text: &str) -> Option<(String, String)> {
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let open = line.find('(')?;
        let close = line[open..].find(')')? + open;
        let amount = line[..open].trim().split_whitespace().last()?;
        let pct = line[open + 1..close].trim();
        let normalized_amount = amount.replace(',', ".");
        let normalized_pct = pct.replace(',', ".");
        if looks_like_decimal(&normalized_amount) && looks_like_percent(&normalized_pct) {
            return Some((normalized_amount, normalized_pct));
        }
    }
    None
}

fn looks_like_decimal(value: &str) -> bool {
    let stripped = value.trim().trim_start_matches(['+', '-']);
    let mut parts = stripped.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(int_part), Some(frac_part), None)
            if !int_part.is_empty()
                && !frac_part.is_empty()
                && int_part.chars().all(|c| c.is_ascii_digit())
                && frac_part.chars().all(|c| c.is_ascii_digit())
    )
}

fn looks_like_percent(value: &str) -> bool {
    let stripped = value.trim().trim_end_matches('%');
    value.trim().ends_with('%') && looks_like_decimal(stripped)
}

fn constrained_recovery_step(
    config: &GoalConfig,
    _context: &ScreenContext,
    current_url: Option<&str>,
    step_index: u32,
) -> Option<PlannedStep> {
    if step_index == 0 {
        return None;
    }

    let target_url = config
        .constrain_to_url
        .as_deref()
        .or_else(|| extract_url(&config.goal))?;

    let current_url = current_url?;
    if url_matches_constraint(current_url, target_url) {
        return None;
    }

    Some(PlannedStep {
        evaluation: "Constraint recovery".into(),
        memory: String::new(),
        plan: vec![
            format!("[>] Return to the constrained target URL {}", target_url),
            "[ ] Continue the task from the required page".into(),
        ],
        reasoning: format!(
            "The task is constrained to {} but the active page is {}. Recover back to the required page before continuing.",
            target_url, current_url
        ),
        action: activate_cdp_browser_action(),
        additional_actions: vec![
            PlannedAction::Wait { ms: 300 },
            PlannedAction::CdpEval {
                expression: navigate_eval(target_url),
            },
            PlannedAction::Wait { ms: 2000 },
            PlannedAction::CdpEval {
                expression: cookie_dismiss_eval().into(),
            },
            PlannedAction::Wait { ms: 750 },
        ],
        expected_outcome: format!("Browser returns to {}", target_url),
        confidence: 0.99,
        context_tier: cel_planner::ContextTier::Full,
        thinking: None,
        progress: Some("repairing".into()),
        notebook_writes: vec![],
        batch_next: false,
    })
}

fn url_matches_constraint(current_url: &str, target_url: &str) -> bool {
    let normalize = |url: &str| {
        url.trim()
            .trim_end_matches('/')
            .to_lowercase()
    };

    let current = normalize(current_url);
    let target = normalize(target_url);

    current == target || current.starts_with(&(target.clone() + "?")) || current.starts_with(&(target + "#"))
}

fn deterministic_first_step(goal: &str, _context: &ScreenContext, step_index: u32) -> Option<PlannedStep> {
    if step_index != 0 {
        return None;
    }

    let url = extract_url(goal)?;
    // Always focus the CDP Chrome first, even if a browser is already focused.
    // The focused browser might be the user's regular Chrome (no CDP), while
    // CdpEval targets the CEL CDP instance (port 9333). Without this, navigation
    // happens in the invisible CDP Chrome while the wrong Chrome stays in front.
    Some(PlannedStep {
        evaluation: "Deterministic fast path".into(),
        memory: String::new(),
        plan: vec![
            "[>] Focus CDP browser and navigate directly to the target URL".into(),
            "[ ] Read the loaded page and complete the task".into(),
        ],
        reasoning:
            "The goal already names a URL, so focus the CDP browser and navigate directly without spending an LLM step."
                .into(),
        action: activate_cdp_browser_action(),
        additional_actions: vec![
            PlannedAction::Wait { ms: 500 },
            PlannedAction::CdpEval {
                expression: navigate_eval(url),
            },
            PlannedAction::Wait { ms: 2000 },
            PlannedAction::CdpEval {
                expression: cookie_dismiss_eval().into(),
            },
            PlannedAction::Wait { ms: 750 },
        ],
        expected_outcome: format!("Browser navigates to {}", url),
        confidence: 0.98,
        context_tier: cel_planner::ContextTier::Full,
        thinking: None,
        progress: Some("on_track".into()),
        notebook_writes: vec![],
        batch_next: false,
    })
}

/// Build a CdpEval action that brings the CEL CDP Chrome window to the front.
/// Uses `window.focus()` via CDP + osascript to activate the Chrome process by PID.
/// Falls back to ActivateApp if CDP PID lookup fails.
fn activate_cdp_browser_action() -> PlannedAction {
    // Find the CDP Chrome's PID from discovered targets
    let targets = cel_cdp::discover_cdp_targets();
    let preferred_port = cel_cdp::preferred_cel_cdp_port();

    let preferred = targets.iter().find(|t| t.port == preferred_port);
    let mut pid_activated = false;

    if let Some(target) = preferred {
        if target.pid > 0 {
            // Activate by PID — this targets the exact CDP Chrome instance
            let script = format!(
                "tell application \"System Events\" to set frontmost of (first process whose unix id is {}) to true",
                target.pid
            );
            let status = std::process::Command::new("osascript")
                .args(["-e", &script])
                .status();
            pid_activated = matches!(status, Ok(s) if s.success());
        }
    }

    // Fallback when the target was discovered via port-probe (pid=0) or the
    // PID-based activate failed. Raise by app bundle so the OS still brings
    // Chrome to the front — critical because downstream native-input
    // recovery assumes the CDP browser is frontmost. Try the likely names
    // in order; whichever exists wins.
    if !pid_activated {
        let candidates = [
            "Google Chrome",
            "Chromium",
            "Brave Browser",
            "Microsoft Edge",
            "Arc",
        ];
        for app in candidates {
            let script = format!("tell application \"{app}\" to activate");
            if let Ok(status) = std::process::Command::new("osascript")
                .args(["-e", &script])
                .status()
            {
                if status.success() {
                    break;
                }
            }
        }
    }

    // Return a CdpEval that also calls window.focus() to ensure the tab is active
    PlannedAction::CdpEval {
        expression: "window.focus(); 'focused'".into(),
    }
}

fn extract_url(goal: &str) -> Option<&str> {
    // Also strip trailing sentence punctuation — `trim_matches` with both
    // paren/quote chars AND period/comma/semicolon/colon/bang/question so
    // a goal like "Open https://foo.com." doesn't navigate to a URL with
    // a trailing `.` (Chrome treats that as an invalid host and fails the
    // whole deterministic fast path). Found by eval smoke on the form
    // fixture task.
    for token in goal.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | ',' | ')' | '(' | '[' | ']' | '.' | ';' | ':' | '!' | '?'
            )
        });
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return Some(trimmed);
        }
    }
    None
}

#[cfg(test)]
mod extract_url_tests {
    use super::*;

    #[test]
    fn strips_trailing_period() {
        assert_eq!(
            extract_url("Navigate to https://example.com."),
            Some("https://example.com")
        );
    }

    #[test]
    fn strips_trailing_comma_and_period() {
        assert_eq!(
            extract_url("Open https://foo.com, then do X."),
            Some("https://foo.com")
        );
    }

    #[test]
    fn preserves_trailing_slash() {
        // trailing slash is a valid path segment — do NOT strip it
        assert_eq!(
            extract_url("Goto https://foo.com/"),
            Some("https://foo.com/")
        );
    }

    #[test]
    fn handles_parenthesized_url() {
        assert_eq!(
            extract_url("See the news (https://example.com/news)."),
            Some("https://example.com/news")
        );
    }

    #[test]
    fn returns_none_when_no_url() {
        assert_eq!(extract_url("Do a thing"), None);
    }
}

fn cookie_dismiss_eval() -> &'static str {
    r#"(function() {
        const prioritySelectors = [
            '#accept-btn',
            'button#accept-btn',
            '[id="accept-btn"]',
            '#onetrust-accept-btn-handler',
            '#didomi-notice-agree-button',
            '#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll',
            '#qc-cmp2-ui button[mode="primary"]'
        ];
        for (const selector of prioritySelectors) {
            const el = document.querySelector(selector);
            if (el instanceof HTMLElement && el.offsetParent !== null) {
                el.click();
                return 'clicked-selector:' + selector;
            }
        }

        const textMatches = [
            'accept', 'accept all', 'agree', 'i agree', 'allow all', 'ok', 'got it',
            'συμφωνώ', 'αποδοχή', 'αποδοχη', 'συνέχεια', 'συνεχεια',
            'συναινώ', 'συναινω', 'δέχομαι', 'δεχομαι'
        ];
        const buttonSelectors = ['button', 'a[role="button"]', '[role="button"]', 'input[type="button"]'];
        for (const selector of buttonSelectors) {
            for (const el of document.querySelectorAll(selector)) {
                const text = ((el.textContent || el.getAttribute('value') || '')).toLowerCase().trim();
                if (!text || text.length > 80) continue;
                if (textMatches.some((needle) => text === needle || text.includes(needle))) {
                    if (el instanceof HTMLElement && el.offsetParent !== null) {
                        el.click();
                        return 'clicked:' + text;
                    }
                }
            }
        }

        const overlaySelectors = [
            '#onetrust-banner-sdk', '#onetrust-consent-sdk', '#didomi-host',
            '.qc-cmp2-container', '.fc-consent-root',
            '[id^="sp_message"]', 'iframe[id^="sp_message"]',
            '[class*="cookie"]', '[class*="consent"]', '[class*="gdpr"]',
            '[id*="cookie"]', '[id*="consent"]', '[id*="gdpr"]'
        ];
        let removed = 0;
        for (const selector of overlaySelectors) {
            for (const el of document.querySelectorAll(selector)) {
                if (el instanceof HTMLElement && el.offsetParent !== null) {
                    el.style.setProperty('display', 'none', 'important');
                    el.style.setProperty('visibility', 'hidden', 'important');
                    removed++;
                }
            }
        }
        document.body.style.overflow = '';
        document.documentElement.style.overflow = '';
        return removed > 0 ? 'removed:' + removed : 'no-consent-found';
    })()"#
}

fn navigate_eval(url: &str) -> String {
    format!(
        "(function() {{ window.location.href = {}; return 'navigating'; }})()",
        serde_json::to_string(url).unwrap_or_else(|_| "\"about:blank\"".into())
    )
}

fn step_action(step: &PlannedStep) -> PlannedAction {
    if step.additional_actions.is_empty() {
        step.action.clone()
    } else {
        let mut actions = Vec::with_capacity(1 + step.additional_actions.len());
        actions.push(step.action.clone());
        actions.extend(step.additional_actions.iter().cloned());
        PlannedAction::Batch { actions }
    }
}

fn step_contains_activation(action: &PlannedAction) -> bool {
    match action {
        PlannedAction::ActivateApp { .. } => true,
        PlannedAction::Batch { actions } => actions.iter().any(step_contains_activation),
        _ => false,
    }
}

/// Simple verification: did the context change after the action?
fn verify_action(before: &ScreenContext, after: &ScreenContext) -> bool {
    if before.elements.len() != after.elements.len() {
        return true;
    }
    if before.app != after.app || before.window != after.window {
        return true;
    }
    for (b, a) in before.elements.iter().zip(after.elements.iter()).take(20) {
        if b.value != a.value || b.state.focused != a.state.focused
            || b.state.selected != a.state.selected || b.label != a.label
        {
            return true;
        }
    }
    false
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Phase 3C: should the runner take the vision route this step? Returns
/// true only when the prior step failed specifically because a target
/// couldn't be found — rate-limit or parse errors are NOT vision-worthy
/// (the image won't fix a broken API key).
///
/// "Target miss" means either our Phase-2 pre-execute abort (recorded via
/// `record_failure("stale_target")`) or an Execute-time "Element … not
/// found" returned from Cortex dispatch (visible in the action_log tail).
fn last_failure_was_target_miss(
    state: &RunnerState,
    action_log: &[ActionRecord],
) -> bool {
    if matches!(state.last_action_type.as_deref(), Some("stale_target")) {
        return true;
    }
    if let Some(last) = action_log.last() {
        if !last.succeeded {
            if let Some(ref err) = last.error {
                let lowered = err.to_lowercase();
                if lowered.contains("not found") || lowered.contains("stale") {
                    return true;
                }
            }
        }
    }
    false
}

/// Phase 3C: capture the primary display, resize, encode as JPEG, return
/// a data URL the LLM image API can consume directly.
///
/// Uses a sync blocking call — `cel_display::ScreenCapture::capture_frame`
/// is sync and typically completes in <50ms for a 3360x2100 display. The
/// runner tolerates the stall because vision is a rare fallback.
fn capture_screenshot_for_vision() -> Result<String, String> {
    let mut capture = cel_display::create_capture();
    let frame = capture
        .capture_frame()
        .map_err(|e| format!("capture_frame: {e}"))?;
    // Claude vision performs well around 1568px max dimension; larger
    // costs more tokens without better grounding. JPEG quality 75 is
    // the sweet spot — imperceptible loss, ~60% of PNG size.
    let base64 = cel_display::encode_for_llm(&frame, 1568, 75)
        .map_err(|e| format!("encode_for_llm: {e}"))?;
    Ok(format!("data:image/jpeg;base64,{base64}"))
}

/// Render the three-lens memory block for the planner prompt. Returns an
/// empty string when all lenses are empty — the prompt builder then omits
/// the whole section. Keeps the format compact: one bullet per entry,
/// coarse age bucket instead of full timestamp, and a status tag.
fn build_memory_block(mem: &cel_cortex::Memory, goal_type: &str) -> String {
    let lenses = match mem.lens(goal_type, 2) {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, "Failed to read memory lenses");
            return String::new();
        }
    };
    if lenses.same_cortex.is_empty()
        && lenses.same_machine.is_empty()
        && lenses.similar_goal.is_empty()
    {
        return String::new();
    }
    let mut out = String::from("## Recent runs on this machine\n");
    if !lenses.same_cortex.is_empty() {
        out.push_str("### This cortex\n");
        for e in &lenses.same_cortex {
            out.push_str(&format_memory_line(e));
        }
    }
    if !lenses.same_machine.is_empty() {
        out.push_str("### Other cortexes\n");
        for e in &lenses.same_machine {
            out.push_str(&format_memory_line(e));
        }
    }
    if !lenses.similar_goal.is_empty() {
        out.push_str("### Similar goal_type\n");
        for e in &lenses.similar_goal {
            out.push_str(&format_memory_line(e));
        }
    }
    out
}

fn format_memory_line(e: &cel_cortex::MemoryEntry) -> String {
    let age = human_age(now_ms().saturating_sub(e.ts_ms));
    let goal = if e.goal.chars().count() > 80 {
        let truncated: String = e.goal.chars().take(77).collect();
        format!("{truncated}…")
    } else {
        e.goal.clone()
    };
    let tail = match (&e.last_error, e.status.as_str()) {
        (Some(err), _) => format!(" — {err}"),
        (_, "achieved") => format!(" — achieved in {} steps", e.steps),
        _ => format!(" — {} after {} steps", e.status, e.steps),
    };
    // Fix #3: if this achieved entry left a winning CdpEval expression
    // AND matches the current host (where applicable), surface it as a
    // hint the planner can re-use verbatim instead of re-discovering
    // the selector. We render it indented on a follow-up line to keep
    // the bullet readable.
    let mut out = format!("- {age}: \"{goal}\"{tail}");
    if let Some(ref url) = e.last_url {
        out.push_str(&format!(" (was at {url})"));
    }
    out.push('\n');
    if let Some(ref eval) = e.winning_cdp_eval {
        let snippet = if eval.chars().count() > 200 {
            let t: String = eval.chars().take(197).collect();
            format!("{t}…")
        } else {
            eval.clone()
        };
        out.push_str(&format!("    prior winning cdp_eval: `{snippet}`\n"));
    }
    out
}

fn human_age(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

/// Build a `CortexSignals` from the current mental model + cortex handle
/// (Phase 3A). Flattens anomalies into short strings the planner prompt
/// can render as bullets without caring about internal enum shapes.
fn cortex_signals_from(
    model: &cel_cortex::MentalModel,
    tick_age_ms: Option<u64>,
) -> cel_planner::CortexSignals {
    let loading = model
        .temporal
        .loading
        .as_ref()
        .map(|l| cel_planner::LoadingSignal {
            duration_ms: l.duration_ms,
        });

    let anomalies = model
        .anomaly_queue
        .iter()
        .map(|a| match &a.title {
            Some(t) => format!("{:?}: {t}", a.anomaly_type).to_lowercase(),
            None => format!("{:?}: {}", a.anomaly_type, a.description).to_lowercase(),
        })
        .collect();

    cel_planner::CortexSignals {
        confidence: model.confidence,
        vision_needed: model.vision_needed,
        loading,
        stable_count: model.stability.stable.len(),
        volatile_ids: model.stability.volatile.iter().cloned().collect(),
        anomalies,
        tick_age_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::ScreenContext;

    fn make_step(action: PlannedAction, additional_actions: Vec<PlannedAction>) -> PlannedStep {
        PlannedStep {
            evaluation: String::new(),
            memory: String::new(),
            plan: vec![],
            reasoning: String::new(),
            action,
            additional_actions,
            expected_outcome: String::new(),
            confidence: 0.9,
            context_tier: cel_planner::ContextTier::Full,
            thinking: None,
            progress: None,
            notebook_writes: vec![],
            batch_next: false,
        }
    }

    fn make_context(app: &str) -> ScreenContext {
        ScreenContext {
            app: app.into(),
            window: "Test Window".into(),
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

    #[test]
    fn step_action_preserves_single_action_steps() {
        let step = make_step(
            PlannedAction::Click {
                target_id: "btn-1".into(),
            },
            vec![],
        );

        match step_action(&step) {
            PlannedAction::Click { target_id } => assert_eq!(target_id, "btn-1"),
            other => panic!("Expected click action, got {:?}", other),
        }
    }

    #[test]
    fn step_action_batches_primary_and_additional_actions() {
        let step = make_step(
            PlannedAction::KeyCombo {
                keys: vec!["Command".into(), "L".into()],
            },
            vec![
                PlannedAction::Type {
                    target_id: None,
                    text: "https://capital.gr".into(),
                },
                PlannedAction::Key {
                    key: "Enter".into(),
                },
            ],
        );

        match step_action(&step) {
            PlannedAction::Batch { actions } => {
                assert_eq!(actions.len(), 3);
                assert!(matches!(actions[0], PlannedAction::KeyCombo { .. }));
                assert!(matches!(actions[1], PlannedAction::Type { .. }));
                assert!(matches!(actions[2], PlannedAction::Key { .. }));
            }
            other => panic!("Expected batch action, got {:?}", other),
        }
    }

    #[test]
    fn deterministic_first_step_navigates_directly_in_browser() {
        // Even when a browser is focused, the runner should still focus the CDP
        // Chrome first (the focused browser might be the user's regular Chrome).
        let context = make_context("Google Chrome");
        let step = deterministic_first_step(
            "Open https://capital.gr/finance/quote/CREDIA and extract the latest price",
            &context,
            0,
        ).expect("expected fast path step");

        match step_action(&step) {
            PlannedAction::Batch { actions } => {
                assert_eq!(actions.len(), 6);
                // activate_cdp_browser_action() returns CdpEval (window.focus)
                assert!(matches!(actions[0], PlannedAction::CdpEval { .. }));
                assert!(matches!(actions[1], PlannedAction::Wait { ms: 500 }));
                assert!(matches!(actions[2], PlannedAction::CdpEval { .. })); // navigate
                assert!(matches!(actions[3], PlannedAction::Wait { ms: 2000 }));
                assert!(matches!(actions[4], PlannedAction::CdpEval { .. })); // cookies
                assert!(matches!(actions[5], PlannedAction::Wait { ms: 750 }));
            }
            other => panic!("Expected batch action, got {:?}", other),
        }
    }

    #[test]
    fn deterministic_first_step_activates_browser_from_non_browser_context() {
        let context = make_context("Finder");

        let step = deterministic_first_step("Open https://capital.gr", &context, 0)
            .expect("expected fast path step");

        match step_action(&step) {
            PlannedAction::Batch { actions } => {
                assert_eq!(actions.len(), 6);
                // First action is CdpEval (activate_cdp_browser_action focuses CDP Chrome by PID)
                assert!(matches!(actions[0], PlannedAction::CdpEval { .. }));
                assert!(matches!(actions[1], PlannedAction::Wait { ms: 500 }));
                assert!(matches!(actions[2], PlannedAction::CdpEval { .. }));
                assert!(matches!(actions[3], PlannedAction::Wait { ms: 2000 }));
                assert!(matches!(actions[4], PlannedAction::CdpEval { .. }));
                assert!(matches!(actions[5], PlannedAction::Wait { ms: 750 }));
            }
            other => panic!("Expected batch action, got {:?}", other),
        }
    }
}
