//! Action Verification — detects whether an action produced the expected effect.
//!
//! Ported from the runtime kernel's verifyActionOutcome().
//! Compares before/after ScreenContext snapshots to detect meaningful changes.

use cel_context::{ContextElement, ScreenContext};
use serde::{Deserialize, Serialize};

/// Result of post-action verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Whether verification detected any meaningful change.
    pub changed: bool,
    /// Whether a set_value action's value was confirmed.
    pub value_confirmed: bool,
    /// Whether the action caused a cross-app or cross-window shift.
    pub cross_app_shift: bool,
    /// Human-readable side-effect summary.
    pub side_effect_summary: Option<String>,
}

/// Verify that an action produced a meaningful change.
pub fn verify_action(
    before: &ScreenContext,
    after: &ScreenContext,
    action_type: &str,
    target_id: Option<&str>,
    expected_value: Option<&str>,
) -> VerificationResult {
    let cross_app_shift = before.app != after.app;
    let window_changed = before.window != after.window;

    // Cross-app shift is a side effect, not a verification success
    if cross_app_shift {
        return VerificationResult {
            changed: true,
            value_confirmed: false,
            cross_app_shift: true,
            side_effect_summary: Some(format!(
                "App changed: {} → {}", before.app, after.app
            )),
        };
    }

    // set_value confirmation: check if the target element's value matches
    let value_confirmed = if action_type == "set_value" {
        if let (Some(tid), Some(expected)) = (target_id, expected_value) {
            after.elements.iter().any(|el| {
                el.id == tid && el.value.as_deref() == Some(expected)
            })
        } else {
            false
        }
    } else {
        false
    };

    if value_confirmed {
        return VerificationResult {
            changed: true,
            value_confirmed: true,
            cross_app_shift: false,
            side_effect_summary: None,
        };
    }

    // Element count change
    if before.elements.len() != after.elements.len() {
        return VerificationResult {
            changed: true,
            value_confirmed: false,
            cross_app_shift: false,
            side_effect_summary: None,
        };
    }

    // Window title change
    if window_changed {
        return VerificationResult {
            changed: true,
            value_confirmed: false,
            cross_app_shift: false,
            side_effect_summary: Some(format!(
                "Window changed: {} → {}", before.window, after.window
            )),
        };
    }

    // Check first 30 elements for value/state changes
    let changed = element_diff_detected(
        &before.elements,
        &after.elements,
        30,
    );

    VerificationResult {
        changed,
        value_confirmed: false,
        cross_app_shift: false,
        side_effect_summary: if !changed {
            Some("No visible change detected after action".into())
        } else {
            None
        },
    }
}

/// Check if any element in the first N pairs differs in value or state.
fn element_diff_detected(before: &[ContextElement], after: &[ContextElement], max_check: usize) -> bool {
    for (b, a) in before.iter().zip(after.iter()).take(max_check) {
        if b.value != a.value { return true; }
        if b.label != a.label { return true; }
        if b.state.focused != a.state.focused { return true; }
        if b.state.selected != a.state.selected { return true; }
        if b.state.checked != a.state.checked { return true; }
        if b.state.expanded != a.state.expanded { return true; }
        if b.state.visible != a.state.visible { return true; }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::{ContextSource, ContentRole, ElementState};

    fn make_context(app: &str, elements: Vec<ContextElement>) -> ScreenContext {
        ScreenContext {
            app: app.into(),
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

    fn make_element(id: &str, value: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.into(),
            label: Some("Test".into()),
            description: None,
            element_type: "button".into(),
            value: value.map(|v| v.into()),
            bounds: None,
            state: ElementState {
                focused: false,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            parent_id: None,
            actions: vec![],
            confidence: 0.9,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::Interactive,
            properties: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_no_change() {
        let el = make_element("btn1", None);
        let before = make_context("App", vec![el.clone()]);
        let after = make_context("App", vec![el]);
        let result = verify_action(&before, &after, "click", Some("btn1"), None);
        assert!(!result.changed);
    }

    #[test]
    fn test_element_count_change() {
        let before = make_context("App", vec![make_element("btn1", None)]);
        let after = make_context("App", vec![
            make_element("btn1", None),
            make_element("btn2", None),
        ]);
        let result = verify_action(&before, &after, "click", Some("btn1"), None);
        assert!(result.changed);
    }

    #[test]
    fn test_cross_app_shift() {
        let before = make_context("Chrome", vec![]);
        let after = make_context("Excel", vec![]);
        let result = verify_action(&before, &after, "click", None, None);
        assert!(result.cross_app_shift);
        assert!(result.changed);
    }

    #[test]
    fn test_set_value_confirmed() {
        let before = make_context("App", vec![make_element("input1", Some(""))]);
        let after = make_context("App", vec![make_element("input1", Some("hello"))]);
        let result = verify_action(&before, &after, "set_value", Some("input1"), Some("hello"));
        assert!(result.value_confirmed);
        assert!(result.changed);
    }

    #[test]
    fn test_value_change_detected() {
        let before = make_context("App", vec![make_element("input1", Some("old"))]);
        let after = make_context("App", vec![make_element("input1", Some("new"))]);
        let result = verify_action(&before, &after, "click", None, None);
        assert!(result.changed);
    }
}
