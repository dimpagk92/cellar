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
    /// Process start/stop from the poller.
    Process,
    /// Filesystem events from FSEvents.
    Fsevents,
    /// Synthetic events emitted by the `cel_act` gateway.
    CelActGateway,
}

/// Catalog of event kinds.
///
/// `Other(String)` is the escape hatch for kinds added by future adapters
/// without a code change here. The matcher treats it transparently — rules
/// can match on `Other("...")` via `eq` on the kind field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    // CortexAx
    /// An app gained keyboard focus.
    AppFocused,
    /// A new window was opened.
    WindowOpened,

    // CortexCdp
    /// The active browser tab navigated to a new URL.
    UrlChanged,

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
}
