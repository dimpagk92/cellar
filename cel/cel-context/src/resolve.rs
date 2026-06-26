//! Reference resolution — find elements by multi-signal matching.

use crate::element::{ContextElement, ContextReference, ScreenContext};

/// Minimum score required for a match to be returned.
const MATCH_THRESHOLD: f64 = 0.55;

/// Weight for element type match (required — if this fails, score is 0).
const W_TYPE: f64 = 0.30;
/// Weight for label match (fuzzy).
const W_LABEL: f64 = 0.30;
/// Weight for ancestor path prefix match.
const W_ANCESTOR: f64 = 0.20;
/// Weight for bounds region proximity.
const W_BOUNDS: f64 = 0.10;
/// Weight for value pattern match.
const W_VALUE: f64 = 0.10;

/// Resolve a reference against a screen context snapshot.
/// Returns the best-matching element if its score exceeds the threshold.
pub fn resolve_reference<'a>(
    context: &'a ScreenContext,
    reference: &ContextReference,
) -> Option<&'a ContextElement> {
    let mut best: Option<(&ContextElement, f64)> = None;

    for el in &context.elements {
        let score = score_element(el, reference, context);
        if score >= MATCH_THRESHOLD && (best.is_none() || score > best.unwrap().1) {
            best = Some((el, score));
        }
    }

    best.map(|(el, _)| el)
}

/// Build the ancestor path for an element by following parent_id chains.
/// Returns a list of element_types from root to parent (not including the element itself).
fn build_ancestor_path(el: &ContextElement, all_elements: &[ContextElement]) -> Vec<String> {
    let mut path = Vec::new();
    let mut current_id = el.parent_id.as_deref();
    // Limit traversal depth to avoid infinite loops from bad data
    let mut depth = 0;
    while let Some(pid) = current_id {
        if depth > 15 {
            break;
        }
        if let Some(parent) = all_elements.iter().find(|e| e.id == pid) {
            path.push(parent.element_type.clone());
            current_id = parent.parent_id.as_deref();
        } else {
            break;
        }
        depth += 1;
    }
    path.reverse(); // Root → parent order
    path
}

fn score_element(
    el: &ContextElement,
    reference: &ContextReference,
    context: &ScreenContext,
) -> f64 {
    // Type must match exactly — it's a hard requirement.
    if el.element_type != reference.element_type {
        return 0.0;
    }
    let mut score = W_TYPE;

    // Label matching (case-insensitive contains)
    match (&el.label, &reference.label) {
        (Some(el_label), Some(ref_label)) => {
            let el_lower = el_label.to_lowercase();
            let ref_lower = ref_label.to_lowercase();
            if el_lower == ref_lower {
                score += W_LABEL;
            } else if el_lower.contains(&ref_lower) || ref_lower.contains(&el_lower) {
                score += W_LABEL * 0.8;
            }
        }
        (None, None) => {
            // Both have no label — slight positive signal
            score += W_LABEL * 0.3;
        }
        _ => {
            // One has label, other doesn't — no match
        }
    }

    // Ancestor path prefix match
    if !reference.ancestor_path.is_empty() {
        let el_path = build_ancestor_path(el, &context.elements);
        if !el_path.is_empty() {
            let match_count = reference
                .ancestor_path
                .iter()
                .zip(el_path.iter())
                .filter(|(a, b)| a == b)
                .count();
            let ref_len = reference.ancestor_path.len();
            if match_count == ref_len {
                score += W_ANCESTOR;
            } else if ref_len > 0 && match_count as f64 / ref_len as f64 >= 0.5 {
                score += W_ANCESTOR * 0.5;
            }
        }
    }

    // Bounds region proximity
    if let (Some(el_bounds), Some(ref_region)) = (&el.bounds, &reference.bounds_region) {
        let el_cx = el_bounds.x as f64 + el_bounds.width as f64 / 2.0;
        let el_cy = el_bounds.y as f64 + el_bounds.height as f64 / 2.0;

        let sw = context.screen_width.unwrap_or(1920) as f64;
        let sh = context.screen_height.unwrap_or(1080) as f64;
        let el_rx = (el_cx / sw).clamp(0.0, 1.0);
        let el_ry = (el_cy / sh).clamp(0.0, 1.0);

        let dist = ((el_rx - ref_region.relative_x).powi(2)
            + (el_ry - ref_region.relative_y).powi(2))
        .sqrt();

        // Close = high score, far = low score
        if dist < 0.1 {
            score += W_BOUNDS;
        } else if dist < 0.3 {
            score += W_BOUNDS * 0.5;
        }
    }

    // Value pattern match
    match (&el.value, &reference.value_pattern) {
        (Some(el_val), Some(pattern)) => {
            if el_val == pattern {
                score += W_VALUE;
            } else if el_val.contains(pattern) {
                score += W_VALUE * 0.5;
            }
        }
        (None, None) => {
            score += W_VALUE * 0.3;
        }
        _ => {}
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{ContentRole, ContextSource};
    use cel_accessibility::{Bounds, ElementState};

    fn make_element(id: &str, etype: &str, label: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.to_string(),
            label: label.map(|s| s.to_string()),
            description: None,
            element_type: etype.to_string(),
            value: None,
            bounds: Some(Bounds {
                x: 100,
                y: 200,
                width: 80,
                height: 30,
            }),
            state: ElementState::default(),
            parent_id: None,
            actions: vec![],
            confidence: 0.85,
            source: ContextSource::AccessibilityTree,
            properties: std::collections::HashMap::new(),
            content_role: ContentRole::default(),
        }
    }

    #[test]
    fn test_exact_match() {
        let ctx = ScreenContext {
            app: "Test".into(),
            window: "Test Window".into(),
            elements: vec![
                make_element("1", "button", Some("Submit")),
                make_element("2", "input", Some("Email")),
            ],
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
        };

        let reference = ContextReference {
            element_type: "button".into(),
            label: Some("Submit".into()),
            ancestor_path: vec![],
            bounds_region: None,
            value_pattern: None,
        };

        let result = resolve_reference(&ctx, &reference);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "1");
    }

    #[test]
    fn test_no_match_wrong_type() {
        let ctx = ScreenContext {
            app: "Test".into(),
            window: "Test Window".into(),
            elements: vec![make_element("1", "button", Some("Submit"))],
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
        };

        let reference = ContextReference {
            element_type: "input".into(),
            label: Some("Submit".into()),
            ancestor_path: vec![],
            bounds_region: None,
            value_pattern: None,
        };

        let result = resolve_reference(&ctx, &reference);
        assert!(result.is_none());
    }

    #[test]
    fn test_fuzzy_label_match() {
        let ctx = ScreenContext {
            app: "Test".into(),
            window: "Test Window".into(),
            elements: vec![make_element("1", "button", Some("Submit Form"))],
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
        };

        let reference = ContextReference {
            element_type: "button".into(),
            label: Some("Submit".into()),
            ancestor_path: vec![],
            bounds_region: None,
            value_pattern: None,
        };

        let result = resolve_reference(&ctx, &reference);
        assert!(result.is_some());
    }
}
