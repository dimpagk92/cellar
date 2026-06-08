//! Integration tests: the compiler producing rules that pass the matcher's
//! v1 scenario checks. Exercises the full pipeline end-to-end:
//!   NL string → Compiler (mocked LLM) → Rule JSON → matcher accepts shape.
//!
//! These tests guard against the prompt's few-shot examples drifting out of
//! sync with the rule schema. If `cellar-types::Rule` changes shape, these
//! tests fail loudly.

use cellar_llm_router::{
    provider::MockProvider,
    types::{CompletionResponse, ContentBlock, StopReason, Usage},
};
use cellar_rule_compiler::{CompileRequest, Compiler};
use cellar_types::{
    event::{Event, EventKind, EventSource},
    matcher::Matcher,
    rule::RuleKind,
    watchlist::InMemoryWatchlists,
};
use serde_json::json;
use std::sync::Arc;

fn mock(text: &str) -> Arc<MockProvider> {
    MockProvider::new(vec![CompletionResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        model: None,
    }])
}

/// Scenario 2 from cellar-app-v1.md §5: big delete in Documents.
/// We mock the LLM emitting the exact expected JSON, then verify the compiled
/// rule fires on the matching event.
#[tokio::test]
async fn compiled_rule_fires_on_big_delete() {
    let llm_json = r#"{
        "id": "draft",
        "name": "Big delete in Documents",
        "nl_original": "notify me when a file larger than 1GB is deleted from ~/Documents",
        "kind": "watcher",
        "enabled": true,
        "created_at": "1970-01-01T00:00:00Z",
        "match": {
            "all": [
                {"leaf": {"field": "kind", "op": "eq", "value": "file_deleted"}},
                {"leaf": {"field": "data.path", "op": "starts_with", "value": "~/Documents"}},
                {"leaf": {"field": "data.size_bytes", "op": "gte", "value": 1073741824}}
            ]
        },
        "action": {"type": "webhook", "webhook_id": "default"},
        "cooldown_seconds": 60
    }"#;

    let compiler = Compiler::new(mock(llm_json), "mock");
    let result = compiler
        .compile(CompileRequest::new(
            "notify me when a file larger than 1GB is deleted from ~/Documents",
        ))
        .await
        .unwrap();

    assert_eq!(result.draft_rule.kind, RuleKind::Watcher);

    // Now run the matcher with the compiled rule.
    let ws = InMemoryWatchlists::default();
    let target = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
        .with_data("path", "~/Documents/big.pdf")
        .with_data("size_bytes", 2_147_483_648u64);
    let rules = [result.draft_rule];
    let fired = Matcher::evaluate(&target, &rules, &ws);
    assert_eq!(
        fired.len(),
        1,
        "compiled rule should fire on matching event"
    );
}

/// Scenario 4 from cellar-app-v1.md §5: agent guard.
/// The canonical demo — compiled rule, agent_action_attempted event, matcher
/// fires with `require_confirmation` action.
#[tokio::test]
async fn compiled_agent_guard_intercepts_action() {
    let llm_json = r#"{
        "id": "draft",
        "name": "No files outside workspace",
        "nl_original": "require my confirmation before the agent moves any file outside ~/Workspace",
        "kind": "guard",
        "enabled": true,
        "created_at": "1970-01-01T00:00:00Z",
        "match": {
            "all": [
                {"leaf": {"field": "kind", "op": "eq", "value": "agent_action_attempted"}},
                {"leaf": {"field": "data.action_type", "op": "in", "value": ["fs.move", "fs.copy"]}},
                {"leaf": {"field": "data.action_args.source_path", "op": "not_starts_with", "value": "~/Workspace"}}
            ]
        },
        "action": {"type": "require_confirmation", "timeout_s": 300},
        "cooldown_seconds": 0
    }"#;

    let compiler = Compiler::new(mock(llm_json), "mock");
    let result = compiler
        .compile(CompileRequest::new(
            "require my confirmation before the agent moves any file outside ~/Workspace",
        ))
        .await
        .unwrap();

    assert_eq!(result.draft_rule.kind, RuleKind::Guard);
    assert_eq!(
        result.draft_rule.action.action_type,
        cellar_types::rule::ActionType::RequireConfirmation
    );
    assert_eq!(result.draft_rule.action.timeout_s, Some(300));

    let ws = InMemoryWatchlists::default();
    let attempted = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted)
        .with_data("action_type", "fs.copy")
        .with_data(
            "action_args",
            json!({
                "source_path": "~/Documents/personal.pdf",
                "target_path": "/Volumes/External/Archive/personal.pdf"
            }),
        );
    let rules = [result.draft_rule];
    let fired = Matcher::evaluate(&attempted, &rules, &ws);
    assert_eq!(
        fired.len(),
        1,
        "guard should fire on out-of-workspace action"
    );
}

/// Phase 5: the named `redact_memory` action — sugar for `Veto` on
/// `memory_write_attempted` events. Verifies the full pipeline:
///   NL phrasing → compiler → `ActionType::RedactMemory` → matcher fires
///                  on the synthetic memory-write event.
#[tokio::test]
async fn compiled_redact_memory_rule_fires_on_matching_chunk() {
    let llm_json = r#"{
        "id": "draft",
        "name": "Redact bank.example.com memory",
        "nl_original": "never persist any memory chunk mentioning bank.example.com",
        "kind": "audit",
        "enabled": true,
        "created_at": "1970-01-01T00:00:00Z",
        "match": {
            "all": [
                {"leaf": {"field": "kind", "op": "eq", "value": "memory_write_attempted"}},
                {"leaf": {"field": "data.content_preview", "op": "contains", "value": "bank.example.com"}}
            ]
        },
        "action": {"type": "redact_memory"},
        "cooldown_seconds": 0
    }"#;

    let compiler = Compiler::new(mock(llm_json), "mock");
    let result = compiler
        .compile(CompileRequest::new(
            "never persist any memory chunk mentioning bank.example.com",
        ))
        .await
        .unwrap();

    assert_eq!(result.draft_rule.kind, RuleKind::Audit);
    assert_eq!(
        result.draft_rule.action.action_type,
        cellar_types::rule::ActionType::RedactMemory,
        "compiler must surface the named RedactMemory variant verbatim"
    );

    // The compiled rule must fire on a synthetic memory_write_attempted event
    // whose content_preview contains the matched substring.
    let ws = InMemoryWatchlists::default();
    let event = Event::now(EventSource::Memory, EventKind::MemoryWriteAttempted)
        .with_data("caller", "embedded")
        .with_data("kind", "chat")
        .with_data("source", "embedded")
        .with_data(
            "content_preview",
            "I logged into bank.example.com today and saw…",
        );
    let rules = [result.draft_rule.clone()];
    let fired = Matcher::evaluate(&event, &rules, &ws);
    assert_eq!(
        fired.len(),
        1,
        "redact_memory rule should fire on matching memory_write_attempted"
    );

    // An innocent chunk doesn't fire — substring isn't there.
    let innocent = Event::now(EventSource::Memory, EventKind::MemoryWriteAttempted)
        .with_data("caller", "embedded")
        .with_data("kind", "chat")
        .with_data("source", "embedded")
        .with_data("content_preview", "discussing the weather");
    let fired = Matcher::evaluate(&innocent, &rules, &ws);
    assert!(
        fired.is_empty(),
        "redact_memory rule should NOT fire on unrelated chunks"
    );

    // The human-readable summary must call out the variant explicitly.
    assert!(
        result.human_readable.contains("redact memory"),
        "human-readable summary should label the action as 'redact memory'"
    );
}

/// Human-readable summary is non-empty and contains the key elements.
#[tokio::test]
async fn human_readable_summary_includes_when_then() {
    let llm_json = r#"{
        "id": "draft",
        "name": "App allowlist",
        "nl_original": "tell me when an app that isn't in my approved list launches",
        "kind": "watcher",
        "enabled": true,
        "created_at": "1970-01-01T00:00:00Z",
        "match": {
            "all": [
                {"leaf": {"field": "kind", "op": "eq", "value": "process_started"}},
                {"leaf": {"field": "data.bundle_id", "op": "not_in_watchlist", "value": "approved_apps"}}
            ]
        },
        "action": {"type": "webhook", "webhook_id": "default"},
        "cooldown_seconds": 60
    }"#;

    let compiler = Compiler::new(mock(llm_json), "mock");
    let result = compiler
        .compile(
            CompileRequest::new("tell me when an app that isn't in my approved list launches")
                .with_watchlists(["approved_apps"]),
        )
        .await
        .unwrap();

    let summary = &result.human_readable;
    assert!(summary.starts_with("WHEN\n"));
    assert!(summary.contains("THEN\n"));
    assert!(summary.contains("kind = process_started"));
    assert!(summary.contains("NOT in watchlist `approved_apps`"));
    assert!(summary.contains("fire webhook `default`"));
}
