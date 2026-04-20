//! Integration tests for the CEL Context pipeline.
//!
//! Tests the full flow: accessibility → merge → confidence scoring.

use cel_accessibility::{AccessibilityElement, AccessibilityTree, ElementRole};
use cel_context::{ConfidenceBehavior, ConfidenceThresholds, ContentRole, ContextElement, ContextSource, ElementState, ScreenContext};

#[test]
fn test_confidence_thresholds_default() {
    let thresholds = ConfidenceThresholds::default();
    assert_eq!(thresholds.behavior_for(0.95), ConfidenceBehavior::ActImmediately);
    assert_eq!(thresholds.behavior_for(0.8), ConfidenceBehavior::ActAndLog);
    assert_eq!(thresholds.behavior_for(0.6), ConfidenceBehavior::ActCautiously);
    assert_eq!(thresholds.behavior_for(0.3), ConfidenceBehavior::PauseAndNotify);
}

#[test]
fn test_context_element_serialization() {
    let element = ContextElement {
        id: "test:button:1".into(),
        label: Some("Submit".into()),
        description: None,
        element_type: "button".into(),
        value: None,
        bounds: Some(cel_context::Bounds { x: 100, y: 200, width: 80, height: 30 }),
        state: cel_context::ElementState::default(),
        parent_id: None,
        actions: vec![],
        confidence: 0.95,
        source: ContextSource::AccessibilityTree,
        properties: std::collections::HashMap::new(),
        content_role: ContentRole::default(),
    };

    let json = serde_json::to_string(&element).unwrap();
    let deserialized: ContextElement = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.id, "test:button:1");
    assert_eq!(deserialized.label.as_deref(), Some("Submit"));
    assert_eq!(deserialized.confidence, 0.95);
}

#[test]
fn test_screen_context_serialization() {
    let ctx = ScreenContext {
        app: "TestApp".into(),
        window: "Main Window".into(),
        elements: vec![
            ContextElement {
                id: "a11y:btn:1".into(),
                label: Some("OK".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: None,
                state: cel_context::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.9,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::default(),
            },
            ContextElement {
                id: "vision:text:1".into(),
                label: Some("Hello World".into()),
                description: None,
                element_type: "text".into(),
                value: Some("Hello World".into()),
                bounds: None,
                state: cel_context::ElementState::default(),
                parent_id: None,
                actions: vec![],
                properties: std::collections::HashMap::new(),
                confidence: 0.75,
                source: ContextSource::Vision,
                content_role: ContentRole::default(),
            },
        ],
        network_events: vec![],
        http_events: vec![],
        timestamp_ms: 1700000000000,
        screen_width: None,
        screen_height: None,
        clipboard: None,
        window_list: vec![],
        audio: None,
        power: None,
        running_apps: vec![],
        recent_files: vec![],
        transcripts: vec![],
    };

    let json = serde_json::to_string(&ctx).unwrap();
    let back: ScreenContext = serde_json::from_str(&json).unwrap();
    assert_eq!(back.app, "TestApp");
    assert_eq!(back.elements.len(), 2);
}

#[test]
fn test_context_source_variants() {
    // Verify all source variants exist and are distinct
    let sources = vec![
        ContextSource::AccessibilityTree,
        ContextSource::NativeApi,
        ContextSource::Vision,
        ContextSource::Merged,
    ];
    for (i, a) in sources.iter().enumerate() {
        for (j, b) in sources.iter().enumerate() {
            if i == j {
                assert_eq!(a, b);
            } else {
                assert_ne!(a, b);
            }
        }
    }
}

// --- Phase 6: Confidence formula unit tests ---

/// Helper: create an accessibility tree with a single custom element, then merge it.
fn get_confidence_for(element: AccessibilityElement) -> f64 {
    struct SingleElement(AccessibilityElement);
    impl AccessibilityTree for SingleElement {
        fn get_tree(&self) -> Result<AccessibilityElement, cel_accessibility::AccessibilityError> {
            Ok(self.0.clone())
        }
        fn find_elements(&self, _: Option<&ElementRole>, _: Option<&str>) -> Result<Vec<AccessibilityElement>, cel_accessibility::AccessibilityError> {
            Ok(vec![])
        }
        fn focused_element(&self) -> Result<Option<AccessibilityElement>, cel_accessibility::AccessibilityError> {
            Ok(None)
        }
    }

    let mut merger = cel_context::ContextMerger::new(Box::new(SingleElement(element)));
    let ctx = merger.get_context();
    // Return the confidence of the first non-root element, or root if only one
    ctx.elements.first().map(|e| e.confidence).unwrap_or(0.0)
}

fn make_element(
    role: ElementRole,
    label: Option<&str>,
    bounds: Option<cel_accessibility::Bounds>,
    visible: bool,
    enabled: bool,
    actions: Vec<String>,
) -> AccessibilityElement {
    AccessibilityElement {
        id: "test-elem".into(),
        role,
        label: label.map(|s| s.to_string()),
        description: None,
        value: None,
        bounds,
        state: ElementState {
            focused: false,
            enabled,
            visible,
            selected: false,
            expanded: None,
            checked: None,
        },
        parent_id: None,
        actions,
        properties: std::collections::HashMap::new(),
        children: vec![],
        ..Default::default()
    }
}

#[test]
fn test_confidence_bare_minimum_element() {
    // No label, no bounds, not visible/enabled, non-actionable type
    let elem = make_element(ElementRole::Group, None, None, false, false, vec![]);
    let _conf = get_confidence_for(elem);
    // Base only: 0.60 — but invisible leaf with no data gets filtered, so use visible
    // Actually invisible leaf with no label/value/children gets skipped entirely.
    // Let's use a visible element with no label/bounds to test base.
    let elem2 = make_element(ElementRole::Group, None, None, true, false, vec![]);
    let conf2 = get_confidence_for(elem2);
    assert!((conf2 - 0.60).abs() < 0.01, "Bare minimum should be 0.60, got {}", conf2);
}

#[test]
fn test_confidence_with_label() {
    let elem = make_element(ElementRole::Group, Some("Hello"), None, true, false, vec![]);
    let conf = get_confidence_for(elem);
    assert!((conf - 0.70).abs() < 0.01, "With label should be 0.70, got {}", conf);
}

#[test]
fn test_confidence_with_label_and_bounds() {
    let bounds = cel_accessibility::Bounds { x: 0, y: 0, width: 100, height: 50 };
    let elem = make_element(ElementRole::Group, Some("Hello"), Some(bounds), true, false, vec![]);
    let conf = get_confidence_for(elem);
    assert!((conf - 0.80).abs() < 0.01, "With label+bounds should be 0.80, got {}", conf);
}

#[test]
fn test_confidence_with_label_bounds_visible_enabled() {
    let bounds = cel_accessibility::Bounds { x: 0, y: 0, width: 100, height: 50 };
    let elem = make_element(ElementRole::Group, Some("Hello"), Some(bounds), true, true, vec![]);
    let conf = get_confidence_for(elem);
    assert!((conf - 0.85).abs() < 0.01, "With label+bounds+state should be 0.85, got {}", conf);
}

#[test]
fn test_confidence_actionable_type() {
    let bounds = cel_accessibility::Bounds { x: 0, y: 0, width: 100, height: 50 };
    let elem = make_element(ElementRole::Button, Some("Create Account"), Some(bounds), true, true, vec![]);
    let conf = get_confidence_for(elem);
    assert!((conf - 0.90).abs() < 0.01, "Actionable should be 0.90, got {}", conf);
}

#[test]
fn test_confidence_with_actions() {
    let bounds = cel_accessibility::Bounds { x: 0, y: 0, width: 100, height: 50 };
    let elem = make_element(
        ElementRole::Button,
        Some("Create Account"),
        Some(bounds),
        true,
        true,
        vec!["click".into()],
    );
    let conf = get_confidence_for(elem);
    // 0.60 + 0.10 (label) + 0.10 (bounds) + 0.05 (visible+enabled) + 0.05 (actionable) + 0.05 (actions)
    // = 0.95 raw, capped at 0.95 by score_element_confidence()
    assert!((conf - 0.95).abs() < 0.01, "Fully loaded should be ~0.95 (capped), got {}", conf);
}

// --- Phase 6.2: State bit parsing tests ---

#[test]
fn test_state_bit_focused() {
    // Bit 12 = ATSPI_STATE_FOCUSED
    let state = parse_state_bits(1 << 12);
    assert!(state.focused, "Bit 12 should map to focused");
    assert!(!state.enabled);
    assert!(!state.visible);
}

#[test]
fn test_state_bit_enabled() {
    // Bit 8 = ATSPI_STATE_ENABLED
    let state = parse_state_bits(1 << 8);
    assert!(state.enabled, "Bit 8 should map to enabled");
    assert!(!state.focused);
}

#[test]
fn test_state_bit_visible() {
    // Bit 30 = ATSPI_STATE_VISIBLE
    let state = parse_state_bits(1 << 30);
    assert!(state.visible, "Bit 30 should map to visible");
}

#[test]
fn test_state_bit_selected() {
    // Bit 23 = ATSPI_STATE_SELECTED
    let state = parse_state_bits(1 << 23);
    assert!(state.selected, "Bit 23 should map to selected");
}

#[test]
fn test_state_bit_expandable_and_expanded() {
    // Bit 9 = EXPANDABLE, Bit 10 = EXPANDED
    let state = parse_state_bits((1 << 9) | (1 << 10));
    assert_eq!(state.expanded, Some(true), "Bits 9+10 should map to expanded=Some(true)");
}

#[test]
fn test_state_bit_expandable_but_collapsed() {
    // Bit 9 = EXPANDABLE only (no bit 10)
    let state = parse_state_bits(1 << 9);
    assert_eq!(state.expanded, Some(false), "Bit 9 only should map to expanded=Some(false)");
}

#[test]
fn test_state_bit_not_expandable() {
    // Neither bit 9 nor bit 10
    let state = parse_state_bits(0);
    assert_eq!(state.expanded, None, "No expandable bit should map to expanded=None");
}

#[test]
fn test_state_bit_checkable_and_checked() {
    // Bit 41 = CHECKABLE (in second u32, bit 9), Bit 4 = CHECKED
    let state = parse_state_bits((1u64 << 41) | (1 << 4));
    assert_eq!(state.checked, Some(true), "Bits 41+4 should map to checked=Some(true)");
}

#[test]
fn test_state_bit_checkable_but_unchecked() {
    // Bit 41 only
    let state = parse_state_bits(1u64 << 41);
    assert_eq!(state.checked, Some(false), "Bit 41 only should map to checked=Some(false)");
}

#[test]
fn test_state_bit_not_checkable() {
    let state = parse_state_bits(0);
    assert_eq!(state.checked, None, "No checkable bit should map to checked=None");
}

#[test]
fn test_state_bits_all_clear() {
    let state = parse_state_bits(0);
    assert!(!state.focused);
    assert!(!state.enabled);
    assert!(!state.visible);
    assert!(!state.selected);
    assert_eq!(state.expanded, None);
    assert_eq!(state.checked, None);
}

#[test]
fn test_state_bits_combined() {
    // focused(12) + enabled(8) + visible(30) + selected(23) + expandable(9) + expanded(10)
    let bits = (1 << 12) | (1 << 8) | (1 << 30) | (1 << 23) | (1 << 9) | (1 << 10);
    let state = parse_state_bits(bits);
    assert!(state.focused);
    assert!(state.enabled);
    assert!(state.visible);
    assert!(state.selected);
    assert_eq!(state.expanded, Some(true));
    assert_eq!(state.checked, None); // bit 41 not set
}

/// Parse a 64-bit AT-SPI2 state bitfield into ElementState.
/// This mirrors the logic in linux.rs get_state() for testability.
fn parse_state_bits(bits: u64) -> ElementState {
    ElementState {
        focused:  bits & (1 << 12) != 0,
        enabled:  bits & (1 << 8) != 0,
        visible:  bits & (1 << 30) != 0,
        selected: bits & (1 << 23) != 0,
        expanded: if bits & (1 << 9) != 0 {
            Some(bits & (1 << 10) != 0)
        } else {
            None
        },
        checked: if bits & (1u64 << 41) != 0 {
            Some(bits & (1 << 4) != 0)
        } else {
            None
        },
    }
}
