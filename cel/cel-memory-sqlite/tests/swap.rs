//! Demonstrates the locked-trait swap point: replacing
//! [`BasicMemoryProvider`] with [`SqliteMemoryProvider`] is a single line
//! in `wire_subsystems()`; every other callsite — the embedded agent, the
//! NL compiler, the gateway, the matcher post-fire hook — depends only on
//! `Arc<dyn MemoryProvider>`.
//!
//! This test constructs a "future" daemon with the SQLite provider, runs
//! the same scenario_4 flow we have for the basic provider, and asserts
//! the audit trail lands in the SQLite store.
//!
//! [`BasicMemoryProvider`]: cel_memory::BasicMemoryProvider
//! [`SqliteMemoryProvider`]: cel_memory_sqlite::SqliteMemoryProvider

use std::sync::Arc;

use cel_act_gateway::test_support::{fake_action, RecordingActuator, ScriptedBroker};
use cel_act_gateway::traits::StaticRules;
use cel_act_gateway::{ConfirmationDecision, Gateway};
use cel_memory::{ChunkKind, MemoryProvider};
use cel_memory_sqlite::{MockEmbedder, SqliteMemoryProvider};
use cellar_types::{
    Action, ActionType, EventKind, Expression, InMemoryWatchlists, Operator, Rule, RuleKind,
};
use chrono::Utc;
use serde_json::json;

#[tokio::test]
async fn scenario_4_works_with_sqlite_memory_provider() {
    // Wire memory using SqliteMemoryProvider — the one-line swap that
    // Memory Phase 1+ enables in `Daemon::wire_subsystems()`.
    let embedder = Arc::new(MockEmbedder::new());
    let memory: Arc<dyn MemoryProvider> = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap(),
    );

    // Same scenario_4 rule as the BasicMemoryProvider test.
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

    let broker = ScriptedBroker::new(vec![ConfirmationDecision::Allow]);
    let gw = Gateway::new(
        RecordingActuator::with_response(json!({"copied": true})),
        broker,
        StaticRules(vec![rule]),
        InMemoryWatchlists::default(),
        memory.clone(),
    );

    let mut action = fake_action("embedded", "fs.copy");
    action.action_args = json!({
        "source_path": "~/Documents/Q4/Q4-report-personal.pdf",
        "dest_path": "/Volumes/External/Archive/"
    });
    action.agent_session_id = Some("sess_archive_q4".into());

    let outcome = gw.intercept(action).await.unwrap();
    assert!(outcome.executed(), "expected Executed, got {outcome:?}");
    assert_eq!(gw.actuator().call_count(), 1);

    // Stats prove the audit chunks landed in the SQLite store.
    let stats = memory.stats().await.unwrap();
    assert_eq!(stats.total_chunks, 2); // 1 Fire + 1 Action
    assert_eq!(stats.embedding_model.as_deref(), Some("mock-384"));
}

#[tokio::test]
async fn sqlite_provider_survives_writes_through_locked_trait() {
    // Plain MemoryProvider usage — make sure the trait surface works
    // through Arc<dyn MemoryProvider>, not just the concrete type.
    let embedder = Arc::new(MockEmbedder::new());
    let memory: Arc<dyn MemoryProvider> = Arc::new(
        SqliteMemoryProvider::open_in_memory(embedder)
            .await
            .unwrap(),
    );
    let chunk = memory
        .write(cel_memory::NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: cel_memory::ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "embedded".into(),
            content: "user asked about the Q4 report".into(),
            metadata: serde_json::Value::Null,
            importance: None,
            shareable: false,
            pinned: false,
        })
        .await
        .unwrap();
    let fetched = memory.get(&chunk.id).await.unwrap().unwrap();
    assert_eq!(fetched.content, chunk.content);
}
