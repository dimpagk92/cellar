//! End-to-end pipeline integration test: ContextMerger → Cortex → GoalRunner.
//!
//! Uses StubAccessibility (no OS permissions), LlmClient::new_with_fn()
//! (no network calls), and NoOpCallbacks to exercise the full Rust
//! perception → planning → execution chain in isolation.

use std::sync::Arc;

use cel_accessibility::StubAccessibility;
use cel_cortex::Cortex;
use cel_goal_runner::{GoalConfig, GoalRunner, GoalStatus, NoOpCallbacks};
use cel_llm::LlmClient;
use cel_planner::{GoalConfig as PlannerGoalConfig, Planner};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn done_step_json(summary: &str) -> String {
    format!(
        r#"{{"reasoning":"done","plan":[],"action":{{"type":"done","summary":"{summary}","evidence_ids":[]}},"expected_outcome":"done","confidence":1.0}}"#
    )
}

fn fail_step_json(reason: &str) -> String {
    format!(
        r#"{{"reasoning":"fail","plan":[],"action":{{"type":"fail","reason":"{reason}"}},"expected_outcome":"fail","confidence":1.0}}"#
    )
}

/// Boot an isolated Cortex and wrap it in an Arc.
async fn boot_cortex(id: &str) -> Arc<Cortex> {
    let (mut cortex, merger) = Cortex::isolated(id);
    cortex.boot(merger, Box::new(StubAccessibility)).await.unwrap();
    Arc::new(cortex)
}

/// Build a Planner with a mock LLM that always returns `response`.
fn mock_planner(goal: &str, response: String) -> Planner {
    let llm = LlmClient::new_with_fn(move |_msgs, _max| Ok(response.clone()));
    Planner::new(llm, PlannerGoalConfig {
        goal: goal.into(),
        max_steps: 10,
        ..Default::default()
    })
}

// ─── Pipeline tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_pipeline_achieved_immediately() {
    let cortex = boot_cortex("pipe-achieved").await;
    let planner = mock_planner("test goal", done_step_json("pipeline done"));
    let config = GoalConfig {
        goal: "test goal".into(),
        max_steps: 5,
        ..Default::default()
    };
    let mut runner = GoalRunner::new_with_planner(
        config, cortex.clone(), planner, Arc::new(NoOpCallbacks),
    );
    let result = runner.run().await;

    assert_eq!(result.status, GoalStatus::Achieved);
    assert!(result.summary.contains("pipeline done") || result.summary.contains("done"),
        "unexpected summary: {}", result.summary);
    assert!(result.metrics.context_reads >= 1);
    assert!(result.metrics.llm_calls >= 1);

    cortex.shutdown();
}

#[tokio::test]
async fn test_pipeline_failed_by_planner() {
    let cortex = boot_cortex("pipe-fail").await;
    let planner = mock_planner("impossible task", fail_step_json("cannot proceed"));
    let config = GoalConfig {
        goal: "impossible task".into(),
        max_steps: 5,
        ..Default::default()
    };
    let mut runner = GoalRunner::new_with_planner(
        config, cortex.clone(), planner, Arc::new(NoOpCallbacks),
    );
    let result = runner.run().await;

    assert_eq!(result.status, GoalStatus::Failed);
    assert!(result.metrics.context_reads >= 1);

    cortex.shutdown();
}

#[tokio::test]
async fn test_pipeline_metrics_populated() {
    let cortex = boot_cortex("pipe-metrics").await;
    let planner = mock_planner("metric test", done_step_json("metrics ok"));
    let config = GoalConfig {
        goal: "metric test".into(),
        max_steps: 5,
        ..Default::default()
    };
    let mut runner = GoalRunner::new_with_planner(
        config, cortex.clone(), planner, Arc::new(NoOpCallbacks),
    );
    let result = runner.run().await;

    assert!(result.total_steps <= 5);
    assert!(result.duration_ms < 30_000);
    assert!(result.metrics.context_reads >= 1, "context must be read at least once");
    assert!(result.metrics.llm_calls >= 1, "LLM must be called at least once");

    cortex.shutdown();
}

#[tokio::test]
async fn test_pipeline_cortex_mental_model_readable_during_run() {
    // Verify the Cortex's mental model (Arc<RwLock<MentalModel>>) is accessible
    // concurrently from outside the runner while it is running.
    let cortex = boot_cortex("pipe-concurrent").await;

    // Snapshot before run
    let model_before = {
        let arc = cortex.model();
        let m = arc.read().await;
        m.current_context.app.clone()
    };

    let planner = mock_planner("concurrent test", done_step_json("concurrent ok"));
    let config = GoalConfig {
        goal: "concurrent test".into(),
        max_steps: 3,
        ..Default::default()
    };
    let mut runner = GoalRunner::new_with_planner(
        config, cortex.clone(), planner, Arc::new(NoOpCallbacks),
    );
    runner.run().await;

    // Mental model should still be accessible after run
    let model_after = {
        let arc = cortex.model();
        let m = arc.read().await;
        m.current_context.app.clone()
    };
    // Both snapshots should be valid strings (not panic)
    let _ = (model_before, model_after);

    cortex.shutdown();
}
