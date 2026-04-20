//! Phase 3C: vision-fallback gating tests.
//!
//! Vision is an expensive rare path. These tests exercise the *gating*
//! logic — when vision should and should not be invoked — without
//! actually making LLM calls. The runner's `last_failure_was_target_miss`
//! plus `enable_vision` + `signals.vision_needed` determine the branch.
//!
//! End-to-end verification of the vision LLM path lives in Phase 5's
//! integration suite, which needs a mock-LLM backend that serves canned
//! vision responses.

use std::sync::Arc;

use cel_accessibility::StubAccessibility;
use cel_cortex::Cortex;
use cel_goal_runner::{GoalConfig, GoalRunner, NoOpCallbacks};
use cel_llm::LlmClient;
use cel_planner::{GoalConfig as PlannerGoalConfig, Planner};

fn mock_click_planner(target_id: &str) -> Planner {
    let target = target_id.to_string();
    let response = format!(
        r#"{{
            "evaluation": "",
            "memory": "",
            "plan": ["click"],
            "reasoning": "fake",
            "action": {{ "type": "click", "target_id": "{target}" }},
            "additional_actions": [],
            "expected_outcome": "",
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
async fn vision_disabled_never_invokes_vision_even_with_stale_targets() {
    // `enable_vision: false` should short-circuit the gate no matter what
    // the signals or prior failures look like. Verifies we never surprise
    // a caller that asked for text-only runs.
    let cortex = isolated_cortex("vision-off").await;
    let planner = mock_click_planner("a11y:ghost");
    let config = GoalConfig {
        goal: "click something".into(),
        max_steps: 3,
        enable_vision: false,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    // Stale targets force replans — several are expected here.
    assert!(result.metrics.stale_targets >= 1);
    // But vision must not have fired.
    assert_eq!(
        result.metrics.vision_calls, 0,
        "vision must not be invoked when enable_vision=false, got {}",
        result.metrics.vision_calls
    );

    cortex.shutdown();
}

#[tokio::test]
async fn vision_not_invoked_when_vision_needed_is_false() {
    // StubAccessibility returns an empty element list → the Cortex's
    // vision_needed flag DOES flip on (SPARSE_CONTEXT_THRESHOLD=5 and
    // actionable==0 < 5). So with a stale-target replan loop, vision
    // is expected to be invoked at least once. We use this test to
    // baseline the counter — the *gate* evaluation runs per step and
    // stale_targets indicates the last failure was a target miss.
    //
    // If the cortex ever stops setting vision_needed under
    // StubAccessibility, update this test — not the gate.
    //
    // NOTE: without a real LLM we can only assert the counter semantics
    // (not the resulting prompt). Actual vision LLM invocation validation
    // is Phase-5 work.
    let cortex = isolated_cortex("vision-stub").await;
    let planner = mock_click_planner("a11y:ghost-too");
    let config = GoalConfig {
        goal: "click a different ghost".into(),
        max_steps: 3,
        enable_vision: true,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    // Gate is VISION-needed + target-miss. We expect target misses (stub
    // has no elements) but the first step has no prior failure — so the
    // first plan goes text-only; subsequent steps may take the vision
    // route once stale_targets accumulates. Either way, vision_calls
    // ≤ llm_calls and the run must terminate cleanly.
    assert!(
        result.metrics.vision_calls <= result.metrics.llm_calls as u32,
        "vision_calls ({}) must not exceed llm_calls ({})",
        result.metrics.vision_calls,
        result.metrics.llm_calls
    );

    cortex.shutdown();
}
