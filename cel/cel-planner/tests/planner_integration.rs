//! Integration tests for the Planner using LlmClient::new_with_fn().
//!
//! These tests verify the full parse→plan→step pipeline without network calls.
//! The mock LLM returns canned JSON responses, exercising the real prompt
//! builder, JSON parser, and step validator.
#![allow(deprecated)]

use async_trait::async_trait;
use cel_context::ScreenContext;
use cel_llm::LlmClient;
use cel_planner::{GoalConfig, PlannedAction, Planner, PlannerBackend, PlannerError, PlannerEvent};

// ─── Minimal PlannerBackend for tests ────────────────────────────────────────

struct MockBackend {
    context: ScreenContext,
}

impl MockBackend {
    fn empty() -> Self {
        Self {
            context: ScreenContext {
                app: "TestApp".into(),
                window: "Main".into(),
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
            },
        }
    }
}

#[async_trait]
impl PlannerBackend for MockBackend {
    async fn get_context(&self) -> Result<ScreenContext, PlannerError> {
        Ok(self.context.clone())
    }
    async fn execute(&self, _action: &PlannedAction) -> Result<bool, PlannerError> {
        Ok(true)
    }
    fn on_event(&self, _event: PlannerEvent) {}
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Valid PlannedStep JSON with a Done action — signals goal achieved.
fn done_response(summary: &str) -> String {
    format!(
        r#"{{"reasoning":"Goal complete","plan":[],"action":{{"type":"done","summary":"{summary}","evidence_ids":[]}},"expected_outcome":"done","confidence":1.0}}"#
    )
}

/// Valid PlannedStep JSON with a Click action.
fn click_response(target_id: &str) -> String {
    format!(
        r#"{{"reasoning":"Clicking","plan":[],"action":{{"type":"click","target_id":"{target_id}"}},"expected_outcome":"clicked","confidence":0.9}}"#
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_planner_mock_done_immediately() {
    let llm = LlmClient::new_with_fn(|_msgs, _max| Ok(done_response("task complete")));
    let config = GoalConfig {
        goal: "do something".into(),
        max_steps: 5,
        ..Default::default()
    };
    let planner = Planner::new(llm, config);
    let backend = MockBackend::empty();
    let result = planner.run(&backend).await.unwrap();
    assert!(
        matches!(result, PlannerEvent::GoalAchieved { .. }),
        "expected GoalAchieved, got {:?}",
        result
    );
}

#[tokio::test]
async fn test_planner_mock_click_then_done() {
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let call_count2 = call_count.clone();
    let llm = LlmClient::new_with_fn(move |_msgs, _max| {
        let n = call_count2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n == 0 {
            Ok(click_response("dom:btn:1"))
        } else {
            Ok(done_response("clicked and done"))
        }
    });
    let config = GoalConfig {
        goal: "click the button".into(),
        max_steps: 5,
        ..Default::default()
    };
    let planner = Planner::new(llm, config);
    let backend = MockBackend::empty();
    let result = planner.run(&backend).await.unwrap();
    assert!(matches!(result, PlannerEvent::GoalAchieved { .. }));
    assert!(call_count.load(std::sync::atomic::Ordering::Relaxed) >= 2);
}

#[tokio::test]
async fn test_planner_plan_step_returns_valid_step() {
    let llm = LlmClient::new_with_fn(|_msgs, _max| Ok(click_response("a11y:42")));
    let config = GoalConfig {
        goal: "click something".into(),
        max_steps: 5,
        ..Default::default()
    };
    let planner = Planner::new(llm, config);
    let backend = MockBackend::empty();
    let ctx = backend.get_context().await.unwrap();
    let history = cel_planner::history::StepHistory::new();
    let system = cel_planner::prompt::system_prompt();
    let signals = cel_planner::CortexSignals::default();
    let step = planner
        .plan_step(&system, &ctx, &signals, "", &history, 0, &None, &backend)
        .await
        .unwrap();
    assert!(matches!(step.action, PlannedAction::Click { .. }));
    assert!(step.confidence > 0.0);
}

#[tokio::test]
async fn test_planner_handles_malformed_json_with_retry() {
    // First call returns garbage; second call returns valid JSON.
    // Verifies the retry-on-parse-failure logic in call_llm_with_retries.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let call_count2 = call_count.clone();
    let llm = LlmClient::new_with_fn(move |_msgs, _max| {
        let n = call_count2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n == 0 {
            Ok("not valid json at all !!!".into())
        } else {
            Ok(done_response("recovered"))
        }
    });
    let config = GoalConfig {
        goal: "test retry".into(),
        max_steps: 5,
        max_retries: 3,
        ..Default::default()
    };
    let planner = Planner::new(llm, config);
    let backend = MockBackend::empty();
    let result = planner.run(&backend).await.unwrap();
    assert!(matches!(result, PlannerEvent::GoalAchieved { .. }));
    assert!(
        call_count.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "should have retried after parse failure"
    );
}

#[tokio::test]
async fn test_planner_fails_after_exhausting_retries() {
    let llm = LlmClient::new_with_fn(|_msgs, _max| Ok("not json".into()));
    let config = GoalConfig {
        goal: "test fail".into(),
        max_steps: 2,
        max_retries: 2,
        ..Default::default()
    };
    let planner = Planner::new(llm, config);
    let backend = MockBackend::empty();
    let result = planner.run(&backend).await;
    assert!(result.is_err(), "should fail after exhausting retries");
}
