//! Event envelope and kind catalog.
//!
//! Every event source emits this uniform shape. The matcher evaluates over
//! `Event` regardless of which adapter produced it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The uniform event envelope.
///
/// `data` is intentionally a sorted map (`BTreeMap`) so serialization is
/// stable for hashing, fired-log diffing, and human-readable logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// When the event was observed.
    pub ts: DateTime<Utc>,
    /// Where the event came from.
    pub source: EventSource,
    /// What kind of event this is.
    pub kind: EventKind,
    /// Source-specific payload, addressable from rule expressions via dotted
    /// field paths (e.g., `data.path`, `data.action_args.target_path`).
    #[serde(default)]
    pub data: BTreeMap<String, Value>,
}

/// All event sources known to v1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// macOS Accessibility tree (focus, window, app changes).
    CortexAx,
    /// Chrome DevTools Protocol (URL changes, page loads).
    CortexCdp,
    /// OS-level network connection monitor (lsof / /proc/net/tcp).
    CortexNetwork,
    /// Audio capture + transcription stream.
    CortexAudio,
    /// Keyboard/pointer input capture (CGEventTap). Content-bearing —
    /// keystroke text and pointer coordinates are governance-sensitive.
    CortexInput,
    /// Process start/stop from the poller.
    Process,
    /// Filesystem events from FSEvents.
    Fsevents,
    /// Synthetic events emitted by the `cel_act` gateway.
    CelActGateway,
    /// Synthetic events emitted by the memory subsystem (every write,
    /// every off-device call). Lets the rule matcher govern memory
    /// itself — e.g., redact-rule users can author "never persist any
    /// chunk mentioning bank.example.com".
    Memory,
}

/// Catalog of event kinds.
///
/// `Other(String)` is the escape hatch for kinds added by future adapters
/// without a code change here. The matcher treats it transparently — rules
/// can match on `Other("...")` via `eq` on the kind field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    // CortexAx — application + window lifecycle
    /// An app gained keyboard focus (frontmost).
    AppFocused,
    /// An app was sent to the background (lost frontmost).
    AppDeactivated,
    /// An app was hidden (⌘H).
    AppHidden,
    /// A hidden app was shown again.
    AppShown,
    /// A new window was opened. `data.title`.
    WindowOpened,
    /// A window was moved.
    WindowMoved,
    /// A window was resized.
    WindowResized,
    /// A window was minimized.
    WindowMinimized,
    /// A window was restored from the minimized state.
    WindowRestored,
    /// The app's main window changed (distinct from focus change).
    MainWindowChanged,
    /// A menu was opened.
    MenuOpened,
    /// A menu was closed.
    MenuClosed,
    /// A sheet/dialog appeared.
    SheetOpened,

    // CortexAx — element-level (high-frequency; forwarded per user policy)
    /// Keyboard/mouse focus moved to a different element. `data.element_id`.
    FocusChanged,
    /// An element's value changed (text, checkbox, slider).
    /// `data.element_id`, `data.new_value`.
    ValueChanged,
    /// An element's title changed. `data.element_id`, `data.new_title`.
    TitleChanged,
    /// A text/list selection changed.
    SelectionChanged,
    /// The number of rows in a table/outline changed.
    RowCountChanged,
    /// UI layout changed (elements added/removed/repositioned).
    LayoutChanged,
    /// An element was destroyed/removed from the tree.
    ElementDestroyed,
    /// A screen-reader announcement was requested. `data.text`.
    AnnouncementRequested,
    /// A tooltip/help tag was shown.
    HelpTagShown,

    // CortexCdp
    /// The active browser tab navigated to a new URL. `data.url`.
    UrlChanged,
    /// The active browser tab fired its load event (`Page.loadEventFired`) —
    /// the page finished loading. `data.timestamp` (CDP monotonic clock).
    PageLoaded,

    // CortexNetwork
    /// A new TCP/UDP connection was observed. `data.remote_addr`,
    /// `data.remote_port`, `data.service`, `data.process_name`, `data.pid`.
    NetworkConnectionOpened,
    /// A previously-observed connection closed.
    NetworkConnectionClosed,

    // CortexAudio
    /// Audio capture started.
    AudioCaptureStarted,
    /// Audio capture stopped.
    AudioCaptureStopped,
    /// A transcript segment was produced. `data.text`, `data.source`,
    /// `data.speaker`. Content-bearing — rules can `Veto`/`RequireConfirmation`
    /// before the text is persisted or leaves the device.
    AudioTranscript,

    // CortexInput (content-bearing; governance-sensitive)
    /// A key was pressed (`data.pressed = true`) or released (`false`).
    /// `data.keycode`; `data.text` is attached only when content forwarding is
    /// explicitly enabled.
    KeyboardInput,
    /// The pointer moved. `data.x`, `data.y`.
    PointerMoved,
    /// A pointer button changed. `data.button`, `data.pressed`, `data.x`,
    /// `data.y`.
    PointerButton,
    /// A scroll-wheel event. `data.delta_x`, `data.delta_y`.
    PointerScroll,

    // Process
    /// A new process started.
    ProcessStarted,
    /// A process exited.
    ProcessStopped,

    // Fsevents
    /// A file was created.
    FileCreated,
    /// A file was modified.
    FileModified,
    /// A file was deleted.
    FileDeleted,

    // cel_act gateway
    /// An agent (embedded or external MCP) attempted a `cel_act` call.
    AgentActionAttempted,
    /// A previously-attempted action completed successfully.
    AgentActionCompleted,
    /// A previously-attempted action was denied (vetoed or confirmation timed out).
    AgentActionDenied,

    // Memory
    /// A chunk is about to be persisted. Rules can `Veto` to block the
    /// write (the provider's `before_write` hook treats Veto as
    /// authorisation-denied) or `LogOnly` to audit. See
    /// `cellar-memory-manager.md` §11.5.
    MemoryWriteAttempted,
    /// A retrieval was performed (sampled). Audit-only signal.
    MemoryRead,
    /// The memory subsystem is about to make an off-device call
    /// (cloud embedder, cloud summarizer). Rules can `RequireConfirmation`
    /// to surface the call to the user.
    MemoryOffdeviceCallAttempted,

    /// Escape hatch for kinds not yet codified.
    Other(String),
}

impl Event {
    /// Build an event at the current time. Convenience for tests and adapters.
    pub fn now(source: EventSource, kind: EventKind) -> Self {
        Self {
            ts: Utc::now(),
            source,
            kind,
            data: BTreeMap::new(),
        }
    }

    /// Add a data field via builder-style chaining.
    pub fn with_data(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Resolve a dotted field path against this event.
    ///
    /// Top-level paths: `ts`, `source`, `kind`. All others are looked up
    /// under `data.*` (e.g., `data.path`, `data.action_args.target_path`).
    /// Returns `None` if the path doesn't resolve.
    pub fn resolve_field(&self, path: &str) -> Option<Value> {
        match path {
            "ts" => Some(Value::String(self.ts.to_rfc3339())),
            "source" => serde_json::to_value(self.source).ok(),
            "kind" => serde_json::to_value(&self.kind).ok(),
            other => {
                // Strip leading "data." if present
                let key = other.strip_prefix("data.").unwrap_or(other);
                resolve_nested(&self.data, key)
            }
        }
    }
}

/// Walk a dotted path through a serde_json structure starting from a BTreeMap.
fn resolve_nested(map: &BTreeMap<String, Value>, path: &str) -> Option<Value> {
    let mut parts = path.splitn(2, '.');
    let head = parts.next()?;
    let rest = parts.next();

    let current = map.get(head)?;

    match rest {
        None => Some(current.clone()),
        Some(remaining) => walk_value(current, remaining),
    }
}

fn walk_value(value: &Value, path: &str) -> Option<Value> {
    let mut parts = path.splitn(2, '.');
    let head = parts.next()?;
    let rest = parts.next();

    let next = value.as_object()?.get(head)?;

    match rest {
        None => Some(next.clone()),
        Some(remaining) => walk_value(next, remaining),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_event() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted)
            .with_data("path", "/Users/me/Documents/big.pdf")
            .with_data("size_bytes", 2_147_483_648u64);
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn page_loaded_serializes_snake_case() {
        let e = Event::now(EventSource::CortexCdp, EventKind::PageLoaded);
        assert_eq!(e.resolve_field("kind"), Some(json!("page_loaded")));
        assert_eq!(e.resolve_field("source"), Some(json!("cortex_cdp")));
    }

    #[test]
    fn resolve_top_level_kind() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        assert_eq!(e.resolve_field("kind"), Some(json!("file_deleted")));
    }

    #[test]
    fn resolve_data_field() {
        let e =
            Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/x/y/z");
        assert_eq!(e.resolve_field("data.path"), Some(json!("/x/y/z")));
        assert_eq!(e.resolve_field("path"), Some(json!("/x/y/z")));
    }

    #[test]
    fn resolve_nested_data_field() {
        let e = Event::now(EventSource::CelActGateway, EventKind::AgentActionAttempted).with_data(
            "action_args",
            json!({ "source_path": "/a", "target_path": "/b" }),
        );
        assert_eq!(
            e.resolve_field("data.action_args.target_path"),
            Some(json!("/b"))
        );
    }

    #[test]
    fn resolve_missing_returns_none() {
        let e = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        assert!(e.resolve_field("data.does_not_exist").is_none());
    }

    #[test]
    fn other_kind_serializes() {
        let k = EventKind::Other("custom.thing".into());
        let s = serde_json::to_string(&k).unwrap();
        let back: EventKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, back);
    }

    #[test]
    fn new_sources_serialize_snake_case() {
        for (src, expected) in [
            (EventSource::CortexNetwork, "cortex_network"),
            (EventSource::CortexAudio, "cortex_audio"),
            (EventSource::CortexInput, "cortex_input"),
        ] {
            assert_eq!(serde_json::to_value(src).unwrap(), json!(expected));
            let back: EventSource = serde_json::from_value(json!(expected)).unwrap();
            assert_eq!(src, back);
        }
    }

    #[test]
    fn new_kinds_serialize_snake_case() {
        // One representative per stream we just added.
        for (kind, expected) in [
            (EventKind::AppDeactivated, "app_deactivated"),
            (EventKind::WindowMinimized, "window_minimized"),
            (EventKind::SheetOpened, "sheet_opened"),
            (EventKind::ValueChanged, "value_changed"),
            (
                EventKind::NetworkConnectionOpened,
                "network_connection_opened",
            ),
            (EventKind::AudioTranscript, "audio_transcript"),
            (EventKind::KeyboardInput, "keyboard_input"),
            (EventKind::PointerMoved, "pointer_moved"),
            (EventKind::PointerScroll, "pointer_scroll"),
        ] {
            assert_eq!(
                serde_json::to_value(&kind).unwrap(),
                json!(expected),
                "kind {kind:?} should serialize to {expected}"
            );
            let back: EventKind = serde_json::from_value(json!(expected)).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn input_event_resolves_content_fields() {
        // Keyboard fields + pointer coordinates must be addressable from rule
        // expressions. The shape mirrors exactly what cel-cortex emits (see the
        // EventKind::KeyboardInput / PointerMoved doc comments).
        let key = Event::now(EventSource::CortexInput, EventKind::KeyboardInput)
            .with_data("keycode", 36u64)
            .with_data("pressed", true)
            .with_data("text", "\r");
        assert_eq!(key.resolve_field("source"), Some(json!("cortex_input")));
        assert_eq!(key.resolve_field("kind"), Some(json!("keyboard_input")));
        assert_eq!(key.resolve_field("data.keycode"), Some(json!(36)));
        assert_eq!(key.resolve_field("data.pressed"), Some(json!(true)));

        let mv = Event::now(EventSource::CortexInput, EventKind::PointerMoved)
            .with_data("x", 640i64)
            .with_data("y", 480i64);
        assert_eq!(mv.resolve_field("data.x"), Some(json!(640)));
        assert_eq!(mv.resolve_field("data.y"), Some(json!(480)));
    }

    #[test]
    fn network_event_round_trips_with_payload() {
        let e = Event::now(
            EventSource::CortexNetwork,
            EventKind::NetworkConnectionOpened,
        )
        .with_data("remote_addr", "93.184.216.34")
        .with_data("remote_port", 443u16)
        .with_data("service", "https")
        .with_data("process_name", "Google Chrome");
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
        assert_eq!(back.resolve_field("data.service"), Some(json!("https")));
    }
}
