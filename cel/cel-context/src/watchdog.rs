//! Context Watchdog — detects changes between context snapshots.
//!
//! The watchdog compares consecutive ScreenContext snapshots and emits
//! CelEvents when something changes. This replaces polling in the MCP
//! observe tool with a more efficient diff-based approach.

use crate::element::ScreenContext;
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
    pub fn tick(&mut self, context: &ScreenContext, network_idle: bool) -> Vec<CelEvent> {
        let current_ids: HashSet<String> =
            context.elements.iter().map(|e| e.id.clone()).collect();

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

    /// Convert AXObserver push events into CelEvents and append them.
    /// Call this after `tick()` to merge push-based notifications with polling-based detections.
    pub fn merge_ax_events(&self, ax_events: Vec<cel_accessibility::AccessibilityEvent>) -> Vec<CelEvent> {
        ax_events
            .into_iter()
            .filter_map(|e| match e {
                cel_accessibility::AccessibilityEvent::FocusChanged { element_id } => {
                    Some(CelEvent::FocusChanged {
                        old: None,
                        new: element_id,
                    })
                }
                cel_accessibility::AccessibilityEvent::ValueChanged { element_id, new_value } => {
                    Some(CelEvent::ValueChanged { element_id, new_value })
                }
                cel_accessibility::AccessibilityEvent::LayoutChanged => Some(CelEvent::LayoutChanged),
                cel_accessibility::AccessibilityEvent::WindowCreated { title } => {
                    Some(CelEvent::WindowCreated { title })
                }
                cel_accessibility::AccessibilityEvent::MenuOpened => Some(CelEvent::MenuOpened),
                cel_accessibility::AccessibilityEvent::MenuClosed => Some(CelEvent::MenuClosed),
                cel_accessibility::AccessibilityEvent::SheetCreated => Some(CelEvent::SheetCreated),
                cel_accessibility::AccessibilityEvent::TitleChanged { new_title, .. } => {
                    Some(CelEvent::TitleChanged { new_title })
                }
                cel_accessibility::AccessibilityEvent::AppActivated { app_name } => {
                    Some(CelEvent::AppActivated { app_name })
                }
                cel_accessibility::AccessibilityEvent::AppDeactivated { app_name } => {
                    Some(CelEvent::AppDeactivated { app_name })
                }
                cel_accessibility::AccessibilityEvent::WindowMoved => Some(CelEvent::WindowMoved),
                cel_accessibility::AccessibilityEvent::WindowResized => Some(CelEvent::WindowResized),
                cel_accessibility::AccessibilityEvent::WindowMinimized => Some(CelEvent::WindowMinimized),
                cel_accessibility::AccessibilityEvent::WindowRestored => Some(CelEvent::WindowRestored),
                cel_accessibility::AccessibilityEvent::SelectionChanged => Some(CelEvent::SelectionChanged),
                cel_accessibility::AccessibilityEvent::RowCountChanged => Some(CelEvent::RowCountChanged),
                cel_accessibility::AccessibilityEvent::ElementDestroyed => Some(CelEvent::LayoutChanged),
                cel_accessibility::AccessibilityEvent::MainWindowChanged => Some(CelEvent::LayoutChanged),
                cel_accessibility::AccessibilityEvent::AppHidden { app_name } => {
                    Some(CelEvent::AppDeactivated { app_name })
                }
                cel_accessibility::AccessibilityEvent::AppShown { app_name } => {
                    Some(CelEvent::AppActivated { app_name })
                }
                cel_accessibility::AccessibilityEvent::AnnouncementRequested { .. } => None,
                cel_accessibility::AccessibilityEvent::HelpTagShown => None,
            })
            .collect()
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
    use crate::element::{ContentRole, ContextElement, ContextSource};
    use cel_accessibility::{Bounds, ElementState};

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

    fn make_context(elements: Vec<ContextElement>) -> ScreenContext {
        ScreenContext {
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
        assert!(events.iter().any(|e| matches!(e, CelEvent::FocusChanged { .. })));
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
