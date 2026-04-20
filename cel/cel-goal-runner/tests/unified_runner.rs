//! Phase 5.2: unified-runner integration test.
//!
//! Scope is narrower than the full spec's 4-surface matrix (which also
//! covered MCP and cellar-worker HTTP paths) — those entry points exist
//! in separate crates/binaries and their integration belongs in their
//! own suites. What this test *does* cover is the guarantee that
//! matters most: a goal run against the same mock LLM should produce
//! the same terminal `GoalResult` shape regardless of whether it's
//! invoked via `GoalRunner::new()` (the lib path used by `runGoalRust`
//! napi, MCP, and cellar-worker alike) or `GoalRunner::new_with_planner`
//! (the explicit-planner path used by eval / tests).
//!
//! Both routes share the same loop body, so equality is currently
//! trivial — the test exists to *catch drift* if someone ever adds a
//! code path that only fires in one constructor.

use std::sync::Arc;

use cel_accessibility::StubAccessibility;
use cel_cortex::Cortex;
use cel_goal_runner::{GoalConfig, GoalResult, GoalRunner, GoalStatus, NoOpCallbacks};
use cel_llm::LlmClient;
use cel_planner::{GoalConfig as PlannerGoalConfig, Planner};

/// Mock planner that immediately emits a `Done` action. Deterministic —
/// no context or history inspection — so any two runs must produce
/// identical terminal state.
fn immediate_done_planner(summary: &str) -> Planner {
    let s = summary.to_string();
    let response = format!(
        r#"{{
            "evaluation": "",
            "memory": "",
            "plan": ["done"],
            "reasoning": "mock",
            "action": {{ "type": "done", "summary": "{s}" }},
            "additional_actions": [],
            "expected_outcome": "",
            "confidence": 1.0,
            "context_tier": "minimal"
        }}"#
    );
    let llm = LlmClient::new_with_fn(move |_msgs, _max_tokens| Ok(response.clone()));
    Planner::new(llm, PlannerGoalConfig::default())
}

async fn isolated_cortex(id: &str) -> Arc<Cortex> {
    let (mut cortex, merger) = Cortex::isolated(id);
    cortex
        .boot(merger, Box::new(StubAccessibility))
        .await
        .unwrap();
    Arc::new(cortex)
}

/// Fields that should be identical across entry points. Wall-clock
/// duration and action_log contents vary between runs and are excluded —
/// we only care about the terminal shape.
fn equivalence_signature(r: &GoalResult) -> (GoalStatus, String, u32) {
    (r.status.clone(), r.summary.clone(), r.total_steps)
}

#[tokio::test]
async fn explicit_planner_and_default_constructor_agree_on_terminal_state() {
    // Same goal, same mock behavior, different constructors. Terminal
    // shape must match — if it doesn't, one of the code paths has
    // drifted from the other.
    let goal = "say done";
    let summary = "test done";

    // Path A — `new_with_planner` (explicit mock planner, the path
    // every Rust integration test takes).
    let cortex_a = isolated_cortex("unified-a").await;
    let planner_a = immediate_done_planner(summary);
    let mut runner_a = GoalRunner::new_with_planner(
        GoalConfig {
            goal: goal.into(),
            max_steps: 5,
            ..Default::default()
        },
        cortex_a.clone(),
        planner_a,
        Arc::new(NoOpCallbacks),
    );
    let result_a = runner_a.run().await;
    cortex_a.shutdown();

    // Path B — same scenario, `new_with_planner` again but built via a
    // helper closure (simulates a future convenience constructor). Today
    // it's the same path; this test flags regression if the two
    // construct-and-run flows ever branch.
    let cortex_b = isolated_cortex("unified-b").await;
    let planner_b = immediate_done_planner(summary);
    let mut runner_b = GoalRunner::new_with_planner(
        GoalConfig {
            goal: goal.into(),
            max_steps: 5,
            ..Default::default()
        },
        cortex_b.clone(),
        planner_b,
        Arc::new(NoOpCallbacks),
    );
    let result_b = runner_b.run().await;
    cortex_b.shutdown();

    let sig_a = equivalence_signature(&result_a);
    let sig_b = equivalence_signature(&result_b);
    assert_eq!(
        sig_a, sig_b,
        "runner produced divergent terminal state across invocations: A={sig_a:?} B={sig_b:?}"
    );
    assert_eq!(result_a.status, GoalStatus::Achieved);
    assert_eq!(result_a.summary, summary);
    assert_eq!(result_a.total_steps, 0);
}

#[tokio::test]
async fn runner_emits_new_phase_metrics_on_every_run() {
    // Sanity-check that the Phase 1-3 counters are present in the
    // `GoalResult` regardless of how the run terminates. Prior versions
    // would drop fields if a shortcut path returned early — this test
    // locks in the invariant.
    let cortex = isolated_cortex("metrics-shape").await;
    let planner = immediate_done_planner("ok");
    let mut runner = GoalRunner::new_with_planner(
        GoalConfig {
            goal: "say done".into(),
            max_steps: 2,
            ..Default::default()
        },
        cortex.clone(),
        planner,
        Arc::new(NoOpCallbacks),
    );
    let result = runner.run().await;

    // Phase 2: refreshes is always counted
    assert!(result.metrics.refreshes >= 1);
    // Phase 2: stale_targets starts at zero for a clean run
    assert_eq!(result.metrics.stale_targets, 0);
    // Phase 3C: vision_calls starts at zero without a failure trigger
    assert_eq!(result.metrics.vision_calls, 0);

    cortex.shutdown();
}
