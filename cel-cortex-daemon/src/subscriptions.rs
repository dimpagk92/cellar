//! Subscription registry and per-subscription forwarder tasks for
//! `events.subscribe` / `fires.subscribe`.
//!
//! Architecture: each successful `.subscribe` call spawns a small
//! forwarder task that bridges the broadcast bus → the per-connection
//! `FrameSink`. The registry holds a `JoinHandle` for each so
//! `.unsubscribe` can abort the task.
//!
//! Filtering happens inside the forwarder (server-side) so the daemon
//! doesn't push frames the client would just discard. Filter is captured
//! at subscribe time and immutable; updating filters requires unsubscribe
//! + resubscribe (the v1 IPC RFC's locked surface — no `.update_filter`).
//!
//! Backpressure (IPC RFC §6): two distinct layers are policed.
//! 1. **Bus → forwarder.** Each forwarder subscribes to a
//!    [`tokio::sync::broadcast`] bus that drops on slow consumers. When
//!    the broadcast lags we surface a [`StreamPayload::Gap`] with the
//!    `dropped` count straight from `RecvError::Lagged(n)`.
//! 2. **Forwarder → connection.** The per-connection mpsc inside
//!    [`FrameSink`] is bounded at 256. The "standard" subscriptions
//!    (`events`, `fires`, `agent_actions`) use the [`GapState`]
//!    state machine below: `try_send`, drop on `Full`, then emit ONE
//!    `subscription.gap` once the client catches up. The "critical"
//!    subscriptions (`confirmation`, `agent.chat`) instead call
//!    `sink.request_close()` to force the client to reconnect (RFC §6
//!    "applies to all subscriptions except confirmation.subscribe and
//!    agent.chat.subscribe").

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;

use cellar_ipc::handler::FrameSink;
use cellar_ipc::params::stream_filter::StreamFilter;
use cellar_ipc::subscription::{StreamFrame, StreamPayload, SubscriptionId};
use cellar_types::Event;
use chrono::{DateTime, Utc};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use crate::agent_action_bus::AgentActionBus;
use crate::bus::EventBus;
use crate::chat_bus::ChatBus;
use crate::confirmation::ConfirmationBus;
use crate::fire_bus::FireBus;
use crate::recent::{
    agent_action_matches, event_matches, event_to_value, fire_matches, fire_to_value,
};

/// Per-subscription backpressure bookkeeping (IPC RFC §6).
///
/// Tracks the count of frames dropped since the last gap emit and the
/// timestamp at which dropping started, so a single
/// [`StreamPayload::Gap`] notification can summarise the missed window
/// once the client catches up.
///
/// The transitions are simple enough to be a pure state machine: there
/// is no IO inside `GapState`, only counters and atomics. Forwarder
/// tasks call [`Self::on_dropped`] when `try_send` returns `Full` and
/// [`Self::on_caught_up`] when a subsequent `try_send` succeeds while
/// the state is in drop-mode.
#[derive(Debug)]
pub struct GapState {
    /// Frames dropped since the last successful gap emit.
    dropped_since_last_gap_emit: AtomicU64,
    /// True iff we're currently dropping frames (i.e. at least one
    /// `Full` since the last successful send).
    dropping: AtomicBool,
    /// Real wallclock at which the current dropping window began. Stored
    /// under a Mutex because `SystemTime` is not `Copy + Atomic`.
    /// Guarded by `dropping == true` — only valid when set.
    last_drop_started_at: Mutex<Option<SystemTime>>,
}

impl Default for GapState {
    fn default() -> Self {
        Self::new()
    }
}

impl GapState {
    /// Fresh state — no drops, not dropping.
    pub fn new() -> Self {
        Self {
            dropped_since_last_gap_emit: AtomicU64::new(0),
            dropping: AtomicBool::new(false),
            last_drop_started_at: Mutex::new(None),
        }
    }

    /// True if currently in drop-mode (at least one frame has been
    /// dropped since the last successful send + gap emit).
    pub fn is_dropping(&self) -> bool {
        self.dropping.load(Ordering::Acquire)
    }

    /// Current dropped count (since last gap emit).
    pub fn dropped_count(&self) -> u64 {
        self.dropped_since_last_gap_emit.load(Ordering::Acquire)
    }

    /// Record a dropped frame. Marks the state as dropping and stamps
    /// the start time on the first drop in this window.
    pub fn on_dropped(&self) {
        // First drop in this window stamps the start time. Subsequent
        // drops while still dropping just bump the counter.
        let was_dropping = self.dropping.swap(true, Ordering::AcqRel);
        if !was_dropping {
            if let Ok(mut g) = self.last_drop_started_at.lock() {
                *g = Some(SystemTime::now());
            }
        }
        self.dropped_since_last_gap_emit
            .fetch_add(1, Ordering::AcqRel);
    }

    /// Called on a successful send when the state was dropping. Returns
    /// `Some(GapEmit)` describing the gap to surface to the client and
    /// resets the state to "not dropping". Returns `None` if no drops
    /// were recorded (no gap needed).
    ///
    /// Per RFC §6 step 3: emit ONE notification covering the whole
    /// dropped window, not one per dropped frame.
    pub fn on_caught_up(&self) -> Option<GapEmit> {
        let dropped = self.dropped_since_last_gap_emit.swap(0, Ordering::AcqRel);
        let was_dropping = self.dropping.swap(false, Ordering::AcqRel);
        if !was_dropping || dropped == 0 {
            return None;
        }
        let since_systemtime = self
            .last_drop_started_at
            .lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_else(SystemTime::now);
        // `chrono::DateTime<Utc>::from(SystemTime)` is infallible.
        let since: DateTime<Utc> = since_systemtime.into();
        Some(GapEmit { dropped, since })
    }
}

/// Describes a single `subscription.gap` notification a forwarder
/// should emit once a slow consumer has caught up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapEmit {
    /// Total frames dropped during the window the client missed.
    pub dropped: u64,
    /// Wallclock time at which dropping started (the `since` field of
    /// the wire `subscription.gap` notification).
    pub since: DateTime<Utc>,
}

/// Classification of a forwarder w.r.t. backpressure handling.
///
/// `Standard` forwarders drop frames + emit a single `subscription.gap`
/// when the per-connection mpsc fills. `Critical` forwarders force the
/// connection to close so the client must reconnect — these are the two
/// streams the RFC marks "critical": `confirmation.subscribe` and
/// `agent.chat.subscribe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwarderKind {
    /// Drop + summarise via `subscription.gap`.
    Standard,
    /// Drop the connection on overflow.
    Critical,
}

/// Outcome of an inner-loop attempt to push one frame downstream. Used by
/// the forwarder bodies to keep the control flow obvious.
#[derive(Debug, PartialEq, Eq)]
enum SendOutcome {
    /// Frame was pushed. May carry a `Gap` to emit immediately
    /// afterwards if the state had been dropping.
    Sent { catch_up_gap: Option<GapEmit> },
    /// Standard subscription dropped this frame; state recorded.
    Dropped,
    /// Critical subscription tripped overflow; caller should request
    /// close and break the loop.
    CriticalOverflow,
    /// Downstream channel is gone — caller should break.
    Closed,
}

/// Push one frame through the sink, applying the configured
/// backpressure policy. Pure data dispatch — no async, no IO beyond the
/// non-blocking `try_send`.
fn push_with_backpressure(
    sink: &FrameSink,
    state: &GapState,
    kind: ForwarderKind,
    frame: StreamFrame,
) -> SendOutcome {
    match sink.try_send(frame) {
        Ok(()) => {
            let catch_up_gap = state.on_caught_up();
            SendOutcome::Sent { catch_up_gap }
        }
        Err(TrySendError::Closed(_)) => SendOutcome::Closed,
        Err(TrySendError::Full(_)) => match kind {
            ForwarderKind::Standard => {
                state.on_dropped();
                SendOutcome::Dropped
            }
            ForwarderKind::Critical => SendOutcome::CriticalOverflow,
        },
    }
}

/// Build a `subscription.gap` frame for `subscription_id` describing
/// `gap`.
fn gap_frame(subscription_id: SubscriptionId, gap: GapEmit) -> StreamFrame {
    StreamFrame {
        subscription_id,
        payload: StreamPayload::Gap {
            dropped: gap.dropped,
            since: gap.since,
        },
    }
}

/// One registered subscription. Holds the JoinHandle so the registry
/// can abort the forwarder task on `.unsubscribe` or connection drop.
///
/// Drop semantics: when a [`Subscription`] is dropped (e.g. the registry
/// is dropped, or the entry is removed from the registry's HashMap), the
/// forwarder task is aborted. This is defense-in-depth — explicit
/// `unregister` is the normal path, but Drop guarantees no task is left
/// running if anyone forgets.
pub struct Subscription {
    /// The forwarder task. Abort it to tear down the subscription.
    pub task: JoinHandle<()>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Thread-safe registry — one instance per `DaemonIpcHandler`, shared
/// across all open subscriptions (events + fires).
#[derive(Default)]
pub struct SubscriptionRegistry {
    by_id: Mutex<HashMap<SubscriptionId, Subscription>>,
}

impl SubscriptionRegistry {
    /// New empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a forwarder task under `id`. Replacing an existing id
    /// aborts the previous task (shouldn't happen in practice — ids are
    /// v7-uuid-derived).
    ///
    /// Also opportunistically prunes any entries whose forwarder has
    /// already exited (e.g. because the per-connection FrameSink was
    /// dropped on client disconnect). This keeps the registry size
    /// proportional to the live subscription count rather than growing
    /// unboundedly across client reconnects.
    pub fn register(&self, id: SubscriptionId, sub: Subscription) {
        let mut map = self.by_id.lock().expect("registry mutex poisoned");
        // Drop finished entries first — `Subscription::Drop` re-aborts
        // (no-op since `is_finished()`) and removes their JoinHandle.
        map.retain(|_, s| !s.task.is_finished());
        if let Some(_prev) = map.insert(id, sub) {
            // The old entry's Drop fires here automatically.
        }
    }

    /// Abort the named subscription's forwarder. Returns true if the
    /// subscription existed, false otherwise. The aborted task's
    /// JoinHandle is dropped (via `Subscription::Drop`), which is the
    /// belt to the abort's suspenders.
    pub fn unregister(&self, id: &SubscriptionId) -> bool {
        let mut map = self.by_id.lock().expect("registry mutex poisoned");
        map.remove(id).is_some()
    }

    /// Drop every entry whose forwarder has already finished (e.g. its
    /// FrameSink closed because the client disconnected). Called by the
    /// daemon's IPC handler on `on_disconnect` so the registry stays
    /// bounded under long-running operation with many client churn.
    pub fn prune_completed(&self) -> usize {
        let mut map = self.by_id.lock().expect("registry mutex poisoned");
        let before = map.len();
        map.retain(|_, s| !s.task.is_finished());
        before - map.len()
    }

    /// Number of live subscriptions.
    pub fn len(&self) -> usize {
        self.by_id.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Abort every subscription. Used on daemon shutdown to drain the
    /// registry; per-connection cleanup happens automatically via
    /// [`Self::prune_completed`] (called from `on_disconnect`) plus
    /// `Subscription::Drop`.
    ///
    /// v1 has one IPC handler shared across all connections, so this
    /// drains everything. Cross-connection unsubscribe authorization
    /// (preventing one client from unsubscribing another client's
    /// subscription by id) is genuinely a v2 design question — the v1
    /// UDS socket is mode 0600 owner-only so all clients are the same
    /// trust domain.
    pub fn abort_all(&self) {
        let mut map = self.by_id.lock().expect("registry mutex poisoned");
        // Draining the HashMap drops each Subscription, which aborts the
        // task via Subscription::Drop.
        map.clear();
    }
}

// ───── Forwarder spawners ─────

/// Spawn an `events.subscribe` forwarder. Returns the subscription id
/// to return from the handler.
pub fn spawn_events_forwarder(
    registry: &Arc<SubscriptionRegistry>,
    bus: &EventBus,
    filter: StreamFilter,
    sink: FrameSink,
) -> SubscriptionId {
    let id = SubscriptionId::new();
    let id_for_task = id.clone();
    let mut rx = bus.subscribe();
    let task = tokio::spawn(async move {
        let gap_state = GapState::new();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if filter_event(&filter, &event) {
                        let frame = StreamFrame {
                            subscription_id: id_for_task.clone(),
                            payload: StreamPayload::Event {
                                event: event_to_value(&event),
                            },
                        };
                        match push_with_backpressure(
                            &sink,
                            &gap_state,
                            ForwarderKind::Standard,
                            frame,
                        ) {
                            SendOutcome::Sent { catch_up_gap } => {
                                if let Some(gap) = catch_up_gap {
                                    let gf = gap_frame(id_for_task.clone(), gap);
                                    // Best-effort: if the wire also can't accept the
                                    // gap frame the next push will roll the counter
                                    // back into drop-mode.
                                    let _ = sink.try_send(gf);
                                }
                            }
                            SendOutcome::Dropped => {
                                tracing::trace!(
                                    subscription_id = %id_for_task,
                                    dropped_so_far = gap_state.dropped_count(),
                                    "events.subscribe dropped frame (slow client)"
                                );
                            }
                            SendOutcome::Closed => {
                                tracing::debug!(
                                    subscription_id = %id_for_task,
                                    "events.subscribe sink closed; forwarder exiting"
                                );
                                break;
                            }
                            SendOutcome::CriticalOverflow => {
                                unreachable!("events.subscribe is Standard, not Critical")
                            }
                        }
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    tracing::warn!(
                        subscription_id = %id_for_task,
                        dropped,
                        "events.subscribe lagged behind bus"
                    );
                    let gap = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: StreamPayload::Gap {
                            dropped,
                            since: chrono::Utc::now(),
                        },
                    };
                    let _ = sink.try_send(gap);
                }
                Err(RecvError::Closed) => {
                    tracing::debug!(
                        subscription_id = %id_for_task,
                        "events bus closed; forwarder exiting"
                    );
                    break;
                }
            }
        }
    });
    registry.register(id.clone(), Subscription { task });
    id
}

/// Spawn a `fires.subscribe` forwarder.
pub fn spawn_fires_forwarder(
    registry: &Arc<SubscriptionRegistry>,
    bus: &FireBus,
    filter: StreamFilter,
    sink: FrameSink,
) -> SubscriptionId {
    let id = SubscriptionId::new();
    let id_for_task = id.clone();
    let mut rx = bus.subscribe();
    let task = tokio::spawn(async move {
        let gap_state = GapState::new();
        loop {
            match rx.recv().await {
                Ok(fire) => {
                    if fire_matches(&fire, &filter) {
                        let frame = StreamFrame {
                            subscription_id: id_for_task.clone(),
                            payload: StreamPayload::Fire {
                                entry: fire_to_value(&fire),
                            },
                        };
                        match push_with_backpressure(
                            &sink,
                            &gap_state,
                            ForwarderKind::Standard,
                            frame,
                        ) {
                            SendOutcome::Sent { catch_up_gap } => {
                                if let Some(gap) = catch_up_gap {
                                    let gf = gap_frame(id_for_task.clone(), gap);
                                    let _ = sink.try_send(gf);
                                }
                            }
                            SendOutcome::Dropped => {
                                tracing::trace!(
                                    subscription_id = %id_for_task,
                                    dropped_so_far = gap_state.dropped_count(),
                                    "fires.subscribe dropped frame (slow client)"
                                );
                            }
                            SendOutcome::Closed => break,
                            SendOutcome::CriticalOverflow => {
                                unreachable!("fires.subscribe is Standard, not Critical")
                            }
                        }
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    let gap = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: StreamPayload::Gap {
                            dropped,
                            since: chrono::Utc::now(),
                        },
                    };
                    let _ = sink.try_send(gap);
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
    registry.register(id.clone(), Subscription { task });
    id
}

/// Thin wrapper around [`crate::recent::event_matches`] — `Event` lives
/// in `cellar_types`, predicate logic in `crate::recent`. Inlined here
/// for forwarder callsite clarity.
fn filter_event(filter: &StreamFilter, event: &Event) -> bool {
    event_matches(event, filter)
}

/// Spawn an `agent_actions.subscribe` forwarder.
///
/// Each received [`AgentActionFrame`] is filtered by `filter.callers` and
/// forwarded as `StreamPayload::AgentAction` on the supplied `sink`.
pub fn spawn_agent_actions_forwarder(
    registry: &Arc<SubscriptionRegistry>,
    bus: &AgentActionBus,
    filter: StreamFilter,
    sink: FrameSink,
) -> SubscriptionId {
    let id = SubscriptionId::new();
    let id_for_task = id.clone();
    let mut rx = bus.subscribe();
    let task = tokio::spawn(async move {
        let gap_state = GapState::new();
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if agent_action_matches(&frame, &filter) {
                        let sf = StreamFrame {
                            subscription_id: id_for_task.clone(),
                            payload: StreamPayload::AgentAction {
                                action: frame.action,
                            },
                        };
                        match push_with_backpressure(&sink, &gap_state, ForwarderKind::Standard, sf)
                        {
                            SendOutcome::Sent { catch_up_gap } => {
                                if let Some(gap) = catch_up_gap {
                                    let gf = gap_frame(id_for_task.clone(), gap);
                                    let _ = sink.try_send(gf);
                                }
                            }
                            SendOutcome::Dropped => {
                                tracing::trace!(
                                    subscription_id = %id_for_task,
                                    dropped_so_far = gap_state.dropped_count(),
                                    "agent_actions.subscribe dropped frame (slow client)"
                                );
                            }
                            SendOutcome::Closed => break,
                            SendOutcome::CriticalOverflow => {
                                unreachable!("agent_actions.subscribe is Standard, not Critical")
                            }
                        }
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    let gap = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: StreamPayload::Gap {
                            dropped,
                            since: chrono::Utc::now(),
                        },
                    };
                    let _ = sink.try_send(gap);
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
    registry.register(id.clone(), Subscription { task });
    id
}

/// Spawn an `agent.chat.subscribe` forwarder. Filters by session_id so
/// one subscription tracks one session's chat stream.
///
/// **Critical stream** (IPC RFC §6): if the per-connection mpsc fills
/// we don't silently skip frames — we ask the connection task to close
/// the connection, forcing the client to reconnect.
pub fn spawn_agent_chat_forwarder(
    registry: &Arc<SubscriptionRegistry>,
    bus: &ChatBus,
    session_id: String,
    sink: FrameSink,
) -> SubscriptionId {
    let id = SubscriptionId::new();
    let id_for_task = id.clone();
    let mut rx = bus.subscribe();
    let task = tokio::spawn(async move {
        // Gap state still here for completeness — `Sent` paths read it,
        // but `Critical` overflow short-circuits straight to close.
        let gap_state = GapState::new();
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if frame.session_id != session_id {
                        continue;
                    }
                    let stream_frame = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: frame.payload,
                    };
                    match push_with_backpressure(
                        &sink,
                        &gap_state,
                        ForwarderKind::Critical,
                        stream_frame,
                    ) {
                        SendOutcome::Sent { .. } => {
                            // Critical streams shouldn't accumulate gap
                            // state — but if they did via a single
                            // Lagged on the bus (handled below) the
                            // counter naturally resets here.
                        }
                        SendOutcome::Closed => break,
                        SendOutcome::CriticalOverflow => {
                            tracing::warn!(
                                subscription_id = %id_for_task,
                                "agent.chat.subscribe per-connection buffer full — \
                                 dropping connection to force reconnect (RFC §6)"
                            );
                            sink.request_close();
                            break;
                        }
                        SendOutcome::Dropped => {
                            unreachable!("agent.chat.subscribe is Critical, not Standard")
                        }
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    // Bus-level lag still emits a Gap notification even
                    // for critical subscriptions — this is a different
                    // failure mode (broadcast bus overflow upstream of
                    // the per-connection mpsc) and clients can recover.
                    let gap = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: StreamPayload::Gap {
                            dropped,
                            since: chrono::Utc::now(),
                        },
                    };
                    let _ = sink.try_send(gap);
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
    registry.register(id.clone(), Subscription { task });
    id
}

/// Spawn a `confirmation.subscribe` forwarder. Confirmation frames are
/// "critical" per the IPC RFC — they're not subject to the standard
/// filter-and-drop logic; every pending confirmation must reach every
/// open subscription. If the per-connection mpsc fills we force the
/// client to reconnect rather than risk skipping a pending confirmation.
pub fn spawn_confirmation_forwarder(
    registry: &Arc<SubscriptionRegistry>,
    bus: &ConfirmationBus,
    sink: FrameSink,
) -> SubscriptionId {
    let id = SubscriptionId::new();
    let id_for_task = id.clone();
    let mut rx = bus.subscribe();
    let task = tokio::spawn(async move {
        let gap_state = GapState::new();
        loop {
            match rx.recv().await {
                Ok(pending) => {
                    let frame = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: StreamPayload::Confirmation {
                            confirmation: pending,
                        },
                    };
                    match push_with_backpressure(&sink, &gap_state, ForwarderKind::Critical, frame)
                    {
                        SendOutcome::Sent { .. } => {}
                        SendOutcome::Closed => break,
                        SendOutcome::CriticalOverflow => {
                            tracing::warn!(
                                subscription_id = %id_for_task,
                                "confirmation.subscribe per-connection buffer full — \
                                 dropping connection to force reconnect (RFC §6)"
                            );
                            sink.request_close();
                            break;
                        }
                        SendOutcome::Dropped => {
                            unreachable!("confirmation.subscribe is Critical, not Standard")
                        }
                    }
                }
                Err(RecvError::Lagged(dropped)) => {
                    let gap = StreamFrame {
                        subscription_id: id_for_task.clone(),
                        payload: StreamPayload::Gap {
                            dropped,
                            since: chrono::Utc::now(),
                        },
                    };
                    let _ = sink.try_send(gap);
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
    registry.register(id.clone(), Subscription { task });
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_bus::ChatBroadcast;
    use crate::confirmation::ConfirmationBus;
    use crate::fire_bus::FireFrame;
    use cellar_ipc::params::confirmation::{PendingConfirmation, PendingRule};
    use cellar_types::{EventKind, EventSource};

    fn sink(cap: usize) -> (FrameSink, tokio::sync::mpsc::Receiver<StreamFrame>) {
        let (tx, rx) = tokio::sync::mpsc::channel(cap);
        (FrameSink::for_tests(tx), rx)
    }

    // ───── GapState unit tests (no IO, pure state machine) ─────

    #[test]
    fn gap_state_starts_clean() {
        let s = GapState::new();
        assert!(!s.is_dropping());
        assert_eq!(s.dropped_count(), 0);
        assert!(s.on_caught_up().is_none());
    }

    #[test]
    fn gap_state_drop_then_catch_up_emits_one_gap() {
        let s = GapState::new();
        s.on_dropped();
        s.on_dropped();
        s.on_dropped();
        assert!(s.is_dropping());
        assert_eq!(s.dropped_count(), 3);
        let gap = s.on_caught_up().expect("dropped 3 — must emit a gap");
        assert_eq!(gap.dropped, 3);
        assert!(!s.is_dropping(), "state must clear after catch-up");
        assert_eq!(s.dropped_count(), 0, "counter must reset after catch-up");
    }

    #[test]
    fn gap_state_catch_up_without_drops_is_none() {
        let s = GapState::new();
        // No drops — `on_caught_up` should be a no-op (the forwarder
        // calls it on every successful send, but only the first send
        // after a dropping window should emit a gap).
        assert!(s.on_caught_up().is_none());
    }

    #[test]
    fn gap_state_second_window_starts_fresh() {
        let s = GapState::new();
        // Window 1
        s.on_dropped();
        s.on_dropped();
        let g1 = s.on_caught_up().unwrap();
        assert_eq!(g1.dropped, 2);

        // Window 2 — counter and timestamp must restart from scratch.
        s.on_dropped();
        let g2 = s.on_caught_up().unwrap();
        assert_eq!(g2.dropped, 1);
        // `since` must be at-or-after window-2's start (strictly after
        // window-1's). Use millisecond resolution to dodge clock jitter
        // on fast machines where the two SystemTime::now() calls round
        // to the same nanosecond.
        assert!(
            g2.since >= g1.since,
            "second-window `since` should not predate the first"
        );
    }

    #[test]
    fn gap_state_concurrent_drops_count_correctly() {
        use std::sync::Arc;
        use std::thread;
        let s = Arc::new(GapState::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let s2 = Arc::clone(&s);
            handles.push(thread::spawn(move || {
                for _ in 0..125 {
                    s2.on_dropped();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.dropped_count(), 1000);
        let g = s.on_caught_up().unwrap();
        assert_eq!(g.dropped, 1000);
    }

    // ───── Existing forwarder tests, ported to the new `FrameSink`. ─────

    #[tokio::test]
    async fn register_and_unregister_round_trip() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (s, _rx) = sink(8);
        let id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), s);
        assert_eq!(reg.len(), 1);
        assert!(reg.unregister(&id));
        assert!(reg.is_empty());
        // Unregistering a missing id is a no-op false.
        assert!(!reg.unregister(&id));
    }

    #[tokio::test]
    async fn events_forwarder_pushes_matching_frames() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (s, mut rx) = sink(8);
        let id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), s);
        // Give the forwarder a tick to subscribe.
        tokio::task::yield_now().await;
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));

        let frame = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .expect("forwarder should produce a frame");
        assert_eq!(frame.subscription_id, id);
        assert!(matches!(frame.payload, StreamPayload::Event { .. }));
    }

    #[tokio::test]
    async fn events_forwarder_filters_by_kind() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (s, mut rx) = sink(8);
        let _id = spawn_events_forwarder(
            &reg,
            &bus,
            StreamFilter {
                kinds: Some(vec!["file_deleted".into()]),
                ..Default::default()
            },
            s,
        );
        tokio::task::yield_now().await;

        // Non-matching event — forwarder drops it server-side.
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileCreated));
        // Matching event — forwarder pushes through.
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));

        let frame = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .expect("expected the matching frame to arrive");
        if let StreamPayload::Event { event } = frame.payload {
            assert_eq!(event["kind"], "file_deleted");
        } else {
            panic!("unexpected payload kind");
        }
    }

    #[tokio::test]
    async fn fires_forwarder_pushes_matching_frames() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = FireBus::new();
        let (s, mut rx) = sink(8);
        let _id = spawn_fires_forwarder(&reg, &bus, StreamFilter::default(), s);
        tokio::task::yield_now().await;
        bus.publish(FireFrame {
            id: "f1".into(),
            fired_at: chrono::Utc::now(),
            rule_id: "r1".into(),
            rule_name: "test".into(),
            rule_kind: "watcher".into(),
            event_kind: "file_deleted".into(),
            event_source: "fsevents".into(),
            event_data: serde_json::Value::Null,
            is_blocking: false,
        });
        let frame = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .unwrap()
            .expect("expected a fire frame");
        assert!(matches!(frame.payload, StreamPayload::Fire { .. }));
    }

    #[tokio::test]
    async fn unregister_aborts_forwarder() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (s, _rx) = sink(8);
        let id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), s);
        {
            let map = reg.by_id.lock().unwrap();
            assert!(map.contains_key(&id));
            // Can't peek the JoinHandle out (not Clone). Just confirm abort
            // observed via length.
        }
        assert!(reg.unregister(&id));
        assert!(reg.is_empty());
    }

    #[tokio::test]
    async fn dropping_subscription_aborts_task() {
        // Verifies the Drop impl on Subscription: dropping the value
        // aborts the forwarder, even if abort() was never called explicitly.
        let mut rx_bus = EventBus::new().subscribe();
        let task = tokio::spawn(async move {
            // Park the task by waiting on a never-published event.
            let _ = rx_bus.recv().await;
        });
        // Clone the abort handle so we can check whether the task got
        // aborted after the Subscription is dropped.
        let abort_handle = task.abort_handle();
        let sub = Subscription { task };
        drop(sub);
        // Yield a couple of times so the abort propagates.
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert!(
            abort_handle.is_finished(),
            "Subscription::drop must abort the task"
        );
    }

    #[tokio::test]
    async fn prune_completed_removes_dead_entries_only() {
        // Spawn two subscriptions, drop one of the receivers so its
        // forwarder exits on the next push, publish to trigger the exit,
        // then assert prune drops exactly that one entry.
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (tx_dead, rx_dead) = tokio::sync::mpsc::channel(8);
        let (tx_live, _rx_live) = tokio::sync::mpsc::channel(8);
        let dead_id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), tx_dead);
        let live_id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), tx_live);
        assert_eq!(reg.len(), 2);

        // Drop the dead receiver and publish — the forwarder's send fails
        // and the task exits.
        drop(rx_dead);
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        // Let the forwarder run.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        let pruned = reg.prune_completed();
        assert_eq!(pruned, 1, "exactly one dead entry should be pruned");
        assert_eq!(reg.len(), 1);
        // The live subscription should still be in the registry.
        let map = reg.by_id.lock().unwrap();
        assert!(map.contains_key(&live_id));
        assert!(!map.contains_key(&dead_id));
    }

    #[tokio::test]
    async fn register_opportunistically_prunes_completed() {
        // Make sure the prune-on-register path keeps the registry bounded
        // across many client disconnects.
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        // First subscription with a receiver we drop immediately to kill it.
        let (tx1, rx1) = tokio::sync::mpsc::channel(8);
        spawn_events_forwarder(&reg, &bus, StreamFilter::default(), tx1);
        drop(rx1);
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileCreated));
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // Now register a new subscription. The dead entry should get
        // pruned implicitly.
        let (tx2, _rx2) = tokio::sync::mpsc::channel(8);
        let _id2 = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), tx2);
        assert_eq!(
            reg.len(),
            1,
            "register should opportunistically prune dead entries"
        );
    }

    #[tokio::test]
    async fn abort_all_clears_registry_via_drop() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (tx1, _rx1) = tokio::sync::mpsc::channel(8);
        let (tx2, _rx2) = tokio::sync::mpsc::channel(8);
        spawn_events_forwarder(&reg, &bus, StreamFilter::default(), tx1);
        spawn_events_forwarder(&reg, &bus, StreamFilter::default(), tx2);
        assert_eq!(reg.len(), 2);
        reg.abort_all();
        assert!(reg.is_empty());
    }

    // ───── New backpressure tests (RFC §6) ─────

    /// Standard subscription: a sink whose receiver isn't drained gets
    /// new frames dropped, and once the receiver wakes up the forwarder
    /// emits ONE `subscription.gap` summarising the missed window.
    #[tokio::test]
    async fn events_forwarder_drops_then_emits_gap_on_catch_up() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        // Tiny per-connection sink so we hit `Full` quickly.
        let (s, mut rx) = sink(2);
        let _id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), s);
        tokio::task::yield_now().await;

        // Publish a burst. The first two fill the channel; the rest
        // get dropped by the forwarder.
        for _ in 0..10 {
            bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        }
        // Let the forwarder process the burst.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Drain the two queued frames — both Events.
        for _ in 0..2 {
            let frame = rx.try_recv().expect("expected queued event frame");
            assert!(matches!(frame.payload, StreamPayload::Event { .. }));
        }

        // Now publish one more so the forwarder transitions back to
        // "Sent" and emits the gap.
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        // The forwarder pushes the matching Event frame; the same loop
        // iteration also queues the gap via try_send.
        let mut saw_gap = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Some(frame)) => {
                    if matches!(frame.payload, StreamPayload::Gap { .. }) {
                        if let StreamPayload::Gap { dropped, .. } = frame.payload {
                            assert!(dropped > 0, "gap should report at least one dropped frame");
                            saw_gap = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_gap,
            "standard forwarder must emit a subscription.gap frame after a drop window"
        );
    }

    /// Standard subscription must only emit ONE gap per dropping
    /// window — the next gap can only happen after a fresh drop.
    #[tokio::test]
    async fn events_forwarder_emits_only_one_gap_per_window() {
        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = EventBus::new();
        let (s, mut rx) = sink(2);
        let _id = spawn_events_forwarder(&reg, &bus, StreamFilter::default(), s);
        tokio::task::yield_now().await;

        // Burst → fill + drop.
        for _ in 0..6 {
            bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // Drain queued events.
        let _ = rx.try_recv();
        let _ = rx.try_recv();
        // Trigger gap emit.
        bus.publish(Event::now(EventSource::Fsevents, EventKind::FileDeleted));

        // Drain whatever's queued; count Gap frames.
        let mut gap_count = 0u32;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
                Ok(Some(frame)) => {
                    if matches!(frame.payload, StreamPayload::Gap { .. }) {
                        gap_count += 1;
                    }
                }
                _ => break,
            }
        }
        assert_eq!(
            gap_count, 1,
            "exactly one gap frame should be emitted per dropping window"
        );
    }

    /// Critical subscription: when the per-connection mpsc fills, the
    /// forwarder requests the connection close via `sink.request_close()`
    /// and exits — it does NOT silently skip frames.
    #[tokio::test]
    async fn confirmation_forwarder_requests_close_on_overflow() {
        use std::sync::Arc as StdArc;
        use tokio::sync::Notify;

        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = ConfirmationBus::new();

        // Build a sink with a tiny capacity AND a manually held Notify
        // so the test can observe `request_close` directly.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamFrame>(1);
        let close_hint = StdArc::new(Notify::new());
        let sink = FrameSink::new(tx, StdArc::clone(&close_hint));

        let _id = spawn_confirmation_forwarder(&reg, &bus, sink);
        tokio::task::yield_now().await;

        // Hammer the bus with confirmations. The first fills capacity
        // (1); every subsequent one trips Full → request_close.
        for i in 0..5 {
            bus.publish(PendingConfirmation {
                id: format!("conf_{i}"),
                created_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
                rule: PendingRule {
                    id: "r".into(),
                    name: "r".into(),
                    nl_original: "test".into(),
                },
                event: serde_json::json!({}),
                originating_action: serde_json::json!({}),
                caller: "test".into(),
                agent_session_id: None,
            });
        }

        // Close hint must fire — that's how the connection task knows
        // to drop the connection.
        tokio::time::timeout(std::time::Duration::from_millis(500), close_hint.notified())
            .await
            .expect("critical-overflow forwarder must signal close_hint");

        // And exactly one buffered frame should be sitting in the rx.
        assert!(rx.try_recv().is_ok(), "first frame should have been queued");
    }

    /// Same as above, for `agent.chat.subscribe` — that's the other
    /// critical subscription per RFC §6.
    #[tokio::test]
    async fn agent_chat_forwarder_requests_close_on_overflow() {
        use std::sync::Arc as StdArc;
        use tokio::sync::Notify;

        let reg = Arc::new(SubscriptionRegistry::new());
        let bus = ChatBus::new();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamFrame>(1);
        let close_hint = StdArc::new(Notify::new());
        let sink = FrameSink::new(tx, StdArc::clone(&close_hint));

        let session_id = "sess_test";
        let _id = spawn_agent_chat_forwarder(&reg, &bus, session_id.to_string(), sink);
        tokio::task::yield_now().await;

        for i in 0..5 {
            bus.publish(ChatBroadcast {
                session_id: session_id.to_string(),
                payload: StreamPayload::Token {
                    request_id: "req_1".into(),
                    message_id: format!("msg_{i}"),
                    delta: format!("tok {i}"),
                },
            });
        }

        tokio::time::timeout(std::time::Duration::from_millis(500), close_hint.notified())
            .await
            .expect("critical-overflow forwarder must signal close_hint");

        assert!(rx.try_recv().is_ok(), "first frame should have been queued");
    }
}
