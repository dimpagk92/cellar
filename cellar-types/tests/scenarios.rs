//! End-to-end tests for the four v1 demo scenarios from `cellar-app-v1.md` §5.
//!
//! These tests are the formal definition of "Phase 0 done": the matcher must
//! handle all four scenarios correctly. Any change to the expression language
//! or the rule schema that breaks these tests is a breaking change.

use cellar_types::{
    event::{Event, EventKind, EventSource},
    expression::{Expression, Operator},
    matcher::Matcher,
    rule::{Action, ActionType, Rule, RuleKind},
    watchlist::InMemoryWatchlists,
};
use serde_json::json;

/// Helper to build a rule with a deterministic id.
fn rule(id: &str, name: &str, kind: RuleKind, expr: Expression, action: Action) -> Rule {
    Rule {
        id: id.into(),
        name: name.into(),
        nl_original: name.into(),
        kind,
        enabled: true,
        match_expr: expr,
        action,
        cooldown_seconds: 60,
        created_at: chrono::Utc::now(),
    }
}

fn webhook_action() -> Action {
    Action {
        action_type: ActionType::Webhook,
        webhook_id: Some("default".into()),
        timeout_s: None,
    }
}

fn require_confirmation_action() -> Action {
    Action {
        action_type: ActionType::RequireConfirmation,
        webhook_id: None,
        timeout_s: Some(300),
    }
}

// -------------------------------------------------------------------------
// Scenario 1: App allowlist
// "Notify me when an app not on my approved_apps watchlist launches."
// -------------------------------------------------------------------------

#[test]
fn scenario_1_app_outside_allowlist_fires() {
    let rule = rule(
        "rule_app_allowlist",
        "App allowlist",
        RuleKind::Watcher,
        Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!("process_started")),
            Expression::leaf(
                "data.bundle_id",
                Operator::NotInWatchlist,
                json!("approved_apps"),
            ),
        ]),
        webhook_action(),
    );

    let mut watchlists = InMemoryWatchlists::default();
    watchlists.set(
        "approved_apps",
        ["com.apple.Safari", "com.anthropic.claude"],
    );

    // Approved app: should NOT fire
    let approved = Event::now(EventSource::Process, EventKind::ProcessStarted)
        .with_data("bundle_id", "com.apple.Safari")
        .with_data("pid", 12345);
    assert_eq!(
        Matcher::evaluate(&approved, std::slice::from_ref(&rule), &watchlists).len(),
        0,
        "approved app should not fire"
    );

    // Unknown app: should fire
    let unknown = Event::now(EventSource::Process, EventKind::ProcessStarted)
        .with_data("bundle_id", "com.suspicious.unknown")
        .with_data("pid", 67890);
    let fired = Matcher::evaluate(&unknown, std::slice::from_ref(&rule), &watchlists);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].rule.id, "rule_app_allowlist");
}

// -------------------------------------------------------------------------
// Scenario 2: Big delete
// "Notify me when a file >1 GB is deleted from ~/Documents."
// -------------------------------------------------------------------------

#[test]
fn scenario_2_big_file_deletion_fires() {
    let rule = rule(
        "rule_big_delete",
        "Big delete in Documents",
        RuleKind::Watcher,
        Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            Expression::leaf("data.path", Operator::StartsWith, json!("~/Documents")),
            Expression::leaf(
                "data.size_bytes",
                Operator::Gte,
                json!(1_073_741_824u64), // 1 GB
            ),
        ]),
        webhook_action(),
    );
    let watchlists = InMemoryWatchlists::default();

    // Small file in Documents: should NOT fire
    let small = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
        .with_data("path", "~/Documents/notes.txt")
        .with_data("size_bytes", 1024u64);
    assert_eq!(
        Matcher::evaluate(&small, std::slice::from_ref(&rule), &watchlists).len(),
        0
    );

    // Big file outside Documents: should NOT fire
    let off_path = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
        .with_data("path", "/tmp/cache.bin")
        .with_data("size_bytes", 5_368_709_120u64);
    assert_eq!(
        Matcher::evaluate(&off_path, std::slice::from_ref(&rule), &watchlists).len(),
        0
    );

    // Big file in Documents: SHOULD fire
    let target = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
        .with_data("path", "~/Documents/quarterly-report.pdf")
        .with_data("size_bytes", 2_147_483_648u64);
    let fired = Matcher::evaluate(&target, std::slice::from_ref(&rule), &watchlists);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].rule.kind, RuleKind::Watcher);
}

// -------------------------------------------------------------------------
// Scenario 3: URL guard
// "Require confirmation when active browser URL matches a blocklist pattern."
// -------------------------------------------------------------------------

#[test]
fn scenario_3_blocklist_url_requires_confirmation() {
    let rule = rule(
        "rule_url_guard",
        "URL guard",
        RuleKind::Guard,
        Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!("url_changed")),
            Expression::leaf(
                "data.url",
                Operator::Regex,
                json!(r"^https?://([^/]+\.)?(twitter\.com|x\.com|reddit\.com)/"),
            ),
        ]),
        require_confirmation_action(),
    );
    let watchlists = InMemoryWatchlists::default();

    let benign = Event::now(EventSource::CortexCdp, EventKind::UrlChanged)
        .with_data("url", "https://docs.rust-lang.org/std/");
    assert_eq!(
        Matcher::evaluate(&benign, std::slice::from_ref(&rule), &watchlists).len(),
        0
    );

    let blocked = Event::now(EventSource::CortexCdp, EventKind::UrlChanged)
        .with_data("url", "https://twitter.com/notifications");
    let fired = Matcher::evaluate(&blocked, std::slice::from_ref(&rule), &watchlists);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].rule.kind, RuleKind::Guard);
    assert_eq!(
        fired[0].rule.action.action_type,
        ActionType::RequireConfirmation
    );
}

// -------------------------------------------------------------------------
// Scenario 4: Agent guard — the canonical demo
// "No files outside ~/Workspace may be moved without confirmation."
// Tests the trust-and-execution wedge: rule fires on a cel_act_gateway event,
// the action type is `require_confirmation`, and the modal flow can resolve it.
// -------------------------------------------------------------------------

#[test]
fn scenario_4_agent_action_intercepted() {
    let rule = rule(
        "rule_workspace_guard",
        "No files outside workspace",
        RuleKind::Guard,
        Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!("agent_action_attempted")),
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
        require_confirmation_action(),
    );
    let watchlists = InMemoryWatchlists::default();

    // Action inside workspace: should NOT fire
    let in_workspace = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted)
        .with_data("action_type", "fs.copy")
        .with_data(
            "action_args",
            json!({
                "source_path": "~/Workspace/Q4/report.pdf",
                "target_path": "/Volumes/External/Backup/report.pdf"
            }),
        )
        .with_data("caller", "embedded")
        .with_data("agent_session_id", "sess_abc");
    assert_eq!(
        Matcher::evaluate(&in_workspace, std::slice::from_ref(&rule), &watchlists).len(),
        0,
        "actions inside workspace should not fire"
    );

    // Read action outside workspace (not in fs.move/fs.copy list): should NOT fire
    let wrong_action = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted)
        .with_data("action_type", "fs.read")
        .with_data(
            "action_args",
            json!({
                "source_path": "~/Documents/personal.pdf"
            }),
        );
    assert_eq!(
        Matcher::evaluate(&wrong_action, std::slice::from_ref(&rule), &watchlists).len(),
        0,
        "non-move/copy actions should not fire"
    );

    // The canonical demo: agent attempting to copy outside workspace → confirm
    let attempted = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted)
        .with_data("action_type", "fs.copy")
        .with_data(
            "action_args",
            json!({
                "source_path": "~/Documents/Q4/personal-report.pdf",
                "target_path": "/Volumes/External/Archive/personal-report.pdf"
            }),
        )
        .with_data("caller", "embedded")
        .with_data("agent_session_id", "sess_abc");

    let fired = Matcher::evaluate(&attempted, std::slice::from_ref(&rule), &watchlists);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].rule.kind, RuleKind::Guard);
    assert_eq!(
        fired[0].rule.action.action_type,
        ActionType::RequireConfirmation
    );
    assert_eq!(fired[0].rule.action.timeout_s, Some(300));
}

// -------------------------------------------------------------------------
// Cross-cutting: external MCP caller (Cursor / Codex / Claude Desktop)
// fires the same rule. No special trust for being the embedded agent.
// -------------------------------------------------------------------------

#[test]
fn external_mcp_caller_governed_by_same_rule() {
    let rule = rule(
        "rule_workspace_guard",
        "No files outside workspace",
        RuleKind::Guard,
        Expression::all(vec![
            Expression::leaf("kind", Operator::Eq, json!("agent_action_attempted")),
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
        require_confirmation_action(),
    );
    let watchlists = InMemoryWatchlists::default();

    let from_cursor = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted)
        .with_data("action_type", "fs.move")
        .with_data(
            "action_args",
            json!({ "source_path": "~/Downloads/x.bin", "target_path": "/tmp/x.bin" }),
        )
        .with_data("caller", "mcp:cursor");

    let fired = Matcher::evaluate(&from_cursor, std::slice::from_ref(&rule), &watchlists);
    assert_eq!(fired.len(), 1, "external MCP callers are governed too");
}

// -------------------------------------------------------------------------
// Multiple rules: matcher returns all matching rules, not just the first.
// -------------------------------------------------------------------------

#[test]
fn multiple_matches_returned_in_order() {
    let r_a = rule(
        "rule_a",
        "any deletion",
        RuleKind::Audit,
        Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
        Action {
            action_type: ActionType::LogOnly,
            webhook_id: None,
            timeout_s: None,
        },
    );
    let r_b = rule(
        "rule_b",
        "in Documents",
        RuleKind::Watcher,
        Expression::leaf("data.path", Operator::StartsWith, json!("~/Documents")),
        webhook_action(),
    );
    let watchlists = InMemoryWatchlists::default();

    let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
        .with_data("path", "~/Documents/x.txt");
    let rs = [r_a, r_b];
    let fired = Matcher::evaluate(&e, &rs, &watchlists);
    assert_eq!(fired.len(), 2);
    assert_eq!(fired[0].rule.id, "rule_a");
    assert_eq!(fired[1].rule.id, "rule_b");
}
