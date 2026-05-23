//! End-to-end test of [`CooldownPersistence`] on [`SqliteRulesStore`].
//!
//! These tests exercise the schema-v3 `rule_cooldowns` table through the
//! `CooldownTracker::with_store(...)` integration. The unit tests in
//! `cel-act-gateway/src/cooldown.rs` cover the tracker logic with a mock
//! store; this file covers the SQL surface — schema migration, RFC3339
//! round-tripping, the ON CONFLICT upsert path, and GC delete behaviour.

use std::sync::Arc;
use std::time::Duration;

use cel_act_gateway::{CooldownPersistence, CooldownTracker};
use cellar_rules_store::SqliteRulesStore;
use chrono::Utc;

#[test]
fn upsert_then_load_round_trips_through_sqlite() {
    let store = SqliteRulesStore::in_memory().unwrap();
    let now = Utc::now();
    CooldownPersistence::upsert(&*store, "r1", now);

    let rows = CooldownPersistence::load_all(&*store);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "r1");
    // RFC3339 round-trip should preserve at least second-precision; we
    // assert millisecond delta to handle the chrono serialization round.
    let delta = (rows[0].1 - now).num_milliseconds().abs();
    assert!(
        delta < 1000,
        "round-trip delta should be < 1s, got {}ms",
        delta
    );
}

#[test]
fn upsert_overwrites_existing_row() {
    let store = SqliteRulesStore::in_memory().unwrap();
    let earlier = Utc::now() - chrono::Duration::seconds(60);
    let later = Utc::now();

    CooldownPersistence::upsert(&*store, "r1", earlier);
    CooldownPersistence::upsert(&*store, "r1", later);

    let rows = CooldownPersistence::load_all(&*store);
    assert_eq!(rows.len(), 1, "ON CONFLICT must update, not insert");
    let delta = (rows[0].1 - later).num_milliseconds().abs();
    assert!(delta < 1000);
}

#[test]
fn delete_older_than_drops_only_stale_rows() {
    let store = SqliteRulesStore::in_memory().unwrap();
    let old = Utc::now() - chrono::Duration::hours(2);
    let fresh = Utc::now();
    CooldownPersistence::upsert(&*store, "old_rule", old);
    CooldownPersistence::upsert(&*store, "fresh_rule", fresh);

    // Cutoff: 1h ago. Only `old_rule` should be deleted.
    let cutoff = Utc::now() - chrono::Duration::hours(1);
    CooldownPersistence::delete_older_than(&*store, cutoff);

    let rows = CooldownPersistence::load_all(&*store);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "fresh_rule");
}

#[test]
fn load_all_on_fresh_store_returns_empty() {
    // Schema v3 must materialise the rule_cooldowns table even when empty,
    // so load_all on a fresh store returns Ok(empty) not an SQL error.
    let store = SqliteRulesStore::in_memory().unwrap();
    let rows = CooldownPersistence::load_all(&*store);
    assert!(rows.is_empty());
}

#[test]
fn tracker_rehydrates_from_store_across_simulated_restart() {
    // Simulates the bug the persistence layer closes: a rule with a long
    // cooldown fires, the daemon crashes, and on restart the same rule is
    // suppressed (not refired) because the persisted row is loaded back.
    let store = SqliteRulesStore::in_memory().unwrap();

    // Round 1: tracker writes through to the store.
    let store_dyn: Arc<dyn CooldownPersistence> = store.clone();
    {
        let t = CooldownTracker::with_store(store_dyn.clone());
        assert!(t.try_fire("long_cool", 3600));
        // Within the 1h window — suppressed.
        assert!(!t.try_fire("long_cool", 3600));
    }
    // Round 2: a fresh tracker bound to the same store should see the
    // persisted timestamp and suppress the would-be refire.
    let t2 = CooldownTracker::with_store(store_dyn);
    assert_eq!(t2.tracked_rules(), 1, "row must rehydrate after restart");
    assert!(
        !t2.try_fire("long_cool", 3600),
        "1h cooldown must hold across restart"
    );
}

#[test]
fn tracker_gc_propagates_to_store() {
    let store = SqliteRulesStore::in_memory().unwrap();
    let store_dyn: Arc<dyn CooldownPersistence> = store.clone();
    let t = CooldownTracker::with_store(store_dyn);

    t.try_fire("r1", 0);
    assert_eq!(CooldownPersistence::load_all(&*store).len(), 1);

    // Wait so the entry is older than the GC cutoff.
    std::thread::sleep(Duration::from_millis(80));
    t.gc(Duration::from_millis(40));

    // Both the in-memory map and the persisted row should be gone.
    assert_eq!(t.tracked_rules(), 0);
    assert_eq!(CooldownPersistence::load_all(&*store).len(), 0);
}
