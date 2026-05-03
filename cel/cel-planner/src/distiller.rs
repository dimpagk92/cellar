//! Context Distillation — reduces ScreenContext to the most relevant elements.
//!
//! Ported from agent/src/goal-runner/context-distiller.ts.
//! Scores elements by relevance to the goal and returns only the top N.
//! Runs entirely in Rust before the LLM call — no TS in the planning path.

use cel_context::{ContextElement, ScreenContext};
use std::collections::HashSet;

/// Actionable element types (can be clicked/interacted with).
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

/// Generic action labels that get deprioritized when repeated.
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

/// Chrome/UI noise patterns to deprioritize.
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

/// Stop words filtered from goal keywords.
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

/// Extract meaningful keywords from a goal string.
fn extract_keywords(goal: &str) -> Vec<String> {
    let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();
    goal.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !stop.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Extract quoted phrases from a goal (e.g., "Machine learning").
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

/// Check if the goal is an extraction/reading goal.
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

/// Score a single element's relevance to the goal.
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

    // Keyword matching
    for kw in keywords {
        if text.contains(kw.as_str()) {
            score += 2.0;
        }
    }

    // Quoted phrase exact match (+50, same as TS version)
    for phrase in quoted_phrases {
        if label == *phrase {
            score += 50.0;
        }
    }

    // Multi-word phrase boost from goal
    // If label matches a multi-word substring of the goal → big boost
    if label.len() > 4 {
        let goal_lower: String = keywords.join(" ");
        if goal_lower.contains(&label) && label.contains(' ') {
            score += 20.0;
        }
    }

    // Extraction goal awareness
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

    // Actionable boost
    if is_actionable(&el.element_type) {
        score += 1.0;
    }
    if el.state.visible && el.state.enabled {
        score += 0.5;
    }
    if !el.actions.is_empty() {
        score += 0.5;
    }

    // Content length
    let label_len = el.label.as_ref().map(|l| l.len()).unwrap_or(0);
    if label_len > 30 {
        score += 2.0;
    } else if label_len > 15 {
        score += 1.0;
    } else if label_len <= 5 && is_actionable(&el.element_type) {
        score -= 1.0;
    }

    // Generic action label deprioritization
    if is_generic_label(&label) && is_actionable(&el.element_type) {
        score -= 1.5;
    }

    // Chrome noise deprioritization
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

/// Distill a context to the top N most relevant elements for a given goal.
pub fn distill_for_goal(
    context: &ScreenContext,
    goal: &str,
    max_elements: usize,
) -> Vec<ContextElement> {
    let keywords = extract_keywords(goal);
    let quoted = extract_quoted_phrases(goal);
    let is_extract = is_extraction_goal(goal);

    if keywords.is_empty() && quoted.is_empty() {
        // No keywords — return actionable elements
        return context
            .elements
            .iter()
            .filter(|el| is_actionable(&el.element_type) || el.id == "page-text")
            .take(max_elements)
            .cloned()
            .collect();
    }

    // Score all elements
    let mut scored: Vec<(f64, &ContextElement)> = context
        .elements
        .iter()
        .map(|el| (score_element(el, &keywords, &quoted, is_extract), el))
        .collect();

    // Sort by score descending
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Take top elements, ensuring page-text is included
    let mut result: Vec<ContextElement> = scored
        .iter()
        .filter(|(s, _)| *s > 0.0)
        .take(max_elements)
        .map(|(_, el)| (*el).clone())
        .collect();

    // Ensure page-text is always included
    let has_page_text = result.iter().any(|el| el.id == "page-text");
    if !has_page_text {
        if let Some(pt) = context.elements.iter().find(|el| el.id == "page-text") {
            result.push(pt.clone());
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords() {
        let kws = extract_keywords("Find the cheapest hotel in Amsterdam");
        assert!(kws.contains(&"cheapest".to_string()));
        assert!(kws.contains(&"hotel".to_string()));
        assert!(kws.contains(&"amsterdam".to_string()));
        assert!(!kws.contains(&"the".to_string())); // stop word
    }

    #[test]
    fn test_extract_quoted_phrases() {
        let phrases = extract_quoted_phrases("Click on \"Machine learning\" link");
        assert_eq!(phrases, vec!["machine learning"]);
    }

    #[test]
    fn test_is_extraction_goal() {
        assert!(is_extraction_goal("Extract the price from the page"));
        assert!(is_extraction_goal("What is the current temperature?"));
        assert!(!is_extraction_goal("Click the submit button"));
    }
}
