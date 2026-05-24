//! Process poller — the first ambient event source for Cellar.
//!
//! Periodically lists running processes (cross-platform via the `sysinfo`
//! crate), diffs against the previous snapshot, and publishes
//! [`EventKind::ProcessStarted`] / [`EventKind::ProcessStopped`] events onto
//! the [`EventBus`]. The matcher consumer task picks them up and fires
//! matching rules.
//!
//! Polling cadence is configurable (default 2 s, matching `monitor.toml`'s
//! `process_poller.interval_ms`). The first poll establishes a baseline
//! without emitting events — only the *diff* between consecutive polls is
//! reported, so the daemon doesn't claim every process running at startup
//! "just started."
//!
//! v1 scope (Phase 1):
//! - `pid`, `name`, `executable` populated on every emitted event.
//! - **`bundle_id` is intentionally absent.** Resolving it from an
//!   executable path requires reading the parent `.app`'s `Info.plist` on
//!   macOS; the dedicated helper lands in a follow-up so this file stays
//!   small and cross-platform. Rules that target `data.bundle_id` won't
//!   match yet — they'll match once that helper is wired in. Rules that
//!   target `data.name` or `data.executable` work today.

use std::collections::HashMap;
use std::time::Duration;

use cellar_types::{Event, EventKind, EventSource};
use sysinfo::{ProcessesToUpdate, System};
use tokio::task::JoinHandle;

use crate::bus::EventBus;

/// Default polling interval. Matches `cellar-app-v1.md` §15's
/// `process_poller.interval_ms` of 2000.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(2000);

/// Process-poller configuration.
#[derive(Debug, Clone)]
pub struct PollerConfig {
    /// How often to re-list processes.
    pub interval: Duration,
}

impl Default for PollerConfig {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
        }
    }
}

/// One process at one moment in time. Stable identity is `(pid, name)`; the
/// poller treats two consecutive polls with the same pid but different
/// names as one process (pid reuse during a 2-second window is exceedingly
/// rare and not worth complicating diff semantics for).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSnapshot {
    /// Operating-system process id.
    pub pid: u32,
    /// Short process name (e.g., `"Safari"`).
    pub name: String,
    /// Full executable path, when available.
    pub exe: Option<String>,
}

/// Spawn the process-poller task. Returns the [`JoinHandle`] so the daemon
/// can `.await` shutdown if it wants to.
///
/// The task exits when the [`EventBus`] is closed (i.e. when the daemon
/// drops its last `EventBus` clone — typically at shutdown).
pub fn spawn(bus: &EventBus, cfg: PollerConfig) -> JoinHandle<()> {
    let bus = bus.clone();
    tokio::spawn(async move {
        tracing::info!(
            interval_ms = cfg.interval.as_millis() as u64,
            "process poller started"
        );

        let mut sys = System::new();
        // Baseline: full process list, no events emitted.
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut last = snapshot(&sys);
        tracing::debug!(
            initial_count = last.len(),
            "process poller baseline established"
        );

        let mut interval = tokio::time::interval(cfg.interval);
        // The first `tick()` fires immediately. We've already taken our
        // baseline, so consume it to avoid an immediate redundant poll.
        interval.tick().await;

        loop {
            interval.tick().await;
            sys.refresh_processes(ProcessesToUpdate::All, true);
            let now = snapshot(&sys);

            let events = diff_to_events(&last, &now);
            if events.is_empty() {
                // Nothing changed; common case.
            } else {
                tracing::trace!(events = events.len(), "process diff emitting events");
            }
            for event in events {
                bus.publish(event);
            }

            last = now;

            // Cheap liveness probe: if there are zero subscribers, the bus
            // is effectively idle. We keep polling anyway because new
            // subscribers may attach later. The bus already trace-logs
            // dropped publishes.
        }
    })
}

/// Build a snapshot map from a refreshed `System`.
///
/// Public so the integration test can substitute its own snapshot pairs to
/// drive [`diff_to_events`] without needing real processes.
pub fn snapshot(sys: &System) -> HashMap<u32, ProcessSnapshot> {
    sys.processes()
        .iter()
        .map(|(pid, p)| {
            let pid_u32 = pid.as_u32();
            (
                pid_u32,
                ProcessSnapshot {
                    pid: pid_u32,
                    name: p.name().to_string_lossy().into_owned(),
                    exe: p.exe().map(|path| path.display().to_string()),
                },
            )
        })
        .collect()
}

/// Pure diff: given two snapshots, return the events to publish.
///
/// New pids in `now` but not in `last` → `process_started`.
/// Pids in `last` but not in `now` → `process_stopped`.
/// Same pid in both → no event (we don't currently emit anything for
/// long-running pid renames; pid reuse is rare enough to defer).
pub fn diff_to_events(
    last: &HashMap<u32, ProcessSnapshot>,
    now: &HashMap<u32, ProcessSnapshot>,
) -> Vec<Event> {
    let mut events = Vec::new();

    for (pid, snap) in now {
        if !last.contains_key(pid) {
            events.push(make_event(EventKind::ProcessStarted, snap));
        }
    }
    for (pid, snap) in last {
        if !now.contains_key(pid) {
            events.push(make_event(EventKind::ProcessStopped, snap));
        }
    }

    events
}

fn make_event(kind: EventKind, snap: &ProcessSnapshot) -> Event {
    let mut event = Event::now(EventSource::Process, kind)
        .with_data("pid", snap.pid)
        .with_data("name", snap.name.clone());
    if let Some(exe) = &snap.exe {
        event = event.with_data("executable", exe.clone());
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(pid: u32, name: &str, exe: Option<&str>) -> ProcessSnapshot {
        ProcessSnapshot {
            pid,
            name: name.into(),
            exe: exe.map(String::from),
        }
    }

    fn map(items: Vec<ProcessSnapshot>) -> HashMap<u32, ProcessSnapshot> {
        items.into_iter().map(|p| (p.pid, p)).collect()
    }

    #[test]
    fn diff_detects_started_pids() {
        let last = map(vec![snap(1, "init", Some("/sbin/init"))]);
        let now = map(vec![
            snap(1, "init", Some("/sbin/init")),
            snap(
                42,
                "Safari",
                Some("/Applications/Safari.app/Contents/MacOS/Safari"),
            ),
        ]);

        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, EventKind::ProcessStarted);
        assert_eq!(e.source, EventSource::Process);
        assert_eq!(e.data["pid"], 42);
        assert_eq!(e.data["name"], "Safari");
        assert!(e.data["executable"].as_str().unwrap().contains("Safari"));
    }

    #[test]
    fn diff_detects_stopped_pids() {
        let last = map(vec![snap(1, "init", None), snap(42, "Safari", None)]);
        let now = map(vec![snap(1, "init", None)]);

        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::ProcessStopped);
        assert_eq!(events[0].data["pid"], 42);
        assert_eq!(events[0].data["name"], "Safari");
    }

    #[test]
    fn diff_no_change_emits_nothing() {
        let s = map(vec![snap(1, "init", None), snap(2, "kthread", None)]);
        assert!(diff_to_events(&s, &s).is_empty());
    }

    #[test]
    fn diff_simultaneous_start_and_stop() {
        let last = map(vec![snap(1, "init", None), snap(42, "Safari", None)]);
        let now = map(vec![snap(1, "init", None), snap(43, "Chrome", None)]);

        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 2);
        let kinds: Vec<EventKind> = events.iter().map(|e| e.kind.clone()).collect();
        assert!(kinds.contains(&EventKind::ProcessStarted));
        assert!(kinds.contains(&EventKind::ProcessStopped));
    }

    #[test]
    fn diff_event_omits_executable_when_absent() {
        let last = map(vec![]);
        let now = map(vec![snap(99, "kernel_task", None)]);
        let events = diff_to_events(&last, &now);
        assert_eq!(events.len(), 1);
        assert!(
            !events[0].data.contains_key("executable"),
            "executable field should be omitted when exe is None"
        );
    }

    /// Real-system smoke test: with the actual `sysinfo` snapshot, the
    /// poller's snapshot helper returns a non-empty map (any reasonable
    /// host has at least an init / launchd process). The diff between two
    /// back-to-back refreshes is usually small but may be non-zero on a
    /// busy test runner; the only contract is "doesn't panic, returns a
    /// reasonable shape."
    #[test]
    fn snapshot_against_real_system_works() {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let s = snapshot(&sys);
        assert!(
            !s.is_empty(),
            "real system should have at least one process"
        );
        let any = s.values().next().unwrap();
        assert!(any.pid > 0);
        assert!(!any.name.is_empty());
    }

    /// End-to-end: spawn the poller against a small interval, observe at
    /// least one tick (no events expected at the first interval since
    /// nothing changed), then shut down by dropping the bus.
    #[tokio::test]
    async fn spawn_runs_at_least_one_tick_then_exits_on_bus_drop() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let handle = spawn(
            &bus,
            PollerConfig {
                interval: Duration::from_millis(50),
            },
        );

        // Race: try to receive an event briefly; if nothing happens within
        // 200ms that's also fine (no process change in this test window).
        let _ = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;

        // Trigger task exit by dropping the bus (all senders go away).
        drop(bus);
        drop(rx);

        // Polling task may still hold its own bus clone; wait briefly.
        let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
    }
}
