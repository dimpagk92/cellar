//! Broadcast bus for rule-fire records the matcher consumer task emits.
//!
//! Analogous to [`crate::bus::EventBus`] but carries [`FireFrame`] values
//! — one per Fire chunk written. The IPC handler holds a clone and
//! subscribes per `fires.subscribe` call; the matcher publishes after
//! every successful Fire write so subscribers see the same ordering the
//! memory audit trail records.
//!
//! Like the event bus, this is a bounded `tokio::sync::broadcast`. A
//! lagged subscriber gets a `RecvError::Lagged(n)` and the forwarder
//! task surfaces it as a `StreamPayload::Gap` so the client can refetch
//! via `fires.recent`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast::{self, Receiver, Sender};

/// One rule fire as observed by the matcher consumer task. The IPC layer
/// wraps this into a `StreamPayload::Fire { entry }` for the wire and
/// returns the same shape from `fires.recent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FireFrame {
    /// Stable identifier so a client can dedupe across reconnect
    /// (`*.recent` + `*.subscribe` may both surface the same frame).
    pub id: String,
    /// Wall-clock time the fire was recorded.
    pub fired_at: DateTime<Utc>,
    /// The rule's ID.
    pub rule_id: String,
    /// The rule's human-readable name.
    pub rule_name: String,
    /// `"watcher"` / `"guard"` / `"audit"`.
    pub rule_kind: String,
    /// The originating event's kind (snake_case enum variant name).
    pub event_kind: String,
    /// The originating event's source (snake_case enum variant name).
    pub event_source: String,
    /// The originating event's data fields.
    pub event_data: Value,
    /// True if the rule's action paused / vetoed the originating action
    /// (gateway-path fires only). Always false for ambient (matcher-task)
    /// fires.
    pub is_blocking: bool,
}

/// Channel capacity for the fire broadcast. Generous because fires are
/// rare relative to events — even a busy day produces single-digit
/// thousands of fires.
const FIRE_BUS_CAPACITY: usize = 4096;

/// Broadcast bus for fires. Cheap to clone (`Arc` underneath).
#[derive(Clone)]
pub struct FireBus {
    tx: Sender<FireFrame>,
}

impl FireBus {
    /// New bus with the default capacity.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(FIRE_BUS_CAPACITY);
        Self { tx }
    }

    /// New bus with an explicit capacity. Used by tests that want a
    /// smaller channel to exercise lag-drop behaviour.
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish one fire. Like `EventBus::publish`, the return tracks
    /// subscribers for trace logs but is not used to throttle.
    pub fn publish(&self, fire: FireFrame) {
        match self.tx.send(fire) {
            Ok(n) => tracing::trace!(subscribers = n, "fire bus publish"),
            Err(_) => tracing::trace!("fire bus publish: no subscribers"),
        }
    }

    /// New subscriber.
    pub fn subscribe(&self) -> Receiver<FireFrame> {
        self.tx.subscribe()
    }

    /// Live subscriber count.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for FireBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(id: &str) -> FireFrame {
        FireFrame {
            id: id.into(),
            fired_at: Utc::now(),
            rule_id: "r1".into(),
            rule_name: "test".into(),
            rule_kind: "watcher".into(),
            event_kind: "file_deleted".into(),
            event_source: "fsevents".into(),
            event_data: json!({"path": "/tmp/x"}),
            is_blocking: false,
        }
    }

    #[tokio::test]
    async fn publish_after_subscribe_delivers() {
        let bus = FireBus::new();
        let mut rx = bus.subscribe();
        bus.publish(frame("a"));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.id, "a");
    }

    #[tokio::test]
    async fn publish_without_subscribers_does_not_panic() {
        let bus = FireBus::new();
        bus.publish(frame("a")); // no subscribers — silently drops
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive() {
        let bus = FireBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(frame("x"));
        assert_eq!(a.recv().await.unwrap().id, "x");
        assert_eq!(b.recv().await.unwrap().id, "x");
    }

    #[test]
    fn frame_round_trips_via_json() {
        let f = frame("test");
        let s = serde_json::to_string(&f).unwrap();
        let back: FireFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(f, back);
    }
}
