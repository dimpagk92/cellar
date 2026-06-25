//! Context Watchdog — detects changes between context snapshots.
//!
//! The watchdog compares consecutive ContextSnapshot snapshots and emits
//! CelEvents when something changes. This replaces polling in the MCP
//! observe tool with a more efficient diff-based approach.

use crate::element::ContextSnapshot;
use crate::events::CelEvent;
use std::collections::HashSet;

/// Watches for context changes by diffing consecutive snapshots.
pub struct ContextWatchdog {
    /// Element IDs from the last snapshot.
    last_element_ids: HashSet<String>,
    /// ID of the focused element from the last snapshot.
    last_focused_id: Option<String>,
    /// Whether the last network state was idle.
    last_network_idle: bool,
    /// Whether we have a baseline (first tick initializes state, doesn't emit events).
    initialized: bool,
}

impl Default for ContextWatchdog {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextWatchdog {
    pub fn new() -> Self {
        Self {
            last_element_ids: HashSet::new(),
            last_focused_id: None,
            last_network_idle: true,
            initialized: false,
        }
    }

    /// Compare the current context against the previous snapshot and emit events.
    ///
    /// `context`: The current screen context.
    /// `network_idle`: Whether the network monitor reports idle state.
    ///
    /// Returns a list of events that occurred since the last tick.
    pub fn tick(&mut self, context: &ContextSnapshot, network_idle: bool) -> Vec<CelEvent> {
        let current_ids: HashSet<String> = context.elements.iter().map(|e| e.id.clone()).collect();

        let current_focused = context
            .elements
            .iter()
            .find(|e| e.state.focused)
            .map(|e| e.id.clone());

        // First tick: just initialize state, don't emit events
        if !self.initialized {
            self.last_element_ids = current_ids;
            self.last_focused_id = current_focused;
            self.last_network_idle = network_idle;
            self.initialized = true;
            return vec![];
        }

        let mut events = Vec::new();

        // Tree diff: detect added/removed elements
        let added: Vec<String> = current_ids
            .difference(&self.last_element_ids)
            .cloned()
            .collect();
        let removed: Vec<String> = self
            .last_element_ids
            .difference(&current_ids)
            .cloned()
            .collect();
        if !added.is_empty() || !removed.is_empty() {
            events.push(CelEvent::TreeChanged { added, removed });
        }

        // Focus change — polling fallback. Even when AXObserver is active, we keep
        // this as a safety net. AXObserver events are merged separately via merge_ax_events(),
        // and deduplication happens naturally (same focus = no event from either source).
        if current_focused != self.last_focused_id {
            events.push(CelEvent::FocusChanged {
                old: self.last_focused_id.clone(),
                new: current_focused.clone(),
            });
        }

        // Network idle transition (was busy → now idle)
        if network_idle && !self.last_network_idle {
            events.push(CelEvent::NetworkIdle);
        }

        // Update state for next tick
        self.last_element_ids = current_ids;
        self.last_focused_id = current_focused;
        self.last_network_idle = network_idle;

        events
    }

    /// Merge source-specific push events that have already been normalized to CEL events.
    pub fn merge_events(&self, events: Vec<CelEvent>) -> Vec<CelEvent> {
        events
    }

    /// Compatibility shim for runtimes that still receive source-specific
    /// accessibility events. Convert those events before calling `merge_events`
    /// if push-event fidelity matters.
    pub fn merge_ax_events<T>(&self, _events: Vec<T>) -> Vec<CelEvent> {
        Vec::new()
    }

    /// Reset the watchdog state (e.g., when restarting monitoring).
    pub fn reset(&mut self) {
        self.last_element_ids.clear();
        self.last_focused_id = None;
        self.last_network_idle = true;
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{Bounds, ContentRole, ContextElement, ContextSource, ElementState};

    fn make_element(id: &str, focused: bool) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: Some(format!("Element {}", id)),
            description: None,
            element_type: "button".to_string(),
            value: None,
            bounds: Some(Bounds {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            }),
            state: ElementState {
                focused,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            confidence: 0.85,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::default(),
        }
    }

    fn make_context(elements: Vec<ContextElement>) -> ContextSnapshot {
        ContextSnapshot {
            app: "Test".into(),
            window: "Test Window".into(),
            elements,
            network_events: vec![],
            http_events: vec![],
            timestamp_ms: 0,
            screen_width: None,
            screen_height: None,
            clipboard: None,
            window_list: vec![],
            audio: None,
            power: None,
            running_apps: vec![],
            recent_files: vec![],
            transcripts: vec![],
        }
    }

    #[test]
    fn test_first_tick_no_events() {
        let mut wd = ContextWatchdog::new();
        let ctx = make_context(vec![make_element("a", true)]);
        let events = wd.tick(&ctx, true);
        assert!(events.is_empty(), "First tick should not emit events");
    }

    #[test]
    fn test_no_change_no_events() {
        let mut wd = ContextWatchdog::new();
        let ctx = make_context(vec![make_element("a", true)]);
        wd.tick(&ctx, true); // init
        let events = wd.tick(&ctx, true);
        assert!(events.is_empty());
    }

    #[test]
    fn test_element_added() {
        let mut wd = ContextWatchdog::new();
        let ctx1 = make_context(vec![make_element("a", true)]);
        wd.tick(&ctx1, true);

        let ctx2 = make_context(vec![make_element("a", true), make_element("b", false)]);
        let events = wd.tick(&ctx2, true);
        assert_eq!(events.len(), 1);
        match &events[0] {
            CelEvent::TreeChanged { added, removed } => {
                assert_eq!(added, &["b"]);
                assert!(removed.is_empty());
            }
            _ => panic!("Expected TreeChanged"),
        }
    }

    #[test]
    fn test_element_removed() {
        let mut wd = ContextWatchdog::new();
        let ctx1 = make_context(vec![make_element("a", true), make_element("b", false)]);
        wd.tick(&ctx1, true);

        let ctx2 = make_context(vec![make_element("a", true)]);
        let events = wd.tick(&ctx2, true);
        assert_eq!(events.len(), 1);
        match &events[0] {
            CelEvent::TreeChanged { added, removed } => {
                assert!(added.is_empty());
                assert_eq!(removed, &["b"]);
            }
            _ => panic!("Expected TreeChanged"),
        }
    }

    #[test]
    fn test_focus_changed() {
        let mut wd = ContextWatchdog::new();
        let ctx1 = make_context(vec![make_element("a", true), make_element("b", false)]);
        wd.tick(&ctx1, true);

        let ctx2 = make_context(vec![make_element("a", false), make_element("b", true)]);
        let events = wd.tick(&ctx2, true);
        assert!(events
            .iter()
            .any(|e| matches!(e, CelEvent::FocusChanged { .. })));
    }

    #[test]
    fn test_network_idle_transition() {
        let mut wd = ContextWatchdog::new();
        let ctx = make_context(vec![make_element("a", true)]);
        wd.tick(&ctx, false); // init: not idle

        let events = wd.tick(&ctx, true); // now idle
        assert!(events.iter().any(|e| matches!(e, CelEvent::NetworkIdle)));
    }

    #[test]
    fn test_network_stays_idle_no_event() {
        let mut wd = ContextWatchdog::new();
        let ctx = make_context(vec![make_element("a", true)]);
        wd.tick(&ctx, true); // init: idle
        let events = wd.tick(&ctx, true); // still idle
        assert!(!events.iter().any(|e| matches!(e, CelEvent::NetworkIdle)));
    }

    #[test]
    fn test_reset() {
        let mut wd = ContextWatchdog::new();
        let ctx = make_context(vec![make_element("a", true)]);
        wd.tick(&ctx, true);
        wd.reset();
        // After reset, next tick is initialization again
        let events = wd.tick(&ctx, true);
        assert!(events.is_empty());
    }
}
