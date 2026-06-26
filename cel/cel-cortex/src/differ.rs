//! Context Differ — compares two ScreenContext snapshots.
//!
//! After a UI-changing action (dropdown open, modal appear, autocomplete expand),
//! diffs the before/after context to identify what changed. This allows the
//! planner to receive a focused prompt showing just what changed, saving 40-80%
//! of tokens on compound interactions.

use cel_context::{ContextElement, ScreenContext};
use std::collections::HashMap;

/// The result of diffing two context snapshots.
#[derive(Debug, Clone)]
pub struct ContextDiff {
    /// Elements present in `after` but not in `before`.
    pub added: Vec<ContextElement>,
    /// Element IDs present in `before` but not in `after`.
    pub removed: Vec<String>,
    /// Elements present in both but with different value or state.
    pub changed: Vec<ChangedElement>,
    /// Number of elements unchanged between snapshots.
    pub unchanged_count: usize,
}

/// An element that changed between snapshots.
#[derive(Debug, Clone)]
pub struct ChangedElement {
    pub element: ContextElement,
    pub changes: Vec<String>,
}

/// Diff two ScreenContext snapshots by element ID.
///
/// - `added`: IDs in `after` that don't exist in `before`
/// - `removed`: IDs in `before` that don't exist in `after`
/// - `changed`: Same ID, different value/state
/// - `unchanged_count`: Elements with identical ID and state
pub fn diff_contexts(before: &ScreenContext, after: &ScreenContext) -> ContextDiff {
    let before_map: HashMap<&str, &ContextElement> =
        before.elements.iter().map(|e| (e.id.as_str(), e)).collect();
    let after_map: HashMap<&str, &ContextElement> =
        after.elements.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged_count = 0;

    for (id, after_el) in &after_map {
        match before_map.get(id) {
            None => added.push((*after_el).clone()),
            Some(before_el) => {
                let changes = detect_changes(before_el, after_el);
                if changes.is_empty() {
                    unchanged_count += 1;
                } else {
                    changed.push(ChangedElement {
                        element: (*after_el).clone(),
                        changes,
                    });
                }
            }
        }
    }

    let removed: Vec<String> = before_map
        .keys()
        .filter(|id| !after_map.contains_key(*id))
        .map(|id| id.to_string())
        .collect();

    ContextDiff {
        added,
        removed,
        changed,
        unchanged_count,
    }
}

/// Detect what changed between two versions of the same element.
fn detect_changes(before: &ContextElement, after: &ContextElement) -> Vec<String> {
    let mut changes = Vec::new();

    if before.value != after.value {
        changes.push(format!(
            "value: \"{}\" → \"{}\"",
            before.value.as_deref().unwrap_or(""),
            after.value.as_deref().unwrap_or("")
        ));
    }
    if before.state.selected != after.state.selected {
        changes.push(format!(
            "selected: {} → {}",
            before.state.selected, after.state.selected
        ));
    }
    if before.state.expanded != after.state.expanded {
        changes.push(format!(
            "expanded: {:?} → {:?}",
            before.state.expanded, after.state.expanded
        ));
    }
    if before.state.checked != after.state.checked {
        changes.push(format!(
            "checked: {:?} → {:?}",
            before.state.checked, after.state.checked
        ));
    }
    if before.state.focused != after.state.focused {
        changes.push(format!(
            "focused: {} → {}",
            before.state.focused, after.state.focused
        ));
    }
    if before.state.visible != after.state.visible {
        changes.push(format!(
            "visible: {} → {}",
            before.state.visible, after.state.visible
        ));
    }
    if before.label != after.label {
        changes.push(format!(
            "label: \"{}\" → \"{}\"",
            before.label.as_deref().unwrap_or(""),
            after.label.as_deref().unwrap_or("")
        ));
    }

    changes
}

/// Determine if a diff is significant enough to store in the rolling window.
/// Returns true if there are meaningful new/changed elements (not just noise).
pub fn is_diff_significant(diff: &ContextDiff) -> bool {
    if diff.added.is_empty() && diff.changed.is_empty() {
        return false;
    }

    // At least one added element must be interactive (visible, enabled, has actions)
    let has_interactive_add = diff
        .added
        .iter()
        .any(|el| el.state.visible && el.state.enabled && !el.actions.is_empty());

    // Or meaningful state changes (expanded, selected, visible)
    let has_meaningful_change = diff.changed.iter().any(|c| {
        c.changes.iter().any(|ch| {
            ch.starts_with("expanded:") || ch.starts_with("selected:") || ch.starts_with("visible:")
        })
    });

    has_interactive_add || has_meaningful_change || diff.added.len() >= 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::{Bounds, ElementState};
    use cel_context::{ContentRole, ContextSource};

    fn make_element(id: &str, label: Option<&str>, value: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: label.map(String::from),
            description: None,
            element_type: "button".to_string(),
            value: value.map(String::from),
            bounds: Some(Bounds {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            }),
            state: ElementState {
                focused: false,
                enabled: true,
                visible: true,
                selected: false,
                expanded: None,
                checked: None,
            },
            parent_id: None,
            actions: vec!["click".to_string()],
            confidence: 0.85,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::Interactive,
            properties: std::collections::HashMap::new(),
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
    fn test_diff_empty_contexts() {
        let before = make_context(vec![]);
        let after = make_context(vec![]);
        let diff = diff_contexts(&before, &after);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
        assert_eq!(diff.unchanged_count, 0);
    }

    #[test]
    fn test_diff_added_elements() {
        let before = make_context(vec![make_element("a", Some("A"), None)]);
        let after = make_context(vec![
            make_element("a", Some("A"), None),
            make_element("b", Some("B"), None),
        ]);
        let diff = diff_contexts(&before, &after);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].id, "b");
        assert!(diff.removed.is_empty());
        assert_eq!(diff.unchanged_count, 1);
    }

    #[test]
    fn test_diff_removed_elements() {
        let before = make_context(vec![
            make_element("a", Some("A"), None),
            make_element("b", Some("B"), None),
        ]);
        let after = make_context(vec![make_element("a", Some("A"), None)]);
        let diff = diff_contexts(&before, &after);
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0], "b");
        assert_eq!(diff.unchanged_count, 1);
    }

    #[test]
    fn test_diff_changed_value() {
        let before = make_context(vec![make_element("a", Some("A"), Some("old"))]);
        let after = make_context(vec![make_element("a", Some("A"), Some("new"))]);
        let diff = diff_contexts(&before, &after);
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.changed[0].changes[0].contains("value"));
    }

    #[test]
    fn test_diff_changed_label() {
        let before = make_context(vec![make_element("a", Some("Old Label"), None)]);
        let after = make_context(vec![make_element("a", Some("New Label"), None)]);
        let diff = diff_contexts(&before, &after);
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.changed[0].changes[0].contains("label"));
    }

    #[test]
    fn test_diff_changed_state() {
        let mut el_before = make_element("a", Some("A"), None);
        el_before.state.selected = false;
        let mut el_after = make_element("a", Some("A"), None);
        el_after.state.selected = true;

        let diff = diff_contexts(
            &make_context(vec![el_before]),
            &make_context(vec![el_after]),
        );
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.changed[0].changes[0].contains("selected"));
    }

    #[test]
    fn test_significance_no_changes() {
        let diff = ContextDiff {
            added: vec![],
            removed: vec![],
            changed: vec![],
            unchanged_count: 5,
        };
        assert!(!is_diff_significant(&diff));
    }

    #[test]
    fn test_significance_interactive_add() {
        let diff = ContextDiff {
            added: vec![make_element("new", Some("New Button"), None)],
            removed: vec![],
            changed: vec![],
            unchanged_count: 5,
        };
        assert!(is_diff_significant(&diff));
    }

    #[test]
    fn test_significance_three_or_more_adds() {
        let mut non_interactive = make_element("x", Some("X"), None);
        non_interactive.actions.clear();
        non_interactive.state.enabled = false;

        let diff = ContextDiff {
            added: vec![
                non_interactive.clone(),
                {
                    let mut e = non_interactive.clone();
                    e.id = "y".into();
                    e
                },
                {
                    let mut e = non_interactive;
                    e.id = "z".into();
                    e
                },
            ],
            removed: vec![],
            changed: vec![],
            unchanged_count: 5,
        };
        assert!(is_diff_significant(&diff));
    }
}
