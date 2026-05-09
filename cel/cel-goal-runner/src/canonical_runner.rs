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
use cel_contracts::{
    AttemptRecord, FailureReport, GoalOutcome, NextMove, PlanProducer, PlannedAction,
    PlanningBudget, RunLimits, RuntimeCaps, Step, StepResult,
};
use cel_cortex::{build_planning_view, Cortex, PlanningViewInputs};

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

    /// Tier A3: snapshot of the cortex's current anomaly queue.
    /// Production `CortexStepExecutor` reads this from the live
    /// `MentalModel`; test executors return empty by default. Surfaced
    /// per turn into `PlanningView.anomalies` (and the blocking subset
    /// into `PlanningView.blockers`) by the cortex selector.
    async fn cortex_anomalies(&self) -> Vec<cel_cortex::Anomaly> {
        vec![]
    }

    /// Tier A3: snapshot of the cortex's freshness assessment.
    /// `HardStale` becomes a `Blocker`; `SoftStale` becomes an
    /// `AnomalyRef`; `Fresh` contributes nothing. `None` skips the
    /// freshness signal entirely (default for test executors that
    /// don't have a `MentalModel`).
    async fn cortex_freshness(&self) -> Option<cel_cortex::FreshnessAssessment> {
        None
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
    /// WK2 (un-deferred): optional embedder for cortex memory.
    /// When wired, the runner embeds the goal once per run (used by
    /// the cortex selector for cosine-boosting candidate memories) and
    /// the outcome memory at write time (so future runs can compare).
    /// `None` means WK1 deterministic recall only — no embedding work
    /// done, no per-call latency or token cost.
    embedder: Option<Arc<dyn cel_llm::Embedder>>,
}

impl<P: PlanProducer, X: StepExecutor> CanonicalGoalRunner<P, X> {
    pub fn new(planner: P, executor: X) -> Self {
        Self {
            planner,
            executor,
            embedder: None,
        }
    }

    /// WK2: builder-style opt-in to embedder-aware memory.
    ///
    /// Pass `Arc::new(MyEmbedder { ... })` to enable. Until called, the
    /// runner ignores embeddings entirely — production preserved
    /// pre-WK2 behaviour for callers who haven't wired one up.
    pub fn with_embedder(mut self, embedder: Arc<dyn cel_llm::Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Run `goal` to completion or structured failure.
    ///
    /// Thin outer wrapper: opens the cortex memory store once (if the
    /// caller opted in via both `RunLimits.workflow_id_for_memory` AND
    /// `RunLimits.memory_db_path`), pre-computes the goal embedding
    /// once (if an embedder is wired), runs the loop with both handles
    /// for per-turn memory READS, then writes the final outcome memory
    /// with the same handle (and embedding, if available).
    ///
    /// WK4: 1 SQLite open per memory-enabled run.
    /// WK2: 1 goal-embed call + 1 outcome-embed call per memory-enabled
    /// run when an embedder is wired; 0 otherwise.
    pub async fn run(&self, goal: &str, limits: RunLimits) -> GoalOutcome {
        let start = Instant::now();
        let memory_store = open_memory_store_if_enabled(&limits);

        // WK2: embed the goal once (it doesn't change within a run).
        // Failure logs at WARN and falls back to None — selector then
        // skips the cosine boost, behaviour matches pre-WK2.
        let goal_embedding = compute_goal_embedding(self.embedder.as_ref(), goal).await;

        let outcome = self
            .run_inner(
                goal,
                &limits,
                start,
                memory_store.as_ref(),
                goal_embedding.as_deref(),
            )
            .await;
        write_outcome_memory_if_enabled(
            memory_store.as_ref(),
            self.embedder.as_ref(),
            &limits,
            goal,
            &outcome,
            start.elapsed(),
        )
        .await;
        outcome
    }

    async fn run_inner(
        &self,
        goal: &str,
        limits: &RunLimits,
        start: Instant,
        memory_store: Option<&std::sync::Mutex<cel_store::CelStore>>,
        goal_embedding: Option<&[u8]>,
    ) -> GoalOutcome {
        info!(
            goal,
            max_steps = limits.max_steps,
            "Canonical runner started"
        );
        let mut history: Vec<AttemptRecord> = Vec::new();
        let mut shared_memory = serde_json::json!({});
        let mut steps_used: u32 = 0;
        let mut last_batch_purpose = String::from("<goal entry>");
        // Loop-detection: same action fired consecutively with errors.
        let mut consecutive_repeat: u32 = 0;
        let mut last_action_hash: u64 = 0;
        // Phase-gate: how many times we've injected a "budget past
        // midpoint, terminal app not yet reached" synthetic record.
        // On the 2nd fire we auto-dispatch activate_app(terminal_app).
        let mut phase_gate_fires: u32 = 0;

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
            // Tier A3: snapshot the cortex's current anomaly queue +
            // freshness state so the planner sees them in `view.anomalies`,
            // `view.blockers`, and the rationale string. Per-turn so a
            // newly-detected anomaly (e.g. a modal that just appeared)
            // surfaces immediately.
            let cortex_anomalies = self.executor.cortex_anomalies().await;
            let cortex_freshness = self.executor.cortex_freshness().await;

            // Build the budgeted planning view from this turn's perception +
            // caps. Replaces the raw `&ScreenContext` + `&RuntimeCaps` pair
            // the planner used to receive directly.
            //
            // PR3 + WK4: when the caller opted in to memory writes via
            // `RunLimits.workflow_id_for_memory` + `RunLimits.memory_db_path`,
            // also use those for memory READS. The planner sees prior
            // memories from past runs of the same workflow on every turn.
            // WK4: the store is opened once in `run` and shared across
            // every turn here — `&Mutex<CelStore>` auto-coerces to
            // `&dyn CortexMemoryStore`.
            let planning_budget = PlanningBudget::default();
            let view = build_planning_view(&PlanningViewInputs {
                goal,
                budget: &planning_budget,
                perception: &perception,
                caps: &caps,
                memory_store: memory_store.map(|m| m as &dyn cel_store::CortexMemoryStore),
                workflow_id: limits.workflow_id_for_memory.as_deref(),
                // Tier A1: same handle satisfies KnowledgeStore. The
                // canonical runner opts the planner into knowledge
                // hydration whenever it opted into memory hydration —
                // both depend on the same `Mutex<CelStore>` opened by
                // `open_memory_store_if_enabled`. A future caller that
                // wants knowledge WITHOUT memory could split this; for
                // now the two opt-ins move together.
                knowledge_store: memory_store.map(|m| m as &dyn cel_store::KnowledgeStore),
                // WK2: pre-computed goal embedding (run-once at the
                // top of `run()`). When present + the candidate has a
                // stored embedding of matching dimension, the selector
                // adds a cosine boost on top of WK1's FTS5+decay base.
                goal_embedding,
                // Tier A2: same handle satisfies RecentEventStore for
                // hydrating PlanningView.recent_events from cortex
                // observations. Couples to the same memory opt-in for
                // now (workflow-scoped via workflow_id_for_memory).
                recent_events_store: memory_store.map(|m| m as &dyn cel_store::RecentEventStore),
                // Tier A3: pass the per-turn cortex anomaly queue +
                // freshness snapshot computed above. Always present
                // (even from test executors via the trait defaults —
                // empty Vec / None there).
                cortex_anomalies: Some(cortex_anomalies.as_slice()),
                cortex_freshness: cortex_freshness.as_ref(),
            });

            // Phase gate: past the budget midpoint with no terminal-
            // app work yet → inject a synthetic history record telling
            // the planner to pivot to the terminal app. Second ignore
            // escalates to runner auto-dispatching activate_app.
            if let Some(record) =
                phase_gate_check(limits, steps_used, &history, &perception, phase_gate_fires)
            {
                phase_gate_fires += 1;
                warn!(
                    fires = phase_gate_fires,
                    terminal_app = ?limits.terminal_app,
                    "phase gate fired — injecting synthetic history record"
                );
                history.push(record);
                // Second ignore → auto-dispatch. Direct activation via
                // the executor bypasses the planner for this one step.
                if phase_gate_fires >= 2 {
                    if let Some(term) = limits.terminal_app.as_deref() {
                        warn!(
                            terminal_app = %term,
                            "phase gate escalated — runner auto-dispatching activate_app"
                        );
                        let activate_step = Step {
                            purpose: format!("phase_gate_auto_activate:{term}"),
                            kind: cel_contracts::StepKind::Deterministic,
                            action: PlannedAction::ActivateApp {
                                app_name: term.to_string(),
                            },
                        };
                        let _ = self.executor.execute(&activate_step, 1).await;
                        steps_used += 1;
                        // Loop back: next iteration will re-perceive
                        // and likely see terminal app frontmost, so
                        // the gate won't fire again.
                        continue;
                    }
                }
                steps_used += 1;
                // Continue to decide_next — the planner sees the new
                // history record and is expected to emit an
                // activate_app or write_cells in its next batch.
            }

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
                .decide_next(goal, &history, &shared_memory, &view, screenshot.as_deref())
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
                        .verify_done(goal, &summary, &shared_memory, &view, screenshot.as_deref())
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
                    let steps_iter = steps.into_iter();
                    let mut remaining: Vec<Step> = Vec::new();
                    for s in steps_iter {
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
                            // For ExtractWithFallback we key shared_memory
                            // on the logical `name` (e.g. "btc_price"),
                            // not on the step purpose. The action result
                            // carries `{ name, value, selector_used, raw }`
                            // — we expose just `value` so downstream
                            // consumers see a clean `shared_memory.btc_price
                            // = 108432.5` and can plug it into
                            // write_cells directly.
                            if let PlannedAction::ExtractWithFallback { name, .. } = &step.action {
                                let value = data
                                    .as_object()
                                    .and_then(|o| o.get("value"))
                                    .cloned()
                                    .unwrap_or(data.clone());
                                merge_into_shared_memory(&mut shared_memory, name, value);
                            } else {
                                merge_into_shared_memory(
                                    &mut shared_memory,
                                    &step.purpose,
                                    data.clone(),
                                );
                            }
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

                        // Extraction retry budget: when an
                        // ExtractWithFallback fails 3 times for the same
                        // `name`, auto-null in shared_memory and append
                        // a synthetic "abandoned" record so the planner
                        // sees the field is lost and stops retrying.
                        // This generalizes: any page that doesn't
                        // surface a field after K selector lists will
                        // free the budget instead of trapping the
                        // agent in extraction polish.
                        let mut extraction_budget_hit = false;
                        if !succeeded {
                            if let PlannedAction::ExtractWithFallback { name, .. } = &step.action {
                                let failures_for_name = history
                                    .iter()
                                    .filter(|r| !r.succeeded)
                                    .filter(|r| matches!(
                                        &r.action,
                                        PlannedAction::ExtractWithFallback { name: n, .. } if n == name
                                    ))
                                    .count();
                                let already_nulled = shared_memory
                                    .as_object()
                                    .and_then(|o| o.get(name))
                                    .map(|v| v.is_null())
                                    .unwrap_or(false);
                                if failures_for_name >= 3 && !already_nulled {
                                    // Insert null directly —
                                    // merge_into_shared_memory short-
                                    // circuits on null values.
                                    if let Some(obj) = shared_memory.as_object_mut() {
                                        obj.insert(name.clone(), serde_json::Value::Null);
                                    } else {
                                        shared_memory = serde_json::json!({ name.clone(): serde_json::Value::Null });
                                    }
                                    history.push(AttemptRecord {
                                        step_purpose: format!("extraction_budget:{name}"),
                                        action: step.action.clone(),
                                        succeeded: false,
                                        error: Some(format!(
                                            "extraction for `{name}` abandoned after {} failed attempts — shared_memory.{name} set to null. Stop retrying this field; move on with the goal's other parts.",
                                            failures_for_name
                                        )),
                                        data: serde_json::Value::Null,
                                    });
                                    warn!(
                                        target = %name,
                                        attempts = failures_for_name,
                                        "extraction budget exhausted; auto-null in shared_memory"
                                    );
                                    extraction_budget_hit = true;
                                }
                            }
                        }

                        // When the extraction retry budget fires, we
                        // don't want the generic 3-strike failure below
                        // to ALSO kill the run — the agent is meant to
                        // continue with a null for this field. Reset
                        // the consecutive counter so the 3-strike
                        // guard below sees a clean slate.
                        if extraction_budget_hit {
                            consecutive_repeat = 0;
                            last_action_hash = 0;
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
        attempts: vec![format!("{kind} budget exhausted after {steps_used} steps")],
    })
}

/// WK4: open the cortex memory store once if both opt-in fields are set.
///
/// Wraps in `Mutex<CelStore>` so the resulting handle satisfies the
/// `CortexMemoryStore` trait's `Send + Sync` bound (required for use
/// across async-fn awaits inside `run_inner`). Failure to open is logged
/// at WARN — the run continues with `None`, identical to the
/// "didn't opt in" path. Open errors no longer manifest mid-run.
fn open_memory_store_if_enabled(
    limits: &RunLimits,
) -> Option<std::sync::Mutex<cel_store::CelStore>> {
    let path = match (
        limits.workflow_id_for_memory.as_deref(),
        limits.memory_db_path.as_deref(),
    ) {
        (Some(_), Some(p)) => p,
        _ => return None,
    };
    match cel_store::CelStore::open(path) {
        Ok(s) => Some(std::sync::Mutex::new(s)),
        Err(e) => {
            tracing::warn!(
                db_path = path,
                error = %e,
                "WK4: failed to open cortex memory store; \
                 run continues with no memory reads/writes",
            );
            None
        }
    }
}

/// WK2: embed the goal text via the wired embedder, or return None when
/// no embedder is configured. Failure logs at WARN and returns None so
/// the run continues with the WK1 deterministic selector path
/// (FTS5+decay only, no cosine boost). Called once per run from `run()`
/// so the loop doesn't pay per-turn embed latency.
async fn compute_goal_embedding(
    embedder: Option<&Arc<dyn cel_llm::Embedder>>,
    goal: &str,
) -> Option<Vec<u8>> {
    let emb = embedder?;
    match emb.embed(goal).await {
        Ok(v) => Some(v.to_bytes()),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "WK2: goal embedding failed; selector falls back to FTS5+decay only",
            );
            None
        }
    }
}

/// PR2: write a final outcome memory to `cortex_memories` if the caller
/// opted in. No-op when `workflow_id_for_memory` is `None` or when the
/// store handle is `None` (open failed earlier or caller didn't opt in).
/// Failure to write is logged at WARN — never propagates back to the
/// caller. The runner's primary contract is "report the run outcome";
/// memory persistence is an opt-in side effect.
///
/// WK4: takes the already-open `Mutex<CelStore>` from `run` instead of
/// re-opening from `RunLimits.memory_db_path`. Same data, one fewer open.
///
/// WK2: also takes the optional embedder. When wired, the summary text
/// is embedded once and stored on the new memory's `embedding` column —
/// future runs of the same workflow can then cosine-boost it during
/// selection (see `planning_view::score_memory`). Embed failure logs at
/// WARN and writes the memory with `embedding: None` (still useful via
/// FTS5+decay; the cosine path just won't fire for this entry).
async fn write_outcome_memory_if_enabled(
    memory_store: Option<&std::sync::Mutex<cel_store::CelStore>>,
    embedder: Option<&Arc<dyn cel_llm::Embedder>>,
    limits: &RunLimits,
    goal: &str,
    outcome: &GoalOutcome,
    duration: std::time::Duration,
) {
    let (store, workflow_id) = match (memory_store, limits.workflow_id_for_memory.as_deref()) {
        (Some(s), Some(w)) => (s, w),
        _ => return,
    };

    let (kind, summary, content) = match outcome {
        GoalOutcome::Succeeded {
            summary,
            extracted_data,
        } => {
            let summary_text = if summary.is_empty() {
                format!("Completed: {goal}")
            } else {
                summary.clone()
            };
            let payload = serde_json::json!({
                "kind": "outcome",
                "goal": goal,
                "status": "succeeded",
                "summary": summary,
                "extracted_data": extracted_data,
                "duration_ms": duration.as_millis() as u64,
                "ts": chrono_iso_now(),
            });
            (
                cel_store::cortex_memory::MemoryKind::Outcome,
                summary_text,
                payload,
            )
        }
        GoalOutcome::Failed(report) => {
            let summary_text = format!(
                "Did not complete: {} (failing step: {})",
                goal, report.failing_step
            );
            let payload = serde_json::json!({
                "kind": "failure",
                "goal": goal,
                "status": "failed",
                "failing_sub_goal": report.failing_sub_goal,
                "failing_step": report.failing_step,
                "attempts": report.attempts,
                "duration_ms": duration.as_millis() as u64,
                "ts": chrono_iso_now(),
            });
            (
                cel_store::cortex_memory::MemoryKind::Failure,
                summary_text,
                payload,
            )
        }
    };

    // WK2: embed the summary text once when an embedder is wired.
    // Falls back to None on embed failure (logged); falls through to
    // None when no embedder is wired at all (no per-call cost).
    let embedding = match embedder {
        Some(emb) => match emb.embed(&summary).await {
            Ok(v) => Some(v.to_bytes()),
            Err(e) => {
                tracing::warn!(
                    workflow_id,
                    error = %e,
                    "WK2 outcome memory: embed failed; storing memory without embedding",
                );
                None
            }
        },
        None => None,
    };

    let new_memory = cel_store::cortex_memory::NewCortexMemory {
        workflow_id: workflow_id.to_string(),
        kind,
        content,
        summary: Some(summary),
        tags: vec!["canonical_runner".into()],
        source_ref: Some(format!(
            "canonical_runner:duration_ms={}",
            duration.as_millis()
        )),
        embedding,
    };

    // Use the `CortexMemoryStore` trait so the inherent `&CelStore`
    // method is reached via the same path the planning_view selector
    // takes — keeps both sides on the same contract.
    match cel_store::CortexMemoryStore::insert_memory(store, &new_memory) {
        Ok(id) => tracing::info!(
            workflow_id,
            memory_id = id,
            "PR2 outcome memory: wrote final-outcome memory for canonical run"
        ),
        Err(e) => tracing::warn!(
            workflow_id,
            error = %e,
            "PR2 outcome memory: insert failed; outcome itself was unaffected",
        ),
    }
}

/// ISO-8601 timestamp without pulling in chrono. Format: YYYY-MM-DDTHH:MM:SSZ.
fn chrono_iso_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs / 86_400;
    let remaining = secs % 86_400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    let (year, month, day) = unix_days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert unix-epoch days to (year, month, day). Naive Gregorian, valid
/// 1970..2100. Used only for memory record timestamps; precision needs are
/// "human-readable later" not scientific.
fn unix_days_to_ymd(days_from_epoch: i64) -> (i64, u32, u32) {
    // Days since 1970-01-01.
    let mut days = days_from_epoch;
    let mut year: i64 = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days >= dy {
            days -= dy;
            year += 1;
        } else {
            break;
        }
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    for &dm in &months {
        let dm_actual = if month == 2 && is_leap(year) { 29 } else { dm };
        if days >= dm_actual {
            days -= dm_actual;
            month += 1;
        } else {
            break;
        }
    }
    (year, month, (days + 1) as u32)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Decide whether the phase gate should fire this turn.
///
/// Returns `Some(AttemptRecord)` when ALL of:
/// 1. `limits.terminal_app` is set.
/// 2. `steps_used >= max_steps / 2`.
/// 3. No `WriteCells` or `SaveDocument` action has yet succeeded in
///    history (i.e. the agent has not begun landing output).
/// 4. `perception.app` differs from `terminal_app` (case-insensitive
///    substring match — macOS reports "Google Chrome", the scenario
///    might say "Chrome").
///
/// The injected record pushes the planner to pivot. If this is the
/// second fire (caller tracks `phase_gate_fires` and escalates
/// separately), the caller auto-dispatches activation.
fn phase_gate_check(
    limits: &RunLimits,
    steps_used: u32,
    history: &[AttemptRecord],
    perception: &ScreenContext,
    prior_fires: u32,
) -> Option<AttemptRecord> {
    let terminal_app = limits.terminal_app.as_ref()?;
    if limits.max_steps == 0 {
        return None;
    }
    // Fire at midpoint; then throttle so we don't re-inject every turn
    // after. Specifically: fire once at >=50% used, and once more at
    // >=75% used (which is where the auto-dispatch kicks in).
    let used_pct = steps_used * 100 / limits.max_steps.max(1);
    let should_consider = match prior_fires {
        0 => used_pct >= 50,
        1 => used_pct >= 75,
        _ => false,
    };
    if !should_consider {
        return None;
    }

    // Has the agent begun landing output?
    let landed = history.iter().any(|r| {
        r.succeeded
            && matches!(
                &r.action,
                PlannedAction::WriteCells { .. } // Future: SaveDocument goes here too.
            )
    });
    if landed {
        return None;
    }

    // Is the terminal app already frontmost?
    let frontmost_matches = app_matches(&perception.app, terminal_app);
    if frontmost_matches {
        return None;
    }

    Some(AttemptRecord {
        step_purpose: "phase_gate".into(),
        action: PlannedAction::Wait { ms: 0 },
        succeeded: false,
        error: Some(format!(
            "phase gate: you are {}% through your step budget ({}/{}), \
             frontmost app is `{}`, but the goal's terminal app is `{}` \
             and no write_cells/save_document has landed. \
             Your NEXT batch MUST begin with activate_app({}). \
             If you ignore this, the runner will dispatch it for you on the next gate fire.",
            used_pct,
            steps_used,
            limits.max_steps,
            if perception.app.is_empty() {
                "<unknown>"
            } else {
                &perception.app
            },
            terminal_app,
            terminal_app
        )),
        data: serde_json::Value::Null,
    })
}

/// Loose app-name match: macOS reports `"Google Chrome"`, planners
/// often say `"Chrome"`. Accept either direction as a substring match.
fn app_matches(frontmost: &str, target: &str) -> bool {
    let a = frontmost.trim().to_lowercase();
    let b = target.trim().to_lowercase();
    !a.is_empty() && (a.contains(&b) || b.contains(&a))
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
        PlannedAction::Key { .. } | PlannedAction::KeyCombo { .. } | PlannedAction::Wait { .. } => {
            false
        }
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
            cdp_browser: if cdp_bound {
                Some("Google Chrome".into())
            } else {
                None
            },
            cdp_url,
            native_input: self.cortex.native_input_allowed(),
            steps_used: 0,
            max_steps: 0,
        }
    }

    /// Tier A3: read the cortex's current anomaly queue from MentalModel.
    /// Cloning the small VecDeque is cheap (anomalies are bounded by
    /// the cortex's own dedup logic; typical queue is 0–5 entries).
    async fn cortex_anomalies(&self) -> Vec<cel_cortex::Anomaly> {
        let model = self.cortex.model();
        let guard = model.read().await;
        guard.anomaly_queue.iter().cloned().collect()
    }

    /// Tier A3: snapshot the cortex's freshness assessment. Returns
    /// `None` until the cortex tick loop populates it via
    /// `refresh_derived`; `Some(_)` after the first refresh. The
    /// selector treats both the same way (skip the freshness signal
    /// when None — pre-A3 behaviour).
    async fn cortex_freshness(&self) -> Option<cel_cortex::FreshnessAssessment> {
        let model = self.cortex.model();
        let guard = model.read().await;
        guard.freshness.clone()
    }
}

fn is_unrecoverable(action: &PlannedAction) -> bool {
    matches!(
        action,
        PlannedAction::Done { .. } | PlannedAction::Fail { .. }
    )
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
        PlannedAction::ReadCells { .. } => "read_cells".into(),
        PlannedAction::ExtractWithFallback { .. } => "extract_with_fallback".into(),
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
        PlannedAction::WriteCells { app, writes, .. } => {
            let mut summary = app.clone();
            let preview = writes
                .iter()
                .take(6)
                .map(|w| format!("{}={}", w.cell_ref, truncate(&w.value, 32)))
                .collect::<Vec<_>>()
                .join(", ");
            if !preview.is_empty() {
                summary.push(' ');
                summary.push_str(&preview);
            }
            if writes.len() > 6 {
                summary.push_str(", ...");
            }
            Some(summary)
        }
        PlannedAction::ReadCells { app, cell_refs, .. } => {
            let mut summary = app.clone();
            if !cell_refs.is_empty() {
                summary.push(' ');
                summary.push_str(
                    &cell_refs
                        .iter()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            if cell_refs.len() > 8 {
                summary.push_str(", ...");
            }
            Some(summary)
        }
        PlannedAction::Done { summary, .. } => Some(truncate(summary, 200)),
        PlannedAction::Fail { reason } => Some(truncate(reason, 200)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_contracts::{Step, StepKind};
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
            _view: &cel_contracts::PlanningView,
            _shot: Option<&[u8]>,
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
    async fn extraction_retry_budget_auto_nulls_after_three_failures() {
        // Simulate 3 consecutive ExtractWithFallback failures for the
        // same name, followed by Done. Expect: run succeeds (not
        // killed by 3-strike guard), and the agent sees null in
        // shared_memory for that name.
        let action = PlannedAction::ExtractWithFallback {
            name: "btc_price".into(),
            selectors: vec!["bad.selector".into()],
            parse_as: "float".into(),
        };
        let failing_step = Step {
            purpose: "err:no match".into(),
            kind: StepKind::Deterministic,
            action: action.clone(),
        };
        let planner = ScriptedPlanner::new(vec![
            NextMove::Batch {
                purpose: "extract".into(),
                steps: vec![
                    failing_step.clone(),
                    failing_step.clone(),
                    failing_step.clone(),
                ],
            },
            NextMove::Done {
                summary: "proceeded with partial data".into(),
                extracted_data: serde_json::Value::Null,
            },
        ]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let outcome = runner.run("x", RunLimits::default()).await;
        match outcome {
            GoalOutcome::Succeeded { extracted_data, .. } => {
                // shared_memory (exposed via extracted_data) should have
                // btc_price: null
                let got = extracted_data
                    .as_object()
                    .and_then(|o| o.get("btc_price"))
                    .cloned();
                assert_eq!(got, Some(serde_json::Value::Null), "got {extracted_data:?}");
            }
            other => panic!("expected Succeeded with null btc_price, got {other:?}"),
        }
    }

    fn empty_perception(app: &str) -> ScreenContext {
        let mut ctx = empty_context();
        ctx.app = app.into();
        ctx
    }

    #[test]
    fn phase_gate_does_not_fire_below_midpoint() {
        let limits = RunLimits {
            max_steps: 100,
            timeout_ms: 60_000,
            max_step_retries: 3,
            terminal_app: Some("Numbers".into()),
            workflow_id_for_memory: None,
            memory_db_path: None,
        };
        let history: Vec<AttemptRecord> = vec![];
        let got = phase_gate_check(&limits, 40, &history, &empty_perception("Google Chrome"), 0);
        assert!(got.is_none(), "below midpoint, gate should not fire");
    }

    #[test]
    fn phase_gate_fires_at_midpoint_when_wrong_app_frontmost() {
        let limits = RunLimits {
            max_steps: 100,
            timeout_ms: 60_000,
            max_step_retries: 3,
            terminal_app: Some("Numbers".into()),
            workflow_id_for_memory: None,
            memory_db_path: None,
        };
        let history: Vec<AttemptRecord> = vec![];
        let got = phase_gate_check(&limits, 50, &history, &empty_perception("Google Chrome"), 0);
        let rec = got.expect("gate should fire at 50% with wrong frontmost");
        assert!(rec.error.as_ref().unwrap().contains("phase gate"));
        assert!(rec
            .error
            .as_ref()
            .unwrap()
            .contains("activate_app(Numbers)"));
    }

    #[test]
    fn phase_gate_suppressed_when_already_on_terminal_app() {
        let limits = RunLimits {
            max_steps: 100,
            timeout_ms: 60_000,
            max_step_retries: 3,
            terminal_app: Some("Numbers".into()),
            workflow_id_for_memory: None,
            memory_db_path: None,
        };
        let history: Vec<AttemptRecord> = vec![];
        // frontmost reports full macOS name; target is shorter —
        // substring match should succeed.
        let got = phase_gate_check(&limits, 60, &history, &empty_perception("Numbers"), 0);
        assert!(got.is_none(), "already on terminal app, should not fire");
    }

    #[test]
    fn phase_gate_suppressed_when_write_cells_has_landed() {
        let limits = RunLimits {
            max_steps: 100,
            timeout_ms: 60_000,
            max_step_retries: 3,
            terminal_app: Some("Numbers".into()),
            workflow_id_for_memory: None,
            memory_db_path: None,
        };
        let history = vec![AttemptRecord {
            step_purpose: "already landed".into(),
            action: PlannedAction::WriteCells {
                app: "Numbers".into(),
                sheet: None,
                table: None,
                writes: vec![],
                verify: true,
            },
            succeeded: true,
            error: None,
            data: serde_json::Value::Null,
        }];
        let got = phase_gate_check(&limits, 60, &history, &empty_perception("Google Chrome"), 0);
        assert!(got.is_none(), "write_cells landed, gate should not fire");
    }

    #[test]
    fn phase_gate_fires_once_at_midpoint_and_again_at_threequarter() {
        let limits = RunLimits {
            max_steps: 100,
            timeout_ms: 60_000,
            max_step_retries: 3,
            terminal_app: Some("Numbers".into()),
            workflow_id_for_memory: None,
            memory_db_path: None,
        };
        // First fire at 50%: prior_fires=0
        assert!(
            phase_gate_check(&limits, 50, &[], &empty_perception("Chrome"), 0).is_some(),
            "first fire at 50% expected"
        );
        // After one fire, at 60% we should NOT re-fire (throttled)
        assert!(
            phase_gate_check(&limits, 60, &[], &empty_perception("Chrome"), 1).is_none(),
            "should throttle between 50 and 75"
        );
        // Second fire at 75%: prior_fires=1
        assert!(
            phase_gate_check(&limits, 75, &[], &empty_perception("Chrome"), 1).is_some(),
            "second fire at 75% expected"
        );
        // After two fires, never again
        assert!(
            phase_gate_check(&limits, 90, &[], &empty_perception("Chrome"), 2).is_none(),
            "after two fires, should stop"
        );
    }

    #[test]
    fn phase_gate_noop_without_terminal_app() {
        let limits = RunLimits {
            max_steps: 100,
            timeout_ms: 60_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: None,
            memory_db_path: None,
        };
        assert!(phase_gate_check(&limits, 80, &[], &empty_perception("Chrome"), 0).is_none());
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
                    terminal_app: None,
                    workflow_id_for_memory: None,
                    memory_db_path: None,
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

    // ─── PR2: outcome auto-write ─────────────────────────────────────────────

    fn pr2_temp_db(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!("cel_pr2_{label}_{nanos}.db"));
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn pr2_outcome_auto_write_on_succeeded_when_opted_in() {
        let db_path = pr2_temp_db("succeeded");
        let planner = ScriptedPlanner::new(vec![NextMove::Done {
            summary: "did the thing".into(),
            extracted_data: serde_json::json!({"value": 42}),
        }]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let limits = RunLimits {
            max_steps: 5,
            timeout_ms: 10_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: Some("test-pr2-success".into()),
            memory_db_path: Some(db_path.clone()),
        };
        let outcome = runner.run("test goal", limits).await;
        assert!(matches!(outcome, GoalOutcome::Succeeded { .. }));

        let store = cel_store::CelStore::open(&db_path).expect("open store");
        let memories = store
            .list_cortex_memories("test-pr2-success", None, 10)
            .expect("list memories");
        assert_eq!(memories.len(), 1, "exactly one outcome memory expected");
        assert_eq!(
            memories[0].kind,
            cel_store::cortex_memory::MemoryKind::Outcome
        );
        assert!(memories[0]
            .summary
            .as_deref()
            .map(|s| s.contains("did the thing"))
            .unwrap_or(false));
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn pr2_outcome_auto_write_on_failed_writes_failure_kind() {
        let db_path = pr2_temp_db("failed");
        let planner = ScriptedPlanner::new(vec![NextMove::Fail {
            reason: "could not finish".into(),
        }]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let limits = RunLimits {
            max_steps: 5,
            timeout_ms: 10_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: Some("test-pr2-failure".into()),
            memory_db_path: Some(db_path.clone()),
        };
        let outcome = runner.run("test goal", limits).await;
        assert!(matches!(outcome, GoalOutcome::Failed(_)));

        let store = cel_store::CelStore::open(&db_path).expect("open store");
        let memories = store
            .list_cortex_memories("test-pr2-failure", None, 10)
            .expect("list memories");
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].kind,
            cel_store::cortex_memory::MemoryKind::Failure
        );
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn pr2_outcome_no_write_when_workflow_id_missing() {
        // Privacy-safe default: even with memory_db_path set, no write
        // happens without workflow_id_for_memory.
        let db_path = pr2_temp_db("no_workflow");
        let planner = ScriptedPlanner::new(vec![NextMove::Done {
            summary: "ok".into(),
            extracted_data: serde_json::Value::Null,
        }]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let limits = RunLimits {
            max_steps: 5,
            timeout_ms: 10_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: None,
            memory_db_path: Some(db_path.clone()),
        };
        let outcome = runner.run("g", limits).await;
        assert!(matches!(outcome, GoalOutcome::Succeeded { .. }));

        if std::path::Path::new(&db_path).exists() {
            let store = cel_store::CelStore::open(&db_path).expect("open store");
            assert_eq!(
                store
                    .list_cortex_memories("anything", None, 10)
                    .unwrap()
                    .len(),
                0
            );
            let _ = std::fs::remove_file(&db_path);
        }
    }

    // ─── WK4: store-handle abstraction ──────────────────────────────────────

    #[tokio::test]
    async fn wk4_open_failure_does_not_kill_run() {
        // Bad path → store open fails. Run should still complete on the
        // happy path (no memory reads, no final write attempt). Pre-WK4
        // the open happened inside `write_outcome_memory_if_enabled` and
        // surfaced as a WARN at the end; with WK4 the open happens once
        // up front in `run` and the same WARN surfaces there. Either way,
        // the outcome itself is unaffected — that's what this test
        // pins down.
        let bad_path = "/dev/null/does-not-exist/wk4-bad.db";
        let planner = ScriptedPlanner::new(vec![NextMove::Done {
            summary: "got it".into(),
            extracted_data: serde_json::Value::Null,
        }]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let limits = RunLimits {
            max_steps: 5,
            timeout_ms: 10_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: Some("wk4-test".into()),
            memory_db_path: Some(bad_path.into()),
        };
        let outcome = runner.run("any goal", limits).await;
        assert!(
            matches!(outcome, GoalOutcome::Succeeded { .. }),
            "bad memory_db_path must not affect the run outcome; got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn wk4_store_opened_once_then_read_and_written_via_same_handle() {
        // Hard pin on the WK4 contract: when memory is enabled and the
        // run produces an outcome, exactly ONE memory should land in the
        // store under the right workflow_id, AND the store must remain
        // openable + readable from a fresh handle afterward (proving the
        // runner closed cleanly when the Mutex<CelStore> went out of
        // scope, no leaked handle blocking a re-open).
        let db_path = pr2_temp_db("wk4_oneshot");
        let planner = ScriptedPlanner::new(vec![NextMove::Done {
            summary: "completed wk4 path".into(),
            extracted_data: serde_json::json!({"k": "v"}),
        }]);
        let runner = CanonicalGoalRunner::new(planner, ScriptedExecutor::new());
        let limits = RunLimits {
            max_steps: 5,
            timeout_ms: 10_000,
            max_step_retries: 3,
            terminal_app: None,
            workflow_id_for_memory: Some("wk4-oneshot".into()),
            memory_db_path: Some(db_path.clone()),
        };
        let outcome = runner.run("test wk4", limits).await;
        assert!(matches!(outcome, GoalOutcome::Succeeded { .. }));

        // Re-open from a fresh handle — proves the runner-owned handle
        // dropped cleanly. (SQLite WAL doesn't lock readers but a leaked
        // exclusive handle on Linux can; this catches that regression.)
        let reread = cel_store::CelStore::open(&db_path).expect("re-open");
        let memories = reread
            .list_cortex_memories("wk4-oneshot", None, 10)
            .expect("list");
        assert_eq!(memories.len(), 1, "expected exactly one outcome memory");
        assert_eq!(
            memories[0].kind,
            cel_store::cortex_memory::MemoryKind::Outcome
        );
        let _ = std::fs::remove_file(&db_path);
    }
}
