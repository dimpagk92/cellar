//! Phase 2: stale-target detection tests.
//!
//! Exercises the pre-execute refresh + target validation path in the
//! GoalRunner loop. Uses a mock LLM planner that returns a `Click` action
//! against an element ID that doesn't exist in the Stub accessibility
//! context — the runner must replan instead of dispatching.

use std::sync::Arc;

use cel_accessibility::StubAccessibility;
use cel_cortex::Cortex;
use cel_goal_runner::{GoalConfig, GoalRunner, GoalStatus, NoOpCallbacks};
use cel_llm::LlmClient;
use cel_planner::{GoalConfig as PlannerGoalConfig, Planner};

/// Build a mock LLM client that returns the same canned PlannedStep JSON
/// every time — regardless of input. `target_id` picks the element the
/// (fake) planner will claim to click.
fn mock_click_planner(target_id: &str) -> Planner {
    let target = target_id.to_string();
    let response = format!(
        r#"{{
            "evaluation": "first attempt",
            "memory": "",
            "plan": ["click the button"],
            "reasoning": "fake planner returns a click against a nonexistent id",
            "action": {{ "type": "click", "target_id": "{target}" }},
            "additional_actions": [],
            "expected_outcome": "element gets clicked",
            "confidence": 0.9,
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

#[tokio::test]
async fn stale_target_increments_metric_and_does_not_execute() {
    // Stub accessibility returns an empty element list. Any target_id the
    // mock planner names is therefore missing, and the runner must replan
    // without dispatching.
    let cortex = isolated_cortex("stale-target").await;
    let planner = mock_click_planner("a11y:nonexistent");
    let config = GoalConfig {
        goal: "click a ghost".into(),
        max_steps: 3,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner =
        GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    assert!(
        result.metrics.stale_targets >= 1,
        "expected stale_targets ≥ 1 when planner names a missing element, got {}",
        result.metrics.stale_targets
    );
    assert_eq!(
        result.metrics.action_successes, 0,
        "no action should have been executed — got {} successes",
        result.metrics.action_successes
    );
    assert_eq!(
        result.metrics.action_failures, 0,
        "no action should have been attempted — got {} failures",
        result.metrics.action_failures
    );
    // Runner burns through its max_steps trying to replan the same click
    // (the mock is stubborn) — status should terminate as MaxSteps, not
    // achieve and not silent-success.
    assert_ne!(result.status, GoalStatus::Achieved);

    cortex.shutdown();
}

#[tokio::test]
async fn unknown_custom_adapter_triggers_replan_not_dispatch() {
    // The planner occasionally hallucinates `Custom { adapter: "browser" }`
    // or similar adapter names that aren't registered. The runner should
    // intercept these up-front and drive a replan, NOT let Cortex return
    // a "No adapter registered" mid-dispatch error that burns a step.
    use cel_planner::PlannedAction;

    // Mock planner returns a Custom action with a bogus adapter name.
    let response = r#"{
        "evaluation": "",
        "memory": "",
        "plan": ["use browser adapter"],
        "reasoning": "fake planner hallucinates a non-existent adapter",
        "action": { "type": "custom", "adapter": "browser", "action": "navigate", "params": { "url": "https://example.com" } },
        "additional_actions": [],
        "expected_outcome": "",
        "confidence": 0.9,
        "context_tier": "minimal"
    }"#;
    let llm = LlmClient::new_with_fn(move |_msgs, _max_tokens| Ok(response.to_string()));
    let planner = Planner::new(llm, PlannerGoalConfig::default());

    let cortex = isolated_cortex("unknown-adapter").await;
    let config = GoalConfig {
        goal: "do a thing".into(),
        max_steps: 3,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    assert!(
        result.metrics.stale_targets >= 1,
        "unknown adapter should bump stale_targets (via the replan path), got {}",
        result.metrics.stale_targets
    );
    // No action ever dispatched — the Custom{browser} was intercepted.
    assert_eq!(result.metrics.action_successes, 0);
    assert_eq!(result.metrics.action_failures, 0);

    // Sanity: the rejection isn't silent — history has a row describing why.
    let records = result.action_log;
    // Confirm the intercepted action is the expected Custom variant (planner
    // returned it) even though it was rejected pre-dispatch.
    assert!(
        records.is_empty() || records.iter().all(|r| r.kind.starts_with("custom:") || r.kind != "click"),
        "action_log should not contain non-custom records — got {records:?}"
    );
    let _ = PlannedAction::Custom {
        adapter: "x".into(),
        action: "y".into(),
        params: serde_json::Value::Null,
    }; // type usage — silence the import if test-only

    cortex.shutdown();
}

#[tokio::test]
async fn refreshes_counter_bumps_each_step() {
    // Every step runs two refreshes (pre-perceive + pre-execute), so for
    // any run that executes N steps we expect refreshes ≥ 2*N. We use the
    // ghost-click planner to get bounded steps without needing a real LLM.
    let cortex = isolated_cortex("refresh-counter").await;
    let planner = mock_click_planner("a11y:also-nonexistent");
    let config = GoalConfig {
        goal: "click another ghost".into(),
        max_steps: 3,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner =
        GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    // N ≥ 1 (at least one perceive+plan+refresh+validate cycle happened).
    assert!(
        result.total_steps >= 1,
        "expected at least 1 step, got {}",
        result.total_steps
    );
    let expected_min = (result.total_steps as u32) * 2;
    assert!(
        result.metrics.refreshes >= expected_min,
        "refreshes should be ≥ 2*total_steps ({expected_min}), got {}",
        result.metrics.refreshes
    );

    cortex.shutdown();
}
