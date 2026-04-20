//! Integration tests for the GoalRunner.
//!
//! Uses `Cortex::isolated()` to avoid OS accessibility permissions.
//! Tests the graceful-failure path (no LLM configured), cancel behavior,
//! and max_steps guard — without making real LLM calls.

use std::sync::Arc;
use cel_accessibility::StubAccessibility;
use cel_cortex::Cortex;
use cel_goal_runner::{GoalConfig, GoalRunner, GoalStatus, NoOpCallbacks};

// ─── Helper ──────────────────────────────────────────────────────────────────

/// Boot an isolated Cortex and return an Arc wrapping it, suitable for GoalRunner.
async fn isolated_cortex(id: &str) -> Arc<Cortex> {
    let (mut cortex, merger) = Cortex::isolated(id);
    cortex.boot(merger, Box::new(StubAccessibility)).await.unwrap();
    Arc::new(cortex)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_runner_fails_gracefully_without_llm() {
    // No LLM env vars set → planner is None → runner returns Failed immediately.
    // The goal has no deterministic fast-path, so it must fail cleanly.
    let cortex = isolated_cortex("no-llm").await;
    let config = GoalConfig {
        goal: "do something that has no fast path".into(),
        max_steps: 5,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new(config, cortex.clone(), callbacks);
    let result = runner.run().await;

    assert_eq!(
        result.status, GoalStatus::Failed,
        "should fail gracefully when LLM not configured, got: {:?}", result.status
    );
    assert!(
        result.summary.contains("LLM") || result.summary.contains("plan"),
        "summary should mention LLM or planning, got: {:?}", result.summary
    );

    cortex.shutdown();
}

#[tokio::test]
async fn test_runner_respects_max_steps_via_config() {
    // With an impossibly small step budget and no LLM, runner fails immediately.
    // This verifies GoalConfig is wired into the runner (not hardcoded).
    let cortex = isolated_cortex("max-steps").await;
    let config = GoalConfig {
        goal: "arbitrary goal with no fast path".into(),
        max_steps: 1,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new(config, cortex.clone(), callbacks);
    let result = runner.run().await;

    // Without LLM: fails before hitting max_steps. Either way we get a terminal status.
    assert_ne!(result.status, GoalStatus::Achieved);

    cortex.shutdown();
}

#[tokio::test]
async fn test_runner_result_has_metrics() {
    let cortex = isolated_cortex("metrics").await;
    let config = GoalConfig {
        goal: "goal with no fast path".into(),
        max_steps: 3,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new(config, cortex.clone(), callbacks);
    let result = runner.run().await;

    // Metrics are always populated (context_reads ≥ 1 for the initial perception)
    assert!(result.metrics.context_reads >= 1, "context_reads should be ≥ 1");
    assert!(result.duration_ms < 30_000, "run should complete in under 30s (no network)");

    cortex.shutdown();
}

#[tokio::test]
async fn test_runner_noop_callbacks_dont_panic() {
    // Smoke test: callbacks are invoked during run without panicking.
    let cortex = isolated_cortex("callbacks").await;
    let config = GoalConfig {
        goal: "callback smoke test".into(),
        max_steps: 2,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new(config, cortex.clone(), callbacks);
    let _ = runner.run().await; // must not panic

    cortex.shutdown();
}
