//! Signals poller — the third ambient event source for Cellar.
//!
//! Polls [`cel_signals::SignalBus`] periodically, diffs against the previous
//! snapshot, and publishes [`EventKind::AppFocused`] when the frontmost
//! application changes and [`EventKind::WindowOpened`] when a new visible
//! window appears. Both are tagged with [`EventSource::CortexAx`] because
//! they're the AX-derived perception slice the cortex would surface in
//! goalless mode — except here we read the underlying signals directly
//! rather than dragging the full cortex (and its AX-observer, CDP client,
//! audio, input, store, context-merger pipeline) into the daemon.
//!
//! Why not the full `cel-cortex`:
//!
//! - The cortex exposes no event stream — it maintains `Arc<RwLock<MentalModel>>`
//!   that callers poll. So whichever path we take, this source is "poll + diff."
//! - The cortex pulls AX + CDP + audio + input + signals + network + store
//!   into the daemon's dep graph. For the v1 matcher, only the frontmost-app
//!   and window-focus slice matters.
//! - The cortex's value (skeleton detection, anomaly detection, element
//!   stability) is for goal-driven agent perception. The matcher's needs are
//!   strictly weaker: "did the frontmost change? did a window open?"
//!
//! v1 scope (Phase 1):
//! - `AppFocused` published with `data.app = <name>`.
//! - `WindowOpened` published with `data.app`, `data.title`, `data.pid`.
//! - **`UrlChanged` is intentionally absent** — that one needs a CDP client
//!   talking to the user's Chrome (with `--remote-debugging-port`). Lands as
//!   its own source (`EventSource::CortexCdp`) in a follow-up.
//! - First poll establishes a baseline without emitting events, same as
//!   `process_poller`, so we don't claim every running app "just focused"
//!   at startup.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use cel_signals::{SignalBus, WindowState};
use cellar_types::{Event, EventKind, EventSource};
use tokio::task::JoinHandle;

use crate::bus::EventBus;

/// Default polling interval. App/window focus changes feel interactive, so
/// the cadence is tighter than the process poller's 2 s.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(1500);

/// Signals-poller configuration.
#[derive(Debug, Clone)]
pub struct SignalsPollerConfig {
    /// How often to re-poll the signal bus.
    pub interval: Duration,
}

impl Default for SignalsPollerConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
        }
    }
}

/// One window's stable identity for the diff: `(pid, title)`. A window's
/// pid is stable across title changes, but two browser windows in the same
/// process have distinct (pid, title) pairs as long as their titles differ,
/// which is the common case.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct WindowKey {
    /// Owning process id (same as the OS-level pid the process poller sees).
    pub pid: u32,
    /// Window title at snapshot time.
    pub title: String,
}

/// One window's metadata at a single moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMeta {
    /// Owning app name (e.g., `"Safari"`, `"Slack"`).
    pub app_name: String,
    /// Window title.
    pub title: String,
    /// Owning process id.
    pub pid: u32,
}

/// Snapshot of the desktop's app + window focus state at one moment in time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowFocusSnapshot {
    /// The frontmost app's name, if any was reported.
    pub frontmost_app: Option<String>,
    /// All visible normal-layer windows on screen, keyed by `(pid, title)`.
    pub windows: HashMap<WindowKey, WindowMeta>,
}

/// Spawn the signals-poller task. Returns the [`JoinHandle`] so the daemon
/// can `.await` shutdown if desired.
///
/// The task exits when the [`EventBus`] is closed (i.e. when the daemon
/// drops its last `EventBus` clone — typically at shutdown).
pub fn spawn(
    bus: &EventBus,
    signals: Arc<dyn SignalBus>,
    cfg: SignalsPollerConfig,
) -> JoinHandle<()> {
    let bus = bus.clone();
    tokio::spawn(async move {
        tracing::info!(
            interval_ms = cfg.interval.as_millis() as u64,
            "signals poller started"
        );

        // Baseline: take one snapshot, emit nothing.
        let mut last = snapshot(signals.as_ref());
        tracing::debug!(
            frontmost = ?last.frontmost_app,
            initial_windows = last.windows.len(),
            "signals poller baseline established"
        );

        let mut interval = tokio::time::interval(cfg.interval);
        // First tick fires immediately; we already took the baseline.
        interval.tick().await;

        loop {
            interval.tick().await;
            let now = snapshot(signals.as_ref());

            let events = diff_to_events(&last, &now);
            if !events.is_empty() {
                tracing::trace!(events = events.len(), "signals diff emitting events");
            }
            for event in events {
                bus.publish(event);
            }

            last = now;
        }
    })
}

/// Build a [`WindowFocusSnapshot`] from one [`SignalBus::snapshot`].
///
/// Public so tests and future callers can substitute their own snapshot
/// pairs to drive [`diff_to_events`] without going through a real bus.
pub fn snapshot(signals: &dyn SignalBus) -> WindowFocusSnapshot {
    let snap = signals.snapshot();

    let frontmost_app = snap
        .running_apps
        .iter()
        .find(|a| a.is_frontmost)
        .map(|a| a.name.clone());

    let windows = snap
        .window_list
        .into_iter()
        .filter(keep_window)
        .map(|w| {
            (
                WindowKey {
                    pid: w.pid,
                    title: w.title.clone(),
                },
                WindowMeta {
                    app_name: w.app_name,
                    title: w.title,
                    pid: w.pid,
                },
            )
        })
        .collect();

    WindowFocusSnapshot {
        frontmost_app,
        windows,
    }
}

/// Filter out windows the matcher will never care about — minimized windows
/// and floating/overlay layers (`layer != 0`: menu bars, tooltips, etc.).
fn keep_window(w: &WindowState) -> bool {
    w.is_on_screen && w.layer == 0
}

/// Pure diff: given two snapshots, return the events to publish.
///
/// - `frontmost_app` changed → `app_focused` event for the new frontmost.
///   No event is emitted when the frontmost goes from `Some` to `None`
///   (e.g., during a window transition where no app reports frontmost) —
///   `AppFocused` always names the new focused app, never "nothing has focus."
/// - New `(pid, title)` window in `now` but not `last` → `window_opened`.
/// - Window disappearing from `now` does not currently emit an event;
///   `cellar-types` doesn't have a `WindowClosed` kind in v1 and adding one
///   would change the locked enum.
pub fn diff_to_events(last: &WindowFocusSnapshot, now: &WindowFocusSnapshot) -> Vec<Event> {
    let mut events = Vec::new();

    if now.frontmost_app != last.frontmost_app {
        if let Some(app) = &now.frontmost_app {
            events.push(
                Event::now(EventSource::CortexAx, EventKind::AppFocused)
                    .with_data("app", app.clone()),
            );
        }
    }

    for (key, meta) in &now.windows {
        if !last.windows.contains_key(key) {
            events.push(
                Event::now(EventSource::CortexAx, EventKind::WindowOpened)
                    .with_data("app", meta.app_name.clone())
                    .with_data("title", meta.title.clone())
                    .with_data("pid", meta.pid),
            );
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_signals::{RunningApp, SignalSnapshot, WindowState};

    /// A tiny in-memory `SignalBus` for tests: returns whatever snapshot
    /// the test loaded into it.
    struct MockSignalBus {
        snap: std::sync::Mutex<SignalSnapshot>,
    }

    impl MockSignalBus {
        fn new() -> Self {
            Self {
                snap: std::sync::Mutex::new(SignalSnapshot::default()),
            }
        }
        fn set(&self, snap: SignalSnapshot) {
            *self.snap.lock().unwrap() = snap;
        }
    }

    impl SignalBus for MockSignalBus {
        fn snapshot(&self) -> SignalSnapshot {
            self.snap.lock().unwrap().clone()
        }
    }

    fn app(name: &str, frontmost: bool) -> RunningApp {
        RunningApp {
            name: name.into(),
            is_frontmost: frontmost,
        }
    }

    fn win(app_name: &str, title: &str, pid: u32) -> WindowState {
        WindowState {
            app_name: app_name.into(),
            title: title.into(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            layer: 0,
            is_on_screen: true,
            pid,
        }
    }

    fn snap_with(apps: Vec<RunningApp>, windows: Vec<WindowState>) -> WindowFocusSnapshot {
        let bus = MockSignalBus::new();
        bus.set(SignalSnapshot {
            running_apps: apps,
            window_list: windows,
            ..Default::default()
        });
        snapshot(&bus)
    }

    #[test]
    fn snapshot_picks_frontmost_app() {
        let s = snap_with(
            vec![app("Slack", false), app("Safari", true), app("Mail", false)],
            vec![],
        );
        assert_eq!(s.frontmost_app.as_deref(), Some("Safari"));
    }

    #[test]
    fn snapshot_returns_none_frontmost_when_no_app_is() {
        let s = snap_with(vec![app("Slack", false), app("Mail", false)], vec![]);
        assert!(s.frontmost_app.is_none());
    }

    #[test]
    fn snapshot_filters_offscreen_and_non_normal_layer_windows() {
        let mut off = win("Safari", "Hidden", 100);
        off.is_on_screen = false;
        let mut overlay = win("Tooltip", "Hover", 101);
        overlay.layer = 5;
        let normal = win("Safari", "GitHub - cellar", 100);

        let s = snap_with(vec![], vec![off, overlay, normal.clone()]);
        assert_eq!(s.windows.len(), 1);
        let only = s.windows.values().next().unwrap();
        assert_eq!(only.title, "GitHub - cellar");
        assert_eq!(only.pid, 100);
    }

    #[test]
    fn diff_emits_app_focused_on_frontmost_change() {
        let last = snap_with(vec![app("Slack", true)], vec![]);
        let now = snap_with(vec![app("Safari", true)], vec![]);

        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, EventKind::AppFocused);
        assert_eq!(e.source, EventSource::CortexAx);
        assert_eq!(e.data["app"], "Safari");
    }

    #[test]
    fn diff_no_event_when_frontmost_unchanged() {
        let last = snap_with(vec![app("Slack", true)], vec![win("Slack", "main", 1)]);
        let now = snap_with(vec![app("Slack", true)], vec![win("Slack", "main", 1)]);
        assert!(diff_to_events(&last, &now).is_empty());
    }

    #[test]
    fn diff_emits_window_opened_for_new_window() {
        let last = snap_with(vec![], vec![win("Safari", "Home", 1)]);
        let now = snap_with(
            vec![],
            vec![win("Safari", "Home", 1), win("Safari", "GitHub", 1)],
        );

        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, EventKind::WindowOpened);
        assert_eq!(e.source, EventSource::CortexAx);
        assert_eq!(e.data["app"], "Safari");
        assert_eq!(e.data["title"], "GitHub");
        assert_eq!(e.data["pid"], 1);
    }

    #[test]
    fn diff_emits_app_focused_and_window_opened_together() {
        let last = snap_with(vec![app("Slack", true)], vec![]);
        let now = snap_with(vec![app("Safari", true)], vec![win("Safari", "GitHub", 2)]);

        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 2);
        let kinds: Vec<&EventKind> = events.iter().map(|e| &e.kind).collect();
        assert!(kinds.contains(&&EventKind::AppFocused));
        assert!(kinds.contains(&&EventKind::WindowOpened));
    }

    #[test]
    fn diff_no_event_when_window_disappears() {
        // WindowClosed isn't an EventKind in v1 — disappearing windows are
        // silently dropped from the snapshot.
        let last = snap_with(vec![], vec![win("Safari", "Home", 1)]);
        let now = snap_with(vec![], vec![]);
        assert!(diff_to_events(&last, &now).is_empty());
    }

    #[test]
    fn diff_no_event_when_frontmost_goes_to_none() {
        // Transitional state where no app reports frontmost shouldn't fire
        // an AppFocused for the previous app.
        let last = snap_with(vec![app("Safari", true)], vec![]);
        let now = snap_with(vec![app("Safari", false)], vec![]);
        assert!(diff_to_events(&last, &now).is_empty());
    }

    /// Real-system smoke test: pull a snapshot from the platform signal bus
    /// and verify it has a reasonable shape. Doesn't assert specific values
    /// because the test runner's desktop state is unknown.
    #[test]
    fn snapshot_against_real_signal_bus_does_not_panic() {
        let bus = cel_signals::PlatformSignalBus::new();
        let s = snapshot(&bus);
        // No assertions on frontmost — the test runner may have nothing focused.
        // Just confirm the call doesn't panic and the shape is well-formed.
        for (key, meta) in &s.windows {
            assert_eq!(key.pid, meta.pid);
            assert_eq!(key.title, meta.title);
        }
    }

    /// End-to-end: spawn the poller with a mock signal bus, mutate the
    /// mock between ticks, observe the matching events on the bus.
    #[tokio::test]
    async fn spawn_emits_events_when_mock_signals_change() {
        use std::time::Duration;

        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        let mock = Arc::new(MockSignalBus::new());
        // Baseline: Safari frontmost, one window.
        mock.set(SignalSnapshot {
            running_apps: vec![app("Safari", true)],
            window_list: vec![win("Safari", "Home", 100)],
            ..Default::default()
        });

        let handle = spawn(
            &bus,
            mock.clone(),
            SignalsPollerConfig {
                interval: Duration::from_millis(50),
            },
        );

        // Give the task a tick to take its baseline.
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Switch frontmost to Slack and open a new window.
        mock.set(SignalSnapshot {
            running_apps: vec![app("Slack", true)],
            window_list: vec![win("Safari", "Home", 100), win("Slack", "general", 200)],
            ..Default::default()
        });

        // Collect a few events with a tight deadline.
        let saw = collect_events(&mut rx, Duration::from_secs(1), 2).await;
        let kinds: Vec<EventKind> = saw.iter().map(|e| e.kind.clone()).collect();
        assert!(
            kinds.contains(&EventKind::AppFocused),
            "expected AppFocused in {kinds:?}"
        );
        assert!(
            kinds.contains(&EventKind::WindowOpened),
            "expected WindowOpened in {kinds:?}"
        );

        // Clean up: dropping the bus closes the channel; cancel the task
        // explicitly so the test runtime can tear down promptly.
        handle.abort();
        let _ = handle.await;
    }

    async fn collect_events(
        rx: &mut tokio::sync::broadcast::Receiver<Event>,
        deadline: Duration,
        wanted: usize,
    ) -> Vec<Event> {
        let mut out = Vec::new();
        let until = tokio::time::Instant::now() + deadline;
        while out.len() < wanted {
            let remaining = until.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) => out.push(ev),
                _ => break,
            }
        }
        out
    }
}
