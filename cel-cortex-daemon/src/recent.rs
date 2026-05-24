//! Bounded in-memory ring buffers for `events.recent` and `fires.recent`.
//!
//! Backfill for newly-attached subscribers: the Activity tab calls
//! `events.recent` once on attach to populate the timeline, then
//! `events.subscribe` for the live tail.
//!
//! **In-memory only.** Daemon restart clears the rings. Phase 2.x can
//! persist them to SQLite if a longer history is needed; for the
//! demo-grade Activity tab a few-minutes-deep memory window is fine.
//!
//! **Cheap reads.** Filtering happens at read time over the materialised
//! ring; the matching predicate (kinds / sources / rule_ids / since)
//! mirrors the IPC `StreamFilter` exactly.

use std::collections::VecDeque;
use std::sync::Mutex;

use cellar_ipc::params::stream_filter::StreamFilter;
use cellar_types::Event;
use serde_json::Value;

use crate::fire_bus::FireFrame;

/// Maximum entries kept per ring buffer. Tuned for the demo workload —
/// roughly a few minutes of activity at the v1 event rate. Older entries
/// are dropped on the oldest end as new ones arrive.
pub const DEFAULT_RING_CAPACITY: usize = 1024;

/// Generic ring buffer with a capacity bound and a predicate filter at
/// read time.
pub struct Ring<T: Clone> {
    inner: Mutex<VecDeque<T>>,
    capacity: usize,
}

impl<T: Clone> Ring<T> {
    /// New ring with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_RING_CAPACITY)
    }

    /// New ring with an explicit capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Append one entry, evicting the oldest if at capacity.
    pub fn push(&self, item: T) {
        let mut q = self.inner.lock().expect("ring mutex poisoned");
        if q.len() == self.capacity {
            q.pop_front();
        }
        q.push_back(item);
    }

    /// Count of entries currently held.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read with a filter predicate. Returns oldest-first up to `limit`.
    /// `predicate` is evaluated per-item; `None` means "no filter".
    pub fn filtered<F>(&self, limit: usize, predicate: F) -> Vec<T>
    where
        F: Fn(&T) -> bool,
    {
        let q = self.inner.lock().expect("ring mutex poisoned");
        q.iter()
            .filter(|t| predicate(t))
            .take(limit)
            .cloned()
            .collect()
    }
}

impl<T: Clone> Default for Ring<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ───── Specialised filter helpers ─────

/// Applies a `StreamFilter` to an `Event` — `kinds`, `sources`, `since`.
/// `limit`, `rule_ids`, and `callers` are ignored (limit handled by
/// caller; the rule_ids / callers filters don't apply to events).
pub fn event_matches(event: &Event, f: &StreamFilter) -> bool {
    if let Some(since) = f.since {
        if event.ts < since {
            return false;
        }
    }
    if let Some(kinds) = &f.kinds {
        let event_kind = serde_json::to_value(&event.kind)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        if !kinds.iter().any(|k| k == &event_kind) {
            return false;
        }
    }
    if let Some(sources) = &f.sources {
        let event_source = serde_json::to_value(event.source)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        if !sources.iter().any(|s| s == &event_source) {
            return false;
        }
    }
    true
}

/// Applies a `StreamFilter` to a `FireFrame`. Same shape as
/// [`event_matches`] but also honours `rule_ids` and reads the event
/// data (kind/source) off the frame's stored fields.
pub fn fire_matches(fire: &FireFrame, f: &StreamFilter) -> bool {
    if let Some(since) = f.since {
        if fire.fired_at < since {
            return false;
        }
    }
    if let Some(kinds) = &f.kinds {
        if !kinds.iter().any(|k| k == &fire.event_kind) {
            return false;
        }
    }
    if let Some(sources) = &f.sources {
        if !sources.iter().any(|s| s == &fire.event_source) {
            return false;
        }
    }
    if let Some(rule_ids) = &f.rule_ids {
        if !rule_ids.iter().any(|r| r == &fire.rule_id) {
            return false;
        }
    }
    true
}

/// Filter predicate for [`crate::agent_action_bus::AgentActionFrame`]s.
/// Checks `filter.callers` against `frame.action["caller"]`.
/// Other filter fields (`since`, `kinds`, `sources`) are not applicable
/// to agent actions in v1 and are silently ignored.
pub fn agent_action_matches(
    frame: &crate::agent_action_bus::AgentActionFrame,
    f: &StreamFilter,
) -> bool {
    if let Some(callers) = &f.callers {
        let caller = frame.action["caller"].as_str().unwrap_or("");
        if !callers.iter().any(|c| c == caller) {
            return false;
        }
    }
    true
}

/// Convenience: render an `Event` to a wire-shaped JSON `Value` for the
/// IPC `events.recent` result. Mirrors what the subscribe forwarder
/// task emits in `StreamPayload::Event { event }`.
pub fn event_to_value(event: &Event) -> Value {
    serde_json::to_value(event).unwrap_or(Value::Null)
}

/// Convenience: render a `FireFrame` to JSON for the IPC `fires.recent`
/// result. Same shape `StreamPayload::Fire { entry }` carries.
pub fn fire_to_value(fire: &FireFrame) -> Value {
    serde_json::to_value(fire).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_types::{EventKind, EventSource};
    use chrono::Duration;

    #[test]
    fn ring_pushes_and_evicts_oldest() {
        let r: Ring<i32> = Ring::with_capacity(3);
        r.push(1);
        r.push(2);
        r.push(3);
        r.push(4);
        let all = r.filtered(10, |_| true);
        assert_eq!(all, vec![2, 3, 4]);
    }

    #[test]
    fn ring_limit_caps_output() {
        let r: Ring<i32> = Ring::with_capacity(10);
        for i in 0..5 {
            r.push(i);
        }
        let two = r.filtered(2, |_| true);
        assert_eq!(two, vec![0, 1]);
    }

    #[test]
    fn ring_filter_predicate() {
        let r: Ring<i32> = Ring::with_capacity(10);
        for i in 0..6 {
            r.push(i);
        }
        let evens = r.filtered(10, |&n| n % 2 == 0);
        assert_eq!(evens, vec![0, 2, 4]);
    }

    #[test]
    fn event_matches_kind() {
        let e1 = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let e2 = Event::now(EventSource::Fsevents, EventKind::FileCreated);
        let f = StreamFilter {
            kinds: Some(vec!["file_deleted".into()]),
            ..Default::default()
        };
        assert!(event_matches(&e1, &f));
        assert!(!event_matches(&e2, &f));
    }

    #[test]
    fn event_matches_source() {
        let e1 = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let e2 = Event::now(EventSource::Process, EventKind::ProcessStarted);
        let f = StreamFilter {
            sources: Some(vec!["fsevents".into()]),
            ..Default::default()
        };
        assert!(event_matches(&e1, &f));
        assert!(!event_matches(&e2, &f));
    }

    #[test]
    fn event_matches_since() {
        let mut e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        // Backdate the event by an hour.
        e.ts = chrono::Utc::now() - Duration::hours(1);
        let f = StreamFilter {
            since: Some(chrono::Utc::now() - Duration::minutes(5)),
            ..Default::default()
        };
        assert!(!event_matches(&e, &f));
    }

    #[test]
    fn empty_filter_matches_everything() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        let f = StreamFilter::default();
        assert!(event_matches(&e, &f));
    }

    #[test]
    fn fire_matches_rule_id() {
        let mut fr = FireFrame {
            id: "f1".into(),
            fired_at: chrono::Utc::now(),
            rule_id: "rule_x".into(),
            rule_name: "X".into(),
            rule_kind: "watcher".into(),
            event_kind: "file_deleted".into(),
            event_source: "fsevents".into(),
            event_data: Value::Null,
            is_blocking: false,
        };
        let f = StreamFilter {
            rule_ids: Some(vec!["rule_y".into()]),
            ..Default::default()
        };
        assert!(!fire_matches(&fr, &f));
        fr.rule_id = "rule_y".into();
        assert!(fire_matches(&fr, &f));
    }
}
