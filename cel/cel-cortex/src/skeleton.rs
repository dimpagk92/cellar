//! Skeleton screen detection — heuristics to identify loading/skeleton states.
//!
//! Avoids acting on pages that are still loading by detecting common patterns:
//! - Many elements but almost no text content (placeholder skeletons)
//! - Very few interactive elements relative to total (loading state)
//! - Elements with aria-busy="true" or aria-live="polite" (loading indicators)

use cel_context::ScreenContext;

/// Minimum elements to consider skeleton detection meaningful.
const MIN_ELEMENTS_FOR_DETECTION: usize = 5;

/// If text-bearing ratio is below this, likely skeleton placeholders.
const TEXT_RATIO_THRESHOLD: f64 = 0.15;

/// If interactive ratio is below this, likely in loading state.
const INTERACTIVE_RATIO_THRESHOLD: f64 = 0.05;

/// Default wait when skeleton detected (ms).
const DEFAULT_SKELETON_WAIT_MS: u64 = 2000;

/// Extended wait when strong skeleton signals (ms).
const EXTENDED_SKELETON_WAIT_MS: u64 = 4000;

/// Check if an element type is interactive.
fn is_interactive_type(t: &str) -> bool {
    matches!(
        t,
        "button"
            | "link"
            | "input"
            | "textarea"
            | "select"
            | "checkbox"
            | "radio"
            | "switch"
            | "slider"
            | "menu_item"
            | "tab"
            | "combobox"
    )
}

/// Check if an element type is an explicit loading indicator.
fn is_loading_type(t: &str) -> bool {
    matches!(
        t,
        "progress_indicator" | "busy_indicator" | "progressbar" | "activity_indicator"
    )
}

/// Check if a label matches spinner patterns (case-insensitive).
fn matches_spinner_label(label: &str) -> bool {
    let lower = label.to_lowercase();
    lower.contains("loading")
        || lower.contains("spinner")
        || lower.contains("progress")
        || lower.contains("indeterminate")
        || lower.contains("please wait")
        || lower.contains("fetching")
        || lower.contains("buffering")
}

/// Check if text matches aria-live loading patterns.
fn matches_aria_live_loading(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("loading") || lower.contains("spinner") || lower.contains("please wait")
}

/// Detect if a context looks like a skeleton/loading screen.
pub fn is_skeleton_screen(context: &ScreenContext) -> bool {
    let elements = &context.elements;
    if elements.len() < MIN_ELEMENTS_FOR_DETECTION {
        return false;
    }

    // Check for explicit loading indicators (aria-busy)
    let has_aria_busy = elements.iter().any(|el| {
        el.properties.get("aria-busy").is_some_and(|v| v == "true")
            || el.properties.get("busy").is_some_and(|v| v == "true")
    });
    if has_aria_busy {
        return true;
    }

    // Check text-bearing ratio
    let text_bearing_count = elements
        .iter()
        .filter(|el| {
            el.label.as_ref().is_some_and(|l| !l.trim().is_empty())
                || el.value.as_ref().is_some_and(|v| !v.trim().is_empty())
        })
        .count();
    let text_ratio = text_bearing_count as f64 / elements.len() as f64;

    // Check interactive element ratio
    let interactive_count = elements
        .iter()
        .filter(|el| is_interactive_type(&el.element_type))
        .count();
    let interactive_ratio = interactive_count as f64 / elements.len() as f64;

    if text_ratio < TEXT_RATIO_THRESHOLD && interactive_ratio < INTERACTIVE_RATIO_THRESHOLD {
        return true;
    }

    // Check for aria-live regions with loading text
    let has_aria_live_loading = elements.iter().any(|el| {
        if !el.properties.contains_key("aria-live") {
            return false;
        }
        let text = format!(
            "{}{}",
            el.label.as_deref().unwrap_or(""),
            el.description.as_deref().unwrap_or("")
        );
        matches_aria_live_loading(&text)
    });
    if has_aria_live_loading {
        return true;
    }

    false
}

/// Suggested wait time in milliseconds based on skeleton detection signals.
/// Returns 0 if no skeleton/loading state detected.
pub fn skeleton_wait_ms(context: &ScreenContext) -> u64 {
    let elements = &context.elements;
    if elements.len() < MIN_ELEMENTS_FOR_DETECTION {
        return 0;
    }

    let has_aria_busy = elements.iter().any(|el| {
        el.properties.get("aria-busy").is_some_and(|v| v == "true")
            || el.properties.get("busy").is_some_and(|v| v == "true")
    });
    if has_aria_busy {
        return EXTENDED_SKELETON_WAIT_MS;
    }

    let text_bearing_count = elements
        .iter()
        .filter(|el| {
            el.label.as_ref().is_some_and(|l| !l.trim().is_empty())
                || el.value.as_ref().is_some_and(|v| !v.trim().is_empty())
        })
        .count();
    let text_ratio = text_bearing_count as f64 / elements.len() as f64;

    let interactive_count = elements
        .iter()
        .filter(|el| is_interactive_type(&el.element_type))
        .count();
    let interactive_ratio = interactive_count as f64 / elements.len() as f64;

    if text_ratio < TEXT_RATIO_THRESHOLD && interactive_ratio < INTERACTIVE_RATIO_THRESHOLD {
        return DEFAULT_SKELETON_WAIT_MS;
    }

    let has_aria_live_loading = elements.iter().any(|el| {
        if !el.properties.contains_key("aria-live") {
            return false;
        }
        let text = format!(
            "{}{}",
            el.label.as_deref().unwrap_or(""),
            el.description.as_deref().unwrap_or("")
        );
        matches_aria_live_loading(&text)
    });
    if has_aria_live_loading {
        return DEFAULT_SKELETON_WAIT_MS;
    }

    0
}

/// Detect if the context has an active spinner, progress bar, or loading indicator.
pub fn has_active_spinner(context: &ScreenContext) -> bool {
    context.elements.iter().any(|el| {
        if is_loading_type(&el.element_type) {
            return el.state.visible;
        }
        let label = el.label.as_deref().unwrap_or("");
        if label.is_empty() {
            return false;
        }
        el.state.visible && matches_spinner_label(label)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::{Bounds, ElementState};
    use cel_context::{ContentRole, ContextElement, ContextSource};

    fn make_element(id: &str, el_type: &str, label: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: label.map(String::from),
            description: None,
            element_type: el_type.to_string(),
            value: None,
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
            actions: vec![],
            confidence: 0.85,
            source: ContextSource::AccessibilityTree,
            content_role: ContentRole::default(),
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
    fn test_too_few_elements_not_skeleton() {
        let ctx = make_context(vec![
            make_element("1", "group", None),
            make_element("2", "group", None),
        ]);
        assert!(!is_skeleton_screen(&ctx));
    }

    #[test]
    fn test_aria_busy_triggers_skeleton() {
        let mut el = make_element("1", "group", None);
        el.properties.insert("aria-busy".into(), "true".into());
        let elements: Vec<_> = (0..6)
            .map(|i| make_element(&format!("{}", i), "group", None))
            .chain(std::iter::once(el))
            .collect();
        let ctx = make_context(elements);
        assert!(is_skeleton_screen(&ctx));
    }

    #[test]
    fn test_low_text_ratio_skeleton() {
        let elements: Vec<_> = (0..10)
            .map(|i| make_element(&format!("{}", i), "group", None))
            .collect();
        let ctx = make_context(elements);
        assert!(is_skeleton_screen(&ctx));
    }

    #[test]
    fn test_normal_page_not_skeleton() {
        let elements: Vec<_> = (0..10)
            .map(|i| {
                if i < 3 {
                    make_element(&format!("{}", i), "button", Some(&format!("Button {}", i)))
                } else {
                    make_element(&format!("{}", i), "text", Some(&format!("Text {}", i)))
                }
            })
            .collect();
        let ctx = make_context(elements);
        assert!(!is_skeleton_screen(&ctx));
    }

    #[test]
    fn test_spinner_by_element_type() {
        let ctx = make_context(vec![make_element("1", "progress_indicator", None)]);
        assert!(has_active_spinner(&ctx));
    }

    #[test]
    fn test_spinner_by_label_pattern() {
        let ctx = make_context(vec![make_element("1", "text", Some("Loading..."))]);
        assert!(has_active_spinner(&ctx));
    }

    #[test]
    fn test_no_spinner_on_normal_page() {
        let ctx = make_context(vec![
            make_element("1", "button", Some("Submit")),
            make_element("2", "text", Some("Hello world")),
        ]);
        assert!(!has_active_spinner(&ctx));
    }

    #[test]
    fn test_skeleton_wait_ms_aria_busy() {
        let mut el = make_element("1", "group", None);
        el.properties.insert("aria-busy".into(), "true".into());
        let elements: Vec<_> = (0..6)
            .map(|i| make_element(&format!("{}", i), "group", None))
            .chain(std::iter::once(el))
            .collect();
        let ctx = make_context(elements);
        assert_eq!(skeleton_wait_ms(&ctx), 4000);
    }

    #[test]
    fn test_skeleton_wait_ms_normal() {
        let elements: Vec<_> = (0..10)
            .map(|i| make_element(&format!("{}", i), "button", Some(&format!("Btn {}", i))))
            .collect();
        let ctx = make_context(elements);
        assert_eq!(skeleton_wait_ms(&ctx), 0);
    }
}
