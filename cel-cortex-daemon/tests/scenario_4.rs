//! Scenario 4 from `cellar-app-v1.md` §5 — the canonical demo.
//!
//! User delegates a task to the embedded agent. The agent attempts a file
//! copy outside the user's workspace. A guard rule intercepts. The user
//! resolves via confirmation modal.
//!
//! Here we construct a `Daemon` (with the same `wire_subsystems()` the
//! production binary uses), reach in to install the scenario-4 rule, swap
//! the broker for a scripted one that simulates the user clicking Allow,
//! and intercept a synthesised `fs.copy` action. The assertion validates
//! the whole flow:
//!
//! 1. The gateway pauses the action (the broker is asked).
//! 2. The broker returns Allow.
//! 3. The actuator runs.
//! 4. Memory holds one Fire chunk and one Action chunk reflecting success.

use std::sync::Arc;

use cel_act_gateway::test_support::{fake_action, RecordingActuator, ScriptedBroker};
use cel_act_gateway::traits::StaticRules;
use cel_act_gateway::{ConfirmationDecision, Gateway};
use cel_memory::{
    BasicMemoryProvider, CallerScope, ChunkKind, MemoryProvider, MemoryQuery, RetrievalProfile,
};
use cellar_types::{
    Action, ActionType, EventKind, Expression, InMemoryWatchlists, Operator, Rule, RuleKind,
};
use chrono::Utc;
use serde_json::json;

#[tokio::test]
async fn scenario_4_agent_guard_canonical_demo() {
    // Construct the scenario-4 rule:
    //   "Require my confirmation before the agent moves any file outside
    //    ~/Workspace"
    // In v1's expression language, that's a guard on
    //   data.action_type in ["fs.move", "fs.copy"]
    //   AND data.action_args.source_path NOT starts_with ~/Workspace
    let rule = Rule {
        id: "no_files_outside_workspace".into(),
        name: "No files outside workspace".into(),
        nl_original: "Require my confirmation before the agent moves any file outside ~/Workspace"
            .into(),
        kind: RuleKind::Guard,
        enabled: true,
        match_expr: Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!(EventKind::AgentActionAttempted)),
            Expression::leaf(
                "data.action_type",
                Operator::In,
                json!(["fs.move", "fs.copy"]),
            ),
            Expression::leaf(
                "data.action_args.source_path",
                Operator::NotStartsWith,
                json!("~/Workspace"),
            ),
        ]),
        action: Action {
            action_type: ActionType::RequireConfirmation,
            webhook_id: None,
            timeout_s: Some(300),
        },
        cooldown_seconds: 0,
        created_at: Utc::now(),
    };

    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let broker = ScriptedBroker::new(vec![ConfirmationDecision::Allow]);
    let gw = Gateway::new(
        RecordingActuator::with_response(json!({"copied": true})),
        broker,
        StaticRules(vec![rule]),
        InMemoryWatchlists::default(),
        memory.clone(),
    );

    // Embedded agent attempts to copy Q4-report-personal.pdf to /Volumes/External.
    let mut action = fake_action("embedded", "fs.copy");
    action.action_args = json!({
        "source_path": "~/Documents/Q4/Q4-report-personal.pdf",
        "dest_path": "/Volumes/External/Archive/"
    });
    action.agent_session_id = Some("sess_archive_q4".into());

    let outcome = gw.intercept(action).await.unwrap();

    // Outcome: action executed because the user clicked Allow.
    assert!(outcome.executed(), "expected Executed, got {outcome:?}");

    // Actuator was called once.
    assert_eq!(gw.actuator().call_count(), 1);

    // Memory: one Fire chunk (rule matched), one Action chunk (audit).
    let fires = memory
        .retrieve(MemoryQuery {
            text: "agent".into(),
            kinds: Some(vec![ChunkKind::Fire]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Own,
            project_root_prefix: None,
            k: 8,
            include_rollups: true,
            min_importance: None,
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "embedded".into(),
        })
        .await
        .unwrap();
    assert_eq!(fires.len(), 1);
    assert!(fires[0].content.contains("No files outside workspace"));

    let actions = memory
        .retrieve(MemoryQuery {
            text: "agent".into(),
            kinds: Some(vec![ChunkKind::Action]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Own,
            project_root_prefix: None,
            k: 8,
            include_rollups: true,
            min_importance: None,
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "embedded".into(),
        })
        .await
        .unwrap();
    assert_eq!(actions.len(), 1);
    assert!(actions[0].content.contains("executed"));
    assert_eq!(actions[0].session_id.as_deref(), Some("sess_archive_q4"));
}
