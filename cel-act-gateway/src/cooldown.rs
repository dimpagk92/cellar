//! Per-rule cooldown tracker.
//!
//! Every [`Rule`](cellar_types::Rule) carries a `cooldown_seconds` field —
//! the minimum time between consecutive fires of that rule. The matcher is
//! a pure function and doesn't track time; cooldown lives here, in a small
//! structure both the gateway and the matcher consumer task hold a clone
//! of via `Arc`.
//!
//! **Where it hooks in:**
//!
//! - Gateway: `Gateway::intercept` runs the matcher, then for each matched
//!   rule consults the tracker. Suppressed rules don't get a Fire chunk
//!   and don't fan out to webhooks; the decision (allow / pause / veto)
//!   is computed from the non-suppressed matches.
//! - Matcher consumer task: same shape — match, filter by cooldown, then
//!   write Fire chunks and fan out webhooks.
//!
//! **Persistence (Phase 2.x — now wired):** if a [`CooldownPersistence`]
//! impl is attached via [`CooldownTracker::with_store`], every successful
//! `try_fire` writes the new last-fire timestamp through; on construction
//! the tracker re-hydrates its in-memory map from the store. This closes
//! the prior gap where a quick crash-restart could bypass a long cooldown.
//! Without a store attached the tracker is fully in-memory (same behaviour
//! as before).
//!
//! **Clock semantics:** timestamps are wall-clock UTC ([`chrono::Utc`]) so
//! they can round-trip through SQLite. Cooldown decisions are therefore
//! sensitive to wall-clock adjustments — but the worst case is one extra
//! refire on a backward adjustment (the prior fire looks "in the future",
//! the duration goes negative, the cooldown check returns true). That's
//! acceptable for the cooldown semantics, which are already best-effort
//! across daemon restarts.
//!
//! **Concurrency:** mutations under a `Mutex<HashMap<...>>`; reads happen
//! on the matcher hot path but a single map lookup behind a Mutex is fine
//! at the v1 scale (low single-digit MHz rule-evaluation rate at most).
//! If contention ever shows up the right swap is `dashmap` — same API.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Persistence hook for [`CooldownTracker`]. Implemented by storage
/// crates that want last-fire timestamps to survive daemon restarts.
///
/// The tracker calls [`load_all`](Self::load_all) once at construction
/// (in [`CooldownTracker::with_store`]), then [`upsert`](Self::upsert)
/// on every successful `try_fire`, and [`delete_older_than`](Self::delete_older_than)
/// on each `gc`. Implementations must be safe to call from multiple
/// threads — the tracker is shared via `Arc`.
pub trait CooldownPersistence: Send + Sync + std::fmt::Debug {
    /// Return every persisted `(rule_id, last_fired_at)` pair. Called
    /// once at tracker construction to rehydrate the in-memory map.
    /// Implementations should return an empty vec on first-run / IO
    /// failure rather than panicking — the tracker degrades gracefully
    /// to in-memory behaviour.
    fn load_all(&self) -> Vec<(String, DateTime<Utc>)>;

    /// Persist `(rule_id, ts)`, overwriting any existing row for the id.
    /// Called from the cooldown hot path; implementations should be
    /// fast and not block on contention. IO failures should be logged
    /// rather than panicked — cooldown is best-effort.
    fn upsert(&self, rule_id: &str, ts: DateTime<Utc>);

    /// Delete every persisted row whose `last_fired_at < cutoff`.
    /// Called from [`CooldownTracker::gc`] to keep the table bounded.
    fn delete_older_than(&self, cutoff: DateTime<Utc>);
}

/// Tracks the last fire time per rule id. `Arc<CooldownTracker>` is shared
/// between the gateway and the matcher consumer task so a fire through
/// either path counts against the same window.
#[derive(Debug)]
pub struct CooldownTracker {
    last_fire: Mutex<HashMap<String, DateTime<Utc>>>,
    /// Optional persistence sink — `None` means in-memory only.
    store: Option<Arc<dyn CooldownPersistence>>,
}

impl Default for CooldownTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CooldownTracker {
    /// Construct an empty in-memory tracker. Cooldown state is lost on
    /// daemon restart — fine for one-shot tests and the legacy code path.
    pub fn new() -> Self {
        Self {
            last_fire: Mutex::new(HashMap::new()),
            store: None,
        }
    }

    /// Construct a tracker backed by `store`. The store is consulted
    /// once at construction to rehydrate any persisted last-fire
    /// timestamps, then on every successful `try_fire` and `gc`.
    pub fn with_store(store: Arc<dyn CooldownPersistence>) -> Self {
        let initial: HashMap<String, DateTime<Utc>> = store.load_all().into_iter().collect();
        Self {
            last_fire: Mutex::new(initial),
            store: Some(store),
        }
    }

    /// Atomic check-and-record:
    ///
    /// - Returns `true` if the rule should fire now, and records the fire
    ///   timestamp so subsequent calls within the cooldown window return
    ///   `false`.
    /// - Returns `false` if the rule is still within its cooldown window.
    ///
    /// `cooldown_seconds == 0` disables cooldown for that rule — always
    /// returns `true` and *also* records the timestamp (so a rule whose
    /// cooldown is changed from 0 to N still gets the correct window on
    /// the next fire).
    ///
    /// The check-and-record is atomic under a `Mutex`, so two concurrent
    /// callers can't both decide "yes, fire" for the same rule within a
    /// window. The persistence upsert (if a store is attached) happens
    /// while the mutex is held, so a crash mid-upsert leaves the next
    /// `try_fire` to see the older timestamp — safe, allows one extra
    /// refire at worst.
    pub fn try_fire(&self, rule_id: &str, cooldown_seconds: u64) -> bool {
        let now = Utc::now();
        let mut map = self.last_fire.lock().expect("cooldown mutex poisoned");
        if cooldown_seconds > 0 {
            if let Some(last) = map.get(rule_id) {
                // `signed_duration_since` is robust against clock skew —
                // a negative result (prior fire "in the future") means
                // the cooldown effectively did not elapse, so we should
                // STILL fire. We cap the comparison at zero so a negative
                // duration doesn't suppress.
                let elapsed = now.signed_duration_since(*last);
                let elapsed_secs = elapsed.num_seconds();
                if elapsed_secs >= 0 && (elapsed_secs as u64) < cooldown_seconds {
                    return false;
                }
            }
        }
        map.insert(rule_id.to_string(), now);
        if let Some(store) = &self.store {
            store.upsert(rule_id, now);
        }
        true
    }

    /// How many rule ids currently have a recorded last-fire timestamp.
    /// Exposed mostly for daemon-health metrics — `daemon.status` will
    /// surface it in a later slice.
    pub fn tracked_rules(&self) -> usize {
        self.last_fire.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Drop any tracked rules whose last-fire time is older than `max_age`.
    /// Called periodically by the daemon to keep the map bounded under
    /// long-running operation where rules churn (add/remove/rename).
    ///
    /// If a store is attached, the same cutoff is applied to the persisted
    /// rows so the SQLite table stays bounded too.
    pub fn gc(&self, max_age: Duration) {
        let chrono_max = match chrono::Duration::from_std(max_age) {
            Ok(d) => d,
            Err(_) => return, // overflow — refuse to GC
        };
        let cutoff = Utc::now() - chrono_max;
        let mut map = self.last_fire.lock().expect("cooldown mutex poisoned");
        map.retain(|_, last| *last >= cutoff);
        drop(map);
        if let Some(store) = &self.store {
            store.delete_older_than(cutoff);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::thread::sleep;

    #[test]
    fn zero_cooldown_always_fires() {
        let t = CooldownTracker::new();
        assert!(t.try_fire("r", 0));
        assert!(t.try_fire("r", 0));
        assert!(t.try_fire("r", 0));
    }

    #[test]
    fn first_fire_succeeds_then_suppressed_within_window() {
        let t = CooldownTracker::new();
        assert!(t.try_fire("r", 60));
        assert!(!t.try_fire("r", 60));
        assert!(!t.try_fire("r", 60));
    }

    #[test]
    fn distinct_rules_independent() {
        let t = CooldownTracker::new();
        assert!(t.try_fire("a", 60));
        assert!(t.try_fire("b", 60));
        // Both rules tracked.
        assert_eq!(t.tracked_rules(), 2);
        assert!(!t.try_fire("a", 60));
        assert!(!t.try_fire("b", 60));
    }

    #[test]
    fn cooldown_elapsed_allows_refire() {
        let t = CooldownTracker::new();
        // 1s cooldown; wait just over a second to refire.
        assert!(t.try_fire("r", 1));
        assert!(!t.try_fire("r", 1));
        sleep(Duration::from_millis(1100));
        assert!(t.try_fire("r", 1), "expected refire after cooldown elapsed");
    }

    #[test]
    fn gc_drops_stale_entries() {
        let t = CooldownTracker::new();
        t.try_fire("old", 0);
        sleep(Duration::from_millis(50));
        t.try_fire("new", 0);
        assert_eq!(t.tracked_rules(), 2);
        // GC anything older than 40ms — "old" goes, "new" survives.
        t.gc(Duration::from_millis(40));
        assert_eq!(t.tracked_rules(), 1);
    }

    #[test]
    fn cooldown_seconds_change_takes_effect_on_next_fire() {
        // Rule was at cooldown=0, fired, then bumped to cooldown=60 — the
        // next try_fire is suppressed because we recorded the previous
        // fire timestamp.
        let t = CooldownTracker::new();
        assert!(t.try_fire("r", 0));
        assert!(!t.try_fire("r", 60));
    }

    // ───── Persistence-aware tests ─────

    #[derive(Debug, Default)]
    struct MockStore {
        rows: StdMutex<HashMap<String, DateTime<Utc>>>,
        upsert_calls: StdMutex<usize>,
        delete_calls: StdMutex<usize>,
    }

    impl CooldownPersistence for MockStore {
        fn load_all(&self) -> Vec<(String, DateTime<Utc>)> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        }
        fn upsert(&self, rule_id: &str, ts: DateTime<Utc>) {
            self.rows.lock().unwrap().insert(rule_id.to_string(), ts);
            *self.upsert_calls.lock().unwrap() += 1;
        }
        fn delete_older_than(&self, cutoff: DateTime<Utc>) {
            self.rows.lock().unwrap().retain(|_, ts| *ts >= cutoff);
            *self.delete_calls.lock().unwrap() += 1;
        }
    }

    #[test]
    fn store_load_all_rehydrates_on_construction() {
        let store = Arc::new(MockStore::default());
        // Pre-populate the store as if a prior daemon run had fired the rule.
        store.upsert("persisted", Utc::now());
        let t = CooldownTracker::with_store(store.clone());
        // Tracker should have picked up the row.
        assert_eq!(t.tracked_rules(), 1);
        // And the cooldown window from the persisted row should apply.
        assert!(!t.try_fire("persisted", 3600));
    }

    #[test]
    fn store_upsert_called_on_successful_fire() {
        let store = Arc::new(MockStore::default());
        let t = CooldownTracker::with_store(store.clone());
        assert!(t.try_fire("r", 60));
        assert_eq!(*store.upsert_calls.lock().unwrap(), 1);
        // Within the cooldown window: no fire → no upsert.
        assert!(!t.try_fire("r", 60));
        assert_eq!(*store.upsert_calls.lock().unwrap(), 1, "no upsert on suppressed fire");
    }

    #[test]
    fn store_delete_older_than_called_on_gc() {
        let store = Arc::new(MockStore::default());
        let t = CooldownTracker::with_store(store.clone());
        t.try_fire("r", 0);
        t.gc(Duration::from_secs(1));
        assert_eq!(*store.delete_calls.lock().unwrap(), 1);
    }

    #[test]
    fn persistence_survives_simulated_restart() {
        // Round 1: tracker with store, fire a rule with a 1h cooldown.
        let store: Arc<dyn CooldownPersistence> = Arc::new(MockStore::default());
        {
            let t = CooldownTracker::with_store(store.clone());
            assert!(t.try_fire("long_cool", 3600));
            // Within window — suppressed.
            assert!(!t.try_fire("long_cool", 3600));
        }
        // Round 2: simulate daemon restart by constructing a fresh tracker
        // backed by the same store. The persisted row should suppress the
        // would-be refire — this is the bug the persistence layer closes.
        let t2 = CooldownTracker::with_store(store);
        assert_eq!(t2.tracked_rules(), 1, "restart should rehydrate persisted row");
        assert!(
            !t2.try_fire("long_cool", 3600),
            "rule fired pre-restart inside 1h window must still be suppressed"
        );
    }

    #[test]
    fn in_memory_tracker_does_not_call_store() {
        // Sanity check: plain new() should not somehow gain a store.
        let t = CooldownTracker::new();
        t.try_fire("r", 0);
        // No store attached; nothing to assert except this doesn't panic
        // and the in-memory behaviour still works.
        assert_eq!(t.tracked_rules(), 1);
    }
}
