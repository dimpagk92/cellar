//! The daemon's in-process event bus.
//!
//! Every ambient event source (Cortex goalless mode, the process poller, the
//! FSEvents adapter, the `cel_act` gateway's synthetic
//! `agent_action_attempted` events) fans in here. Subscribers — the matcher
//! consumer task (see [`crate::matcher_task`]), the IPC `events`
//! subscription stream, future telemetry collectors — receive every event.
//!
//! Backed by [`tokio::sync::broadcast`]: single producer per source,
//! multiple consumers, lossy on slow consumers. This is the right primitive
//! because events are observational; durable storage is the matcher's
//! responsibility (it writes [`cel_memory::ChunkKind::Fire`] chunks on
//! rule matches) rather than the bus's.

use cellar_types::Event;
use tokio::sync::broadcast;

/// Default channel depth. Generous on the assumption that events can spike
/// during e.g. an `rm -rf` deluge. Configurable when sources are wired in.
pub const DEFAULT_CAPACITY: usize = 4096;

/// Handle for publishing and subscribing.
///
/// Clone freely — clones share the underlying channel.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// New bus with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// New bus with an explicit capacity. Useful for tests that want to
    /// observe lag behavior with a small backlog.
    pub fn with_capacity(cap: usize) -> Self {
        let (tx, _rx) = broadcast::channel(cap);
        Self { tx }
    }

    /// Publish an event. Silently drops it (with a trace-level log) when
    /// there are no subscribers — broadcast won't buffer for absent
    /// receivers, and an event nobody is listening for has no purpose.
    pub fn publish(&self, event: Event) {
        if let Err(broadcast::error::SendError(dropped)) = self.tx.send(event) {
            tracing::trace!(
                source = ?dropped.source,
                kind = ?dropped.kind,
                "event dropped — no subscribers"
            );
        }
    }

    /// Get a fresh subscriber receiver. Each call returns a new receiver
    /// that sees events published from that moment forward (broadcast
    /// semantics — no replay).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Number of currently-connected subscribers. Mostly for diagnostics.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_types::{EventKind, EventSource};

    #[tokio::test]
    async fn publish_then_receive() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.source, EventSource::Fsevents);
        assert_eq!(got.kind, EventKind::FileDeleted);
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
        bus.publish(Event::now(EventSource::Process, EventKind::ProcessStarted));
        let a = rx1.recv().await.unwrap();
        let b = rx2.recv().await.unwrap();
        assert_eq!(a.kind, b.kind);
    }

    #[tokio::test]
    async fn publish_without_subscribers_does_not_panic() {
        let bus = EventBus::new();
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        // No panic, no error surfaced to caller — silent drop is the contract.
    }

    #[tokio::test]
    async fn late_subscriber_misses_earlier_events() {
        let bus = EventBus::new();
        // Published before anyone subscribed → dropped.
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileCreated));

        let mut rx = bus.subscribe();
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.kind, EventKind::FileDeleted);
        // No second event waiting — the create was dropped before subscribe.
    }
}
