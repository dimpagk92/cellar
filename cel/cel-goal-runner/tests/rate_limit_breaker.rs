//! Phase 5.1: rate-limit circuit breaker.
//!
//! Simulates an LLM that always returns HTTP 429 (rate limit exhausted
//! even after the client's internal 1s/2s/4s backoff). The runner must
//! fail the goal after `max_consecutive_rate_limits` hits, not chew
//! through `max_consecutive_failures` × retry-latency.

use std::sync::Arc;

use cel_accessibility::StubAccessibility;
use cel_cortex::Cortex;
use cel_goal_runner::{GoalConfig, GoalRunner, GoalStatus, NoOpCallbacks};
use cel_llm::{LlmClient, LlmError};
use cel_planner::{GoalConfig as PlannerGoalConfig, Planner};

fn always_rate_limited_planner() -> Planner {
    let llm = LlmClient::new_with_fn(|_msgs, _max_tokens| {
        Err(LlmError::HttpError {
            status: 429,
            body: "{\"error\":{\"type\":\"rate_limit_error\"}}".into(),
        })
    });
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
async fn repeated_429s_trip_circuit_breaker_before_max_steps() {
    // With max_consecutive_rate_limits=3 (default), a non-terminating 429
    // stream must stop the goal at roughly step 3 — NOT step 30 (default
    // max_steps) or step 8 (default max_consecutive_failures).
    let cortex = isolated_cortex("rate-limit-breaker").await;
    let planner = always_rate_limited_planner();
    let config = GoalConfig {
        goal: "a goal that isn't a URL".into(), // avoid deterministic fast path
        max_steps: 30,
        max_consecutive_failures: 8,
        max_consecutive_rate_limits: 3,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    assert_eq!(result.status, GoalStatus::Failed);
    assert!(
        result.summary.contains("rate-limited") || result.summary.contains("429"),
        "summary should name the rate-limit cause, got: {:?}",
        result.summary
    );
    assert!(
        result.total_steps <= 5,
        "breaker should trip within ~3 steps, got {}",
        result.total_steps
    );

    cortex.shutdown();
}

#[tokio::test]
async fn non_rate_limit_errors_do_not_trip_the_breaker() {
    // The breaker is specific to 429/529. A generic `RequestFailed` error
    // should flow through the existing `max_consecutive_failures` gate
    // (default 8) — not the rate-limit gate (default 3).
    let llm = LlmClient::new_with_fn(|_msgs, _max_tokens| {
        Err(LlmError::RequestFailed("generic failure".into()))
    });
    let planner = Planner::new(llm, PlannerGoalConfig::default());
    let cortex = isolated_cortex("generic-error").await;
    let config = GoalConfig {
        goal: "not a url either".into(),
        max_steps: 30,
        max_consecutive_failures: 5,
        max_consecutive_rate_limits: 3,
        ..Default::default()
    };
    let callbacks = Arc::new(NoOpCallbacks);
    let mut runner = GoalRunner::new_with_planner(config, cortex.clone(), planner, callbacks);
    let result = runner.run().await;

    assert_eq!(result.status, GoalStatus::Failed);
    // With non-429s, the consecutive_failures gate (5) must fire before
    // anything mentioning rate limits.
    assert!(
        !result.summary.contains("rate-limited"),
        "non-429 errors should not mention rate-limiting, got: {:?}",
        result.summary
    );
    assert!(
        result.summary.contains("planning failures") || result.summary.contains("plan"),
        "expected generic planning-failure message, got: {:?}",
        result.summary
    );

    cortex.shutdown();
}
