//! Phase 3A + 3B end-to-end prompt-capture tests.
//!
//! Verifies that populated `CortexSignals` and the rolling-memory block
//! *actually reach the LLM* — not just that the prompt builder can emit
//! them in isolation. Mocks the LLM with a capturing closure, drives
//! `Planner::plan_step` with realistic inputs, and asserts the captured
//! user-role message body contains the expected sections.
//!
//! These are the first tests in the suite to touch the full
//! prompt-builder → plan_step → ChatMessage content path in one run.

use std::sync::{Arc, Mutex};

use cel_context::ScreenContext;
use cel_llm::{ChatMessage, ContentPart, LlmClient, LlmError};
use cel_planner::{
    history::StepHistory, CortexSignals, GoalConfig, LoadingSignal, Planner, PlannerBackend,
    PlannerError, PlannerEvent,
};

// ─── Helpers ────────────────────────────────────────────────────────────────

type Captured = Arc<Mutex<Vec<ChatMessage>>>;

/// Build a planner whose LLM capture every incoming `Vec<ChatMessage>`
/// and returns a canned "Done" response so the planner parses cleanly.
fn capturing_planner(goal: &str) -> (Planner, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_fn = Arc::clone(&captured);
    let llm = LlmClient::new_with_fn(move |msgs, _max_tokens| {
        captured_for_fn.lock().unwrap().extend(msgs);
        Ok(r#"{
            "evaluation": "",
            "memory": "",
            "plan": ["done"],
            "reasoning": "mock",
            "action": { "type": "done", "summary": "ok" },
            "additional_actions": [],
            "expected_outcome": "",
            "confidence": 1.0,
            "context_tier": "minimal"
        }"#
        .to_string())
    });
    let cfg = GoalConfig {
        goal: goal.into(),
        ..Default::default()
    };
    (Planner::new(llm, cfg), captured)
}

fn empty_context() -> ScreenContext {
    ScreenContext {
        app: "TestApp".into(),
        window: "TestWindow".into(),
        elements: vec![],
        network_events: vec![],
        timestamp_ms: 0,
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: vec![],
        audio: None,
        power: None,
        running_apps: vec![],
        recent_files: vec![],
        http_events: vec![],
        transcripts: vec![],
    }
}

/// No-op backend — plan_step doesn't actually use it for perception/exec.
struct NoopBackend;

#[async_trait::async_trait]
impl PlannerBackend for NoopBackend {
    async fn get_context(&self) -> Result<ScreenContext, PlannerError> {
        Ok(empty_context())
    }
    async fn execute(&self, _action: &cel_planner::PlannedAction) -> Result<bool, PlannerError> {
        Ok(true)
    }
    fn on_event(&self, _event: PlannerEvent) {}
}

/// Return the first user-role text content the LLM received.
fn user_prompt_text(captured: &Captured) -> String {
    let guard = captured.lock().unwrap();
    for msg in guard.iter() {
        if msg.role == "user" {
            for part in &msg.content {
                if let ContentPart::Text { text } = part {
                    return text.clone();
                }
            }
        }
    }
    panic!(
        "no user-role text message captured; got {} messages",
        guard.len()
    );
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn signals_reach_llm_prompt_body() {
    // A populated CortexSignals must render as a "## Perception signals"
    // section in the prompt the LLM actually receives — verifying that
    // plan_step wires Options.cortex_signals → build_user_prompt → LLM
    // input end-to-end.
    let (planner, captured) = capturing_planner("verify signals");
    let ctx = empty_context();
    let signals = CortexSignals {
        confidence: 0.77,
        vision_needed: true,
        loading: Some(LoadingSignal { duration_ms: 900 }),
        stable_count: 9,
        volatile_ids: vec!["a11y:vol-1".into(), "a11y:vol-2".into()],
        anomalies: vec!["dialog: Cookie Consent".into()],
        tick_age_ms: Some(150),
    };

    let _ = planner
        .plan_step(
            "system",
            &ctx,
            &signals,
            "",
            &StepHistory::new(),
            0,
            &None,
            &NoopBackend,
        )
        .await
        .expect("plan_step");

    let prompt = user_prompt_text(&captured);
    assert!(prompt.contains("## Perception signals"));
    assert!(prompt.contains("Confidence: 0.77"));
    assert!(prompt.contains("Context age: 150ms"));
    assert!(prompt.contains("Loading detected (900ms)"));
    assert!(prompt.contains("Stable elements: 9"));
    assert!(prompt.contains("a11y:vol-1"));
    assert!(prompt.contains("Cookie Consent") || prompt.contains("cookie consent"));
    assert!(prompt.contains("vision fallback"));
}

#[tokio::test]
async fn default_signals_do_not_bloat_the_prompt() {
    // Inverse guarantee: callers passing `CortexSignals::default()` (the
    // "nothing to say" case) must NOT get a Perception signals section
    // in their outgoing prompt — we don't want to spend tokens on empty
    // bullets just because the parameter exists.
    let (planner, captured) = capturing_planner("verify default signals");
    let ctx = empty_context();
    let signals = CortexSignals::default();

    let _ = planner
        .plan_step(
            "system",
            &ctx,
            &signals,
            "",
            &StepHistory::new(),
            0,
            &None,
            &NoopBackend,
        )
        .await
        .expect("plan_step");

    let prompt = user_prompt_text(&captured);
    assert!(
        !prompt.contains("## Perception signals"),
        "default signals must not render a section; got prompt:\n{prompt}"
    );
}

#[tokio::test]
async fn memory_block_reaches_llm_prompt_body() {
    // Phase 3B: the pre-rendered three-lens memory block handed to
    // plan_step should show up verbatim in the LLM input. No post-
    // processing that might drop it.
    let (planner, captured) = capturing_planner("verify memory");
    let ctx = empty_context();
    let signals = CortexSignals::default();
    let memory_block = "## Recent runs on this machine\n\
        ### This cortex\n\
        - 5m ago: \"prior successful goal\" — achieved in 4 steps\n\
        ### Other cortexes\n\
        - 2h ago: \"sibling run\" — rate_limit_error after 3 steps\n";

    let _ = planner
        .plan_step(
            "system",
            &ctx,
            &signals,
            memory_block,
            &StepHistory::new(),
            0,
            &None,
            &NoopBackend,
        )
        .await
        .expect("plan_step");

    let prompt = user_prompt_text(&captured);
    assert!(prompt.contains("## Recent runs on this machine"));
    assert!(prompt.contains("prior successful goal"));
    assert!(prompt.contains("achieved in 4 steps"));
    assert!(prompt.contains("sibling run"));
    assert!(prompt.contains("rate_limit_error after 3 steps"));
}

#[tokio::test]
async fn signals_and_memory_coexist_in_a_single_prompt() {
    // Realistic path: both sections populated at once. Verifies they
    // don't stomp each other and both survive the prompt assembly.
    let (planner, captured) = capturing_planner("verify both");
    let ctx = empty_context();
    let signals = CortexSignals {
        confidence: 0.9,
        stable_count: 3,
        ..Default::default()
    };
    let memory_block = "## Recent runs on this machine\n- 1m ago: \"foo\" — achieved in 2 steps\n";

    let _ = planner
        .plan_step(
            "system",
            &ctx,
            &signals,
            memory_block,
            &StepHistory::new(),
            0,
            &None,
            &NoopBackend,
        )
        .await
        .expect("plan_step");

    let prompt = user_prompt_text(&captured);
    assert!(prompt.contains("## Perception signals"));
    assert!(prompt.contains("Stable elements: 3"));
    assert!(prompt.contains("## Recent runs on this machine"));
    assert!(prompt.contains("foo"));
    // Order matters for comprehensibility — signals before memory.
    let sig_pos = prompt.find("## Perception signals").unwrap();
    let mem_pos = prompt.find("## Recent runs on this machine").unwrap();
    assert!(
        sig_pos < mem_pos,
        "Perception signals should appear before Recent runs in the prompt"
    );
}

#[tokio::test]
async fn vision_path_sends_image_and_ground_truth_hint() {
    // Phase 3C: `plan_step_with_vision` must (a) attach the supplied
    // data URL as an `ImageUrl` ContentPart and (b) append the "vision
    // ground truth" hint to the user prompt so the LLM knows the
    // screenshot is authoritative.
    let (planner, captured) = capturing_planner("verify vision");
    let ctx = empty_context();
    let signals = CortexSignals {
        vision_needed: true,
        ..Default::default()
    };
    let fake_image = "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQ=="; // not a real image — we only care the URL survives

    let _ = planner
        .plan_step_with_vision(
            "system",
            &ctx,
            &signals,
            "",
            &StepHistory::new(),
            0,
            &None,
            fake_image,
        )
        .await
        .expect("plan_step_with_vision");

    let guard = captured.lock().unwrap();
    let user_msg = guard
        .iter()
        .find(|m| m.role == "user")
        .expect("expected user-role message");

    // (a) image part survived
    let has_image = user_msg
        .content
        .iter()
        .any(|p| matches!(p, ContentPart::ImageUrl { image_url } if image_url.url == fake_image));
    assert!(
        has_image,
        "expected ImageUrl ContentPart with the supplied data URL"
    );

    // (b) text part carries the ground-truth hint
    let text_body = user_msg
        .content
        .iter()
        .find_map(|p| match p {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("expected Text ContentPart in the same message");
    assert!(
        text_body.contains("Vision ground truth"),
        "vision prompt should include the ground-truth hint section"
    );
    assert!(
        text_body.contains("trust the screenshot"),
        "vision prompt should instruct the LLM to trust the image when it conflicts with the AX tree"
    );
}

#[tokio::test]
async fn non_rate_limit_llm_error_is_propagated_cleanly() {
    // Sanity check on the mock — a planner failure surfaces as the
    // LlmError variant the closure returns, not a generic parse error.
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_fn = Arc::clone(&captured);
    let llm = LlmClient::new_with_fn(move |msgs, _max_tokens| {
        captured_for_fn.lock().unwrap().extend(msgs);
        Err(LlmError::RequestFailed("boom".into()))
    });
    let planner = Planner::new(
        llm,
        GoalConfig {
            goal: "x".into(),
            ..Default::default()
        },
    );
    let err = planner
        .plan_step(
            "system",
            &empty_context(),
            &CortexSignals::default(),
            "",
            &StepHistory::new(),
            0,
            &None,
            &NoopBackend,
        )
        .await
        .expect_err("should propagate LLM error");
    match err {
        PlannerError::Llm(LlmError::RequestFailed(msg)) => assert_eq!(msg, "boom"),
        other => panic!("expected Llm(RequestFailed), got {other:?}"),
    }
    // The LLM was still called, so our capture isn't empty.
    assert!(!captured.lock().unwrap().is_empty());
}
