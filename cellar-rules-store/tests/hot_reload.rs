//! Hot-reload integration test.
//!
//! The matcher and the gateway each hold their own `Arc<SqliteRulesStore>`
//! clone. When the store is mutated, both clones must reflect the new
//! state on their next `snapshot()` / `contains()` call — no explicit
//! reload signal.
//!
//! This test pins down the exact wiring pattern the daemon uses in
//! production: one `Arc<SqliteRulesStore>` shared by N consumers, mutated
//! through the same handle.

use std::sync::Arc;

use cel_act_gateway::RuleSource;
use cellar_rules_store::SqliteRulesStore;
use cellar_types::expression::Operator;
use cellar_types::matcher::WatchlistLookup;
use cellar_types::rule::{Action, ActionType, RuleKind};
use cellar_types::{Event, EventKind, EventSource, Expression, Matcher, Rule};
use chrono::Utc;
use serde_json::json;

fn rule(id: &str) -> Rule {
    Rule {
        id: id.into(),
        name: format!("rule {id}"),
        nl_original: "test".into(),
        kind: RuleKind::Watcher,
        enabled: true,
        match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
        action: Action {
            action_type: ActionType::LogOnly,
            webhook_id: None,
            timeout_s: None,
        },
        cooldown_seconds: 0,
        created_at: Utc::now(),
    }
}

#[test]
fn matcher_sees_rule_added_after_clone() {
    // Set up: one store, two `Arc` clones — one for the "gateway", one for
    // the "matcher consumer." Both go through the same shared state.
    let store: Arc<SqliteRulesStore> = SqliteRulesStore::in_memory().unwrap();
    let matcher_view: Arc<SqliteRulesStore> = store.clone();

    // Before the rule is added: matcher's snapshot is empty.
    let initial: Vec<Rule> = matcher_view.snapshot();
    assert!(
        initial.is_empty(),
        "expected empty snapshot before any inserts"
    );

    // Mutate through the original handle.
    store.create_rule(rule("after_clone")).unwrap();

    // Matcher's clone must see it on the next snapshot — no reload call.
    let after: Vec<Rule> = matcher_view.snapshot();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, "after_clone");

    // Drive the actual matcher against the rule and a matching event:
    let event =
        Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/tmp/x");
    let watchlists = matcher_view.clone(); // both R and W from the same Arc
    let rules = matcher_view.snapshot();
    let fires = Matcher::evaluate(&event, &rules, &watchlists);
    assert_eq!(fires.len(), 1, "matcher should have fired the new rule");
}

#[test]
fn matcher_sees_watchlist_changes_through_arc_clones() {
    let store: Arc<SqliteRulesStore> = SqliteRulesStore::in_memory().unwrap();
    let matcher_view: Arc<SqliteRulesStore> = store.clone();

    // Empty list initially.
    assert!(!matcher_view.contains("approved", "x"));

    // Add via the original handle.
    store.create_watchlist("approved", None).unwrap();
    store.add_watchlist_item("approved", "x").unwrap();

    // Matcher's clone sees it.
    assert!(matcher_view.contains("approved", "x"));

    // Remove → matcher's clone reflects the removal.
    store.remove_watchlist_item("approved", "x").unwrap();
    assert!(!matcher_view.contains("approved", "x"));
}

#[test]
fn rule_disabled_flag_changes_visible_to_clones() {
    let store: Arc<SqliteRulesStore> = SqliteRulesStore::in_memory().unwrap();
    let matcher_view: Arc<SqliteRulesStore> = store.clone();

    store.create_rule(rule("r1")).unwrap();
    let event = Event::now(EventSource::Fsevents, EventKind::FileDeleted);

    // Enabled → fires.
    let rules = matcher_view.snapshot();
    let fires = Matcher::evaluate(&event, &rules, &matcher_view);
    assert_eq!(fires.len(), 1);

    // Disable.
    store.set_enabled("r1", false).unwrap();
    let rules = matcher_view.snapshot();
    let fires = Matcher::evaluate(&event, &rules, &matcher_view);
    assert_eq!(fires.len(), 0, "disabled rule must not fire");

    // Re-enable.
    store.set_enabled("r1", true).unwrap();
    let rules = matcher_view.snapshot();
    let fires = Matcher::evaluate(&event, &rules, &matcher_view);
    assert_eq!(fires.len(), 1);
}

#[test]
fn deleted_rule_disappears_from_matcher() {
    let store: Arc<SqliteRulesStore> = SqliteRulesStore::in_memory().unwrap();
    let matcher_view: Arc<SqliteRulesStore> = store.clone();

    store.create_rule(rule("a")).unwrap();
    store.create_rule(rule("b")).unwrap();
    assert_eq!(matcher_view.snapshot().len(), 2);

    store.delete_rule("a").unwrap();
    let after = matcher_view.snapshot();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, "b");
}
