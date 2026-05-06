//! Planning-view builder — projects rich Cortex state into a budgeted view.
//!
//! Implements the **store broadly, select narrowly** principle: the
//! `MentalModel` (and underlying `ScreenContext`) can be 60K+ tokens of raw
//! perception. The planner doesn't need that. This builder produces a
//! `PlanningView` with the goal-relevant slice — current screen + selected
//! elements + capabilities + run progress — and tracks what was dropped.
//!
//! In PR1a this is **deterministic only**:
//!   - Score elements by goal-keyword + quoted-phrase + actionable-type
//!     heuristics, drop the lowest-scoring to fit `budget.max_elements`.
//!   - Memory / knowledge / event refs stay empty (populated by PR3).
//!
//! Later PRs add a memory-aware path on top of this same builder; the
//! deterministic selector becomes the fallback when LLM-based selection is
//! unavailable or times out.

use std::collections::HashSet;

use cel_context::{ContextElement, ScreenContext};
use cel_contracts::{
    CapabilityRef, OmittedCounts, PlanningBudget, PlanningElement, PlanningElementState,
    PlanningScreen, PlanningView, RunProgress, RuntimeCaps,
};

/// Inputs the builder needs from the cortex / runner side.
///
/// Kept as a borrowed-data struct so the runner can build it once per
/// turn from its own state without copying.
pub struct PlanningViewInputs<'a> {
    pub goal: &'a str,
    pub budget: &'a PlanningBudget,
    pub perception: &'a ScreenContext,
    pub caps: &'a RuntimeCaps,
}

/// Build a budgeted `PlanningView` from current cortex perception + caps.
///
/// Deterministic — no LLM calls, no memory lookups (those land in PR3).
/// Selects the goal-relevant elements, folds runtime capabilities into the
/// view, fills `run_progress`, tracks omitted counts.
pub fn build_planning_view(inputs: &PlanningViewInputs<'_>) -> PlanningView {
    let total_elements = inputs.perception.elements.len() as u32;
    let elements = select_elements(inputs.perception, inputs.goal, inputs.budget.max_elements);
    let kept_elements = elements.len() as u32;
    let omitted_elements = total_elements.saturating_sub(kept_elements);

    PlanningView {
        goal: inputs.goal.to_string(),
        budget: inputs.budget.clone(),
        screen: PlanningScreen {
            active_app: inputs.perception.app.clone(),
            window: inputs.perception.window.clone(),
            summary: None,
            url: inputs.caps.cdp_url.clone(),
        },
        elements,
        adapter_facts: vec![],
        capabilities: caps_to_capabilities(inputs.caps),
        memories: vec![],
        knowledge: vec![],
        recent_events: vec![],
        blockers: vec![],
        anomalies: vec![],
        evidence: vec![],
        selection_rationale: None,
        omitted_counts: OmittedCounts {
            elements: omitted_elements,
            ..Default::default()
        },
        run_progress: RunProgress {
            steps_used: inputs.caps.steps_used,
            max_steps: inputs.caps.max_steps,
        },
    }
}

// ─── Capabilities folding ────────────────────────────────────────────────────

fn caps_to_capabilities(caps: &RuntimeCaps) -> Vec<CapabilityRef> {
    let mut out = Vec::with_capacity(2);
    if caps.cdp_bound {
        out.push(CapabilityRef {
            id: "cdp_bound".into(),
            detail: caps.cdp_browser.clone(),
        });
    }
    if caps.native_input {
        out.push(CapabilityRef {
            id: "native_input".into(),
            detail: None,
        });
    }
    out
}

// ─── Element selection (deterministic) ───────────────────────────────────────

fn select_elements(context: &ScreenContext, goal: &str, max_elements: u32) -> Vec<PlanningElement> {
    let max = max_elements as usize;
    if max == 0 {
        return Vec::new();
    }

    let keywords = extract_keywords(goal);
    let quoted = extract_quoted_phrases(goal);
    let is_extract = is_extraction_goal(goal);

    let mut scored: Vec<(f64, &ContextElement)> = context
        .elements
        .iter()
        .filter(|el| el.state.visible)
        .map(|el| (score_element(el, &keywords, &quoted, is_extract), el))
        .collect();

    // Stable sort by score descending so deterministic ordering is preserved
    // when scores tie.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<PlanningElement> = scored
        .iter()
        .filter(|(s, _)| *s > 0.0)
        .take(max)
        .map(|(_, el)| compress(el))
        .collect();

    // Always include `page-text` if present and we're not at budget.
    if out.len() < max && out.iter().all(|e| e.id != "page-text") {
        if let Some(pt) = context.elements.iter().find(|el| el.id == "page-text") {
            out.push(compress(pt));
        }
    }

    out
}

fn compress(el: &ContextElement) -> PlanningElement {
    PlanningElement {
        id: el.id.clone(),
        element_type: el.element_type.clone(),
        label: el.label.clone(),
        value: el.value.clone(),
        state: PlanningElementState {
            focused: el.state.focused,
            selected: el.state.selected,
            enabled: el.state.enabled,
            checked: el.state.checked.unwrap_or(false),
            expanded: el.state.expanded.unwrap_or(false),
        },
        clickable: !el.actions.is_empty()
            || matches!(
                el.element_type.as_str(),
                "button" | "link" | "a" | "checkbox" | "radio_button" | "menu_item"
            ),
        settable: matches!(
            el.element_type.as_str(),
            "input" | "textarea" | "combobox" | "select"
        ),
    }
}

// ─── Scoring (ported from cel-planner/distiller.rs) ──────────────────────────

const ACTIONABLE_TYPES: &[&str] = &[
    "button",
    "input",
    "select",
    "textarea",
    "a",
    "link",
    "checkbox",
    "radio_button",
    "combobox",
    "slider",
    "tab",
    "menu_item",
];

const GENERIC_ACTION_LABELS: &[&str] = &[
    "open",
    "close",
    "cancel",
    "ok",
    "more",
    "menu",
    "next",
    "back",
    "learn more",
    "details",
    "view",
    "edit",
    "delete",
    "remove",
    "select",
    "continue",
    "submit",
    "save",
    "apply",
    "retry",
    "dismiss",
];

const CHROME_HINT_KEYWORDS: &[&str] = &[
    "header",
    "nav",
    "navbar",
    "toolbar",
    "menu",
    "sidebar",
    "breadcrumb",
    "footer",
    "legal",
    "cookie",
    "consent",
    "account",
    "profile",
    "help",
    "support",
    "social",
    "share",
    "newsletter",
    "chat",
    "intercom",
];

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "and", "or", "but", "in", "on",
    "at", "to", "for", "of", "with", "from", "by", "as", "it", "do", "not", "this", "that", "my",
    "me", "i", "you", "we", "can", "will", "just", "any", "all", "each", "find", "search", "open",
    "read", "get", "show", "tell", "what", "how", "please",
];

fn is_actionable(element_type: &str) -> bool {
    ACTIONABLE_TYPES.contains(&element_type)
}

fn is_generic_label(label: &str) -> bool {
    GENERIC_ACTION_LABELS.contains(&label.to_lowercase().as_str())
}

fn extract_keywords(goal: &str) -> Vec<String> {
    let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();
    goal.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !stop.contains(w))
        .map(|w| w.to_string())
        .collect()
}

fn extract_quoted_phrases(goal: &str) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut in_quote = false;
    let mut current = String::new();
    let mut quote_char = '"';

    for ch in goal.chars() {
        if !in_quote && (ch == '"' || ch == '\'') {
            in_quote = true;
            quote_char = ch;
            current.clear();
        } else if in_quote && ch == quote_char {
            if current.len() > 2 {
                phrases.push(current.to_lowercase());
            }
            in_quote = false;
            current.clear();
        } else if in_quote {
            current.push(ch);
        }
    }
    phrases
}

fn is_extraction_goal(goal: &str) -> bool {
    let lower = goal.to_lowercase();
    lower.contains("extract")
        || lower.contains("read")
        || lower.contains("what is")
        || lower.contains("how many")
        || lower.contains("find")
        || lower.contains("list")
        || lower.contains("get the")
}

fn score_element(
    el: &ContextElement,
    keywords: &[String],
    quoted_phrases: &[String],
    is_extract: bool,
) -> f64 {
    let mut score: f64 = 0.0;
    let label = el.label.as_deref().unwrap_or("").to_lowercase();
    let value = el.value.as_deref().unwrap_or("").to_lowercase();
    let text = format!("{} {} {}", label, value, el.element_type);

    for kw in keywords {
        if text.contains(kw.as_str()) {
            score += 2.0;
        }
    }
    for phrase in quoted_phrases {
        if label == *phrase {
            score += 50.0;
        }
    }
    if label.len() > 4 {
        let goal_lower: String = keywords.join(" ");
        if goal_lower.contains(&label) && label.contains(' ') {
            score += 20.0;
        }
    }
    if is_extract {
        if matches!(el.element_type.as_str(), "text" | "heading" | "static_text") {
            score += 3.0;
        }
        if el.id == "page-text" {
            score += 50.0;
        }
    } else if el.id == "page-text" {
        score += 5.0;
    }
    if is_actionable(&el.element_type) {
        score += 1.0;
    }
    if el.state.visible && el.state.enabled {
        score += 0.5;
    }
    if !el.actions.is_empty() {
        score += 0.5;
    }
    let label_len = el.label.as_ref().map(|l| l.len()).unwrap_or(0);
    if label_len > 30 {
        score += 2.0;
    } else if label_len > 15 {
        score += 1.0;
    } else if label_len <= 5 && is_actionable(&el.element_type) {
        score -= 1.0;
    }
    if is_generic_label(&label) && is_actionable(&el.element_type) {
        score -= 1.5;
    }
    let props_hint = el
        .properties
        .get("css_selector")
        .map(|s| s.as_str())
        .unwrap_or("");
    for kw in CHROME_HINT_KEYWORDS {
        if props_hint.contains(kw) {
            score -= 4.0;
            break;
        }
    }

    score
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::ElementState;

    fn make_el(id: &str, ty: &str, label: Option<&str>) -> ContextElement {
        ContextElement {
            id: id.into(),
            label: label.map(String::from),
            description: None,
            element_type: ty.into(),
            value: None,
            bounds: None,
            state: ElementState {
                visible: true,
                enabled: true,
                ..Default::default()
            },
            parent_id: None,
            actions: vec![],
            confidence: 1.0,
            source: cel_context::ContextSource::AccessibilityTree,
            content_role: cel_context::ContentRole::Interactive,
            properties: Default::default(),
        }
    }

    fn make_context(elements: Vec<ContextElement>) -> ScreenContext {
        ScreenContext {
            app: "Browser".into(),
            window: "Test".into(),
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
    fn budget_caps_element_count_and_records_omitted() {
        let elements: Vec<ContextElement> = (0..200)
            .map(|i| make_el(&format!("e{i}"), "button", Some("Submit form")))
            .collect();
        let perception = make_context(elements);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_elements: 30,
            ..Default::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit the form",
            budget: &budget,
            perception: &perception,
            caps: &caps,
        });
        assert_eq!(view.elements.len(), 30);
        assert_eq!(view.omitted_counts.elements, 170);
    }

    #[test]
    fn goal_relevant_elements_outrank_irrelevant_ones() {
        let mut elements = Vec::new();
        for i in 0..20 {
            elements.push(make_el(&format!("noise{i}"), "button", Some("Open menu")));
        }
        elements.push(make_el("submit-1", "button", Some("Submit Invoice")));
        elements.push(make_el("submit-2", "button", Some("Save Draft")));

        let perception = make_context(elements);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_elements: 5,
            ..Default::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit the invoice",
            budget: &budget,
            perception: &perception,
            caps: &caps,
        });

        let ids: Vec<&str> = view.elements.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains(&"submit-1"),
            "submit-related element should rank in top-5; got {ids:?}"
        );
    }

    #[test]
    fn caps_fold_into_capabilities_and_run_progress() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps {
            cdp_bound: true,
            cdp_browser: Some("Google Chrome".into()),
            cdp_url: Some("https://example.com".into()),
            native_input: true,
            steps_used: 13,
            max_steps: 80,
        };
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "anything",
            budget: &budget,
            perception: &perception,
            caps: &caps,
        });

        assert_eq!(view.run_progress.steps_used, 13);
        assert_eq!(view.run_progress.max_steps, 80);
        assert_eq!(view.run_progress.steps_remaining(), 67);

        assert!(view.capabilities.iter().any(|c| c.id == "cdp_bound"));
        assert!(view.capabilities.iter().any(|c| c.id == "native_input"));
        assert_eq!(view.screen.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn empty_perception_yields_empty_view() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "do nothing",
            budget: &budget,
            perception: &perception,
            caps: &caps,
        });
        assert_eq!(view.elements.len(), 0);
        assert_eq!(view.omitted_counts.elements, 0);
        assert!(view.capabilities.is_empty());
    }

    #[test]
    fn invisible_elements_are_filtered_before_scoring() {
        let mut visible = make_el("visible", "button", Some("Submit"));
        let mut hidden = make_el("hidden", "button", Some("Submit"));
        hidden.state.visible = false;
        visible.state.visible = true;

        let perception = make_context(vec![hidden, visible]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_elements: 10,
            ..Default::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit",
            budget: &budget,
            perception: &perception,
            caps: &caps,
        });
        assert!(view.elements.iter().all(|e| e.id != "hidden"));
    }
}
