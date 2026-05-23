//! Integration tests for the `cel_act` gateway.
//!
//! These exercise each decision path end-to-end: pass-through, audit-only,
//! require-confirmation (allow / deny / timeout), veto, and the precedence
//! rules between matching actions of different kinds. Each path also
//! verifies the memory audit trail.

use std::sync::Arc;

use cel_act_gateway::test_support::{
    fake_action, AutoAllowBroker, AutoDenyBroker, RecordingActuator, ScriptedBroker,
};
use cel_act_gateway::{ActionOutcome, ConfirmationDecision, Gateway};
use cel_memory::{
    BasicMemoryProvider, CallerScope, ChunkKind, MemoryProvider, MemoryQuery, RetrievalProfile,
};
use cellar_types::{
    Action, ActionType, EventKind, Expression, InMemoryWatchlists, Operator, Rule, RuleKind,
};
use chrono::Utc;
use serde_json::json;

use cel_act_gateway::traits::StaticRules;

fn rule(id: &str, expr: Expression, action: Action) -> Rule {
    Rule {
        id: id.into(),
        name: id.into(),
        nl_original: format!("rule {id}"),
        kind: RuleKind::Watcher,
        enabled: true,
        match_expr: expr,
        action,
        cooldown_seconds: 0,
        created_at: Utc::now(),
    }
}

fn act_attempted_for(action_type: &str) -> Expression {
    Expression::all(vec![
        Expression::leaf("kind", Operator::Eq, json!(EventKind::AgentActionAttempted)),
        Expression::leaf("data.action_type", Operator::Eq, json!(action_type)),
    ])
}

async fn count_chunks_by_kind(
    memory: &Arc<dyn MemoryProvider>,
    caller: &str,
    kind: ChunkKind,
) -> usize {
    let hits = memory
        .retrieve(MemoryQuery {
            text: "agent".into(), // matches the gateway's chunk content
            kinds: Some(vec![kind]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Own,
            project_root_prefix: None,
            k: 100,
            include_rollups: true,
            min_importance: None,
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: caller.into(),
        })
        .await
        .unwrap();
    hits.len()
}

#[tokio::test]
async fn no_match_passes_through_and_audits() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let actuator = RecordingActuator::with_response(json!({"ok": true}));
    let gw = Gateway::new(
        actuator,
        AutoAllowBroker,
        StaticRules(vec![]),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();

    assert!(outcome.executed());
    assert_eq!(gw_actuator_calls(&gw), 1);
    // No fire chunks, one action chunk.
    assert_eq!(
        count_chunks_by_kind(&memory, "embedded", ChunkKind::Fire).await,
        0
    );
    assert_eq!(
        count_chunks_by_kind(&memory, "embedded", ChunkKind::Action).await,
        1
    );
}

#[tokio::test]
async fn audit_only_match_passes_through_and_records_fire() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "audit_copy",
        act_attempted_for("fs.copy"),
        Action {
            action_type: ActionType::LogOnly,
            webhook_id: None,
            timeout_s: None,
        },
    )];
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoAllowBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    assert!(outcome.executed());
    // Action ran; one fire recorded.
    assert_eq!(
        count_chunks_by_kind(&memory, "embedded", ChunkKind::Fire).await,
        1
    );
    assert_eq!(
        count_chunks_by_kind(&memory, "embedded", ChunkKind::Action).await,
        1
    );
}

#[tokio::test]
async fn veto_blocks_action_and_audits() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "ban_shell",
        act_attempted_for("shell.run"),
        Action {
            action_type: ActionType::Veto,
            webhook_id: None,
            timeout_s: None,
        },
    )];
    let actuator = RecordingActuator::new();
    let gw = Gateway::new(
        actuator,
        AutoAllowBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("mcp:cursor", "shell.run"))
        .await
        .unwrap();
    match outcome {
        ActionOutcome::Vetoed {
            rule_id,
            soft_block,
            ..
        } => {
            assert_eq!(rule_id, "ban_shell");
            assert!(!soft_block);
        }
        other => panic!("expected Vetoed, got {other:?}"),
    }
    assert_eq!(gw_actuator_calls(&gw), 0);
    assert_eq!(
        count_chunks_by_kind(&memory, "mcp:cursor", ChunkKind::Fire).await,
        1
    );
    assert_eq!(
        count_chunks_by_kind(&memory, "mcp:cursor", ChunkKind::Action).await,
        1
    );
}

#[tokio::test]
async fn soft_block_carries_flag() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "soft_ban",
        act_attempted_for("fs.move"),
        Action {
            action_type: ActionType::SoftBlock,
            webhook_id: None,
            timeout_s: None,
        },
    )];
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoAllowBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.move"))
        .await
        .unwrap();
    match outcome {
        ActionOutcome::Vetoed { soft_block, .. } => assert!(soft_block),
        other => panic!("expected Vetoed (soft_block), got {other:?}"),
    }
}

#[tokio::test]
async fn require_confirmation_allow_executes() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "no_files_out_of_workspace",
        act_attempted_for("fs.copy"),
        Action {
            action_type: ActionType::RequireConfirmation,
            webhook_id: None,
            timeout_s: Some(60),
        },
    )];
    let gw = Gateway::new(
        RecordingActuator::with_response(json!({"copied": true})),
        AutoAllowBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    assert!(outcome.executed());
    assert_eq!(
        count_chunks_by_kind(&memory, "embedded", ChunkKind::Fire).await,
        1
    );
    // The audit chunk reflects success.
    let hits = memory
        .retrieve(MemoryQuery {
            text: "executed".into(),
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
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn require_confirmation_deny_returns_denied() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "no_files_out_of_workspace",
        act_attempted_for("fs.copy"),
        Action {
            action_type: ActionType::RequireConfirmation,
            webhook_id: None,
            timeout_s: Some(60),
        },
    )];
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoDenyBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    match outcome {
        ActionOutcome::ConfirmationDenied { rule_id, .. } => {
            assert_eq!(rule_id, "no_files_out_of_workspace");
        }
        other => panic!("expected ConfirmationDenied, got {other:?}"),
    }
    assert_eq!(gw_actuator_calls(&gw), 0);
}

#[tokio::test]
async fn require_confirmation_timeout_propagates() {
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "no_files_out_of_workspace",
        act_attempted_for("fs.copy"),
        Action {
            action_type: ActionType::RequireConfirmation,
            webhook_id: None,
            timeout_s: Some(5),
        },
    )];
    let broker = ScriptedBroker::new(vec![ConfirmationDecision::TimedOut]);
    let gw = Gateway::new(
        RecordingActuator::new(),
        broker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    match outcome {
        ActionOutcome::ConfirmationTimedOut {
            rule_id, timeout_s, ..
        } => {
            assert_eq!(rule_id, "no_files_out_of_workspace");
            assert_eq!(timeout_s, 5);
        }
        other => panic!("expected ConfirmationTimedOut, got {other:?}"),
    }
    assert_eq!(gw_actuator_calls(&gw), 0);
}

#[tokio::test]
async fn precedence_veto_over_confirmation() {
    // Two rules match: veto and require_confirmation. Veto wins.
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![
        rule(
            "confirm_fs_copy",
            act_attempted_for("fs.copy"),
            Action {
                action_type: ActionType::RequireConfirmation,
                webhook_id: None,
                timeout_s: None,
            },
        ),
        rule(
            "veto_fs_copy",
            act_attempted_for("fs.copy"),
            Action {
                action_type: ActionType::Veto,
                webhook_id: None,
                timeout_s: None,
            },
        ),
    ];
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoAllowBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome = gw
        .intercept(fake_action("embedded", "fs.copy"))
        .await
        .unwrap();
    match outcome {
        ActionOutcome::Vetoed { rule_id, .. } => assert_eq!(rule_id, "veto_fs_copy"),
        other => panic!("expected Vetoed, got {other:?}"),
    }
    // Both rules logged as fires.
    assert_eq!(
        count_chunks_by_kind(&memory, "embedded", ChunkKind::Fire).await,
        2
    );
}

#[tokio::test]
async fn external_mcp_caller_governed_identically() {
    // The same rule fires for both embedded and mcp:cursor callers — the
    // gateway has no special trust for being "ours".
    let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
    let rules = vec![rule(
        "no_shell",
        act_attempted_for("shell.run"),
        Action {
            action_type: ActionType::Veto,
            webhook_id: None,
            timeout_s: None,
        },
    )];
    let gw = Gateway::new(
        RecordingActuator::new(),
        AutoAllowBroker,
        StaticRules(rules),
        InMemoryWatchlists::default(),
        memory.clone(),
    );
    let outcome_embedded = gw
        .intercept(fake_action("embedded", "shell.run"))
        .await
        .unwrap();
    let outcome_mcp = gw
        .intercept(fake_action("mcp:cursor", "shell.run"))
        .await
        .unwrap();
    assert!(matches!(outcome_embedded, ActionOutcome::Vetoed { .. }));
    assert!(matches!(outcome_mcp, ActionOutcome::Vetoed { .. }));
    assert_eq!(gw_actuator_calls(&gw), 0);
}

// Helper: read the call count off a Gateway with our concrete actuator type.
fn gw_actuator_calls<B, R, W>(gw: &Gateway<RecordingActuator, B, R, W>) -> usize
where
    B: cel_act_gateway::ConfirmationBroker,
    R: cel_act_gateway::RuleSource,
    W: cellar_types::WatchlistLookup + Send + Sync,
{
    gw.actuator().call_count()
}
