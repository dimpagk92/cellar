//! Goal-aware element selection and scoring.
//!
//! Deterministic heuristics — goal-keyword and quoted-phrase matching plus
//! actionable-role scoring — that pick the on-screen elements worth surfacing,
//! with the keyword / phrase extraction and content-compression helpers.

use super::*;

pub(crate) fn select_elements(
    context: &ScreenContext,
    goal: &str,
    max_elements: u32,
) -> Vec<PlanningElement> {
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

pub(crate) fn compress(el: &ContextElement) -> PlanningElement {
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
        // The browser CDP / DOM adapter encodes select option pairs
        // into properties["select_options"]. Copy it over to the
        // planning view so the planner prompt can render the actual
        // option values — without this the model guesses slugs and
        // we see `cdp set: no-option:select:subject:Test` failures
        // (Run-6, 2026-05-19, contact form scenarios 3/3 trials).
        select_options: el.properties.get("select_options").cloned(),
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

pub(crate) fn extract_keywords(goal: &str) -> Vec<String> {
    let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();
    goal.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !stop.contains(w))
        .map(|w| w.to_string())
        .collect()
}

pub(crate) fn extract_quoted_phrases(goal: &str) -> Vec<String> {
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
