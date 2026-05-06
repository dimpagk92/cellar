//! Planning-view builder — projects rich Cortex state into a budgeted view.
//!
//! Implements the **store broadly, select narrowly** principle: the
//! `MentalModel` (and underlying `ScreenContext`) can be 60K+ tokens of raw
//! perception. The planner doesn't need that. This builder produces a
//! `PlanningView` with the goal-relevant slice — current screen + selected
//! elements + capabilities + run progress + (PR3) memory-aware hydration —
//! and tracks what was dropped.
//!
//! Element selection is deterministic (goal-keyword + quoted-phrase +
//! actionable-type heuristics).
//!
//! Memory selection (PR3) is also deterministic: the builder pulls
//! workflow-scoped memories from `cortex_memories`, scores each by keyword
//! overlap with the goal × exponential decay against `last_accessed_at`,
//! and takes the top-N within `budget.max_memories`. No LLM call. PR4 may
//! add an LLM-based selector on top, but the deterministic path is the
//! fallback (and the default).

use std::collections::HashSet;

use cel_context::{ContextElement, ScreenContext};
use cel_contracts::{
    CapabilityRef, MemoryRef, OmittedCounts, PlanningBudget, PlanningElement, PlanningElementState,
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
    /// PR3: when set together with `workflow_id`, the builder hydrates
    /// goal-relevant memories from this SQLite store into `view.memories`.
    /// Defaults to `None` — view's `memories` stays empty (preserves PR1a
    /// behaviour for callers that haven't opted in).
    pub memory_db_path: Option<&'a str>,
    /// PR3: required alongside `memory_db_path`. Memory selection is
    /// strictly workflow-scoped — the same workflow_id used for writes
    /// (via `RunLimits.workflow_id_for_memory` or `cel_perceive start
    /// { enable_memory, workflow_id }`).
    pub workflow_id: Option<&'a str>,
}

/// Build a budgeted `PlanningView` from current cortex perception + caps,
/// optionally hydrating workflow-scoped memories.
///
/// Selection is fully deterministic. No LLM calls. Memory hydration only
/// fires when both `memory_db_path` and `workflow_id` are set — privacy-
/// preserving default. Failure to open the store is logged at WARN and
/// the builder returns an empty memory list rather than failing the view.
pub fn build_planning_view(inputs: &PlanningViewInputs<'_>) -> PlanningView {
    let total_elements = inputs.perception.elements.len() as u32;
    let elements = select_elements(inputs.perception, inputs.goal, inputs.budget.max_elements);
    let kept_elements = elements.len() as u32;
    let omitted_elements = total_elements.saturating_sub(kept_elements);

    let memory_selection = match (inputs.memory_db_path, inputs.workflow_id) {
        (Some(db), Some(wf)) => select_memories(db, wf, inputs.goal, inputs.budget.max_memories),
        _ => MemorySelection::default(),
    };

    let selection_rationale = build_rationale(elements.len(), &memory_selection, omitted_elements);

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
        memories: memory_selection.kept,
        knowledge: vec![],
        recent_events: vec![],
        blockers: vec![],
        anomalies: vec![],
        evidence: vec![],
        selection_rationale,
        omitted_counts: OmittedCounts {
            elements: omitted_elements,
            memories: memory_selection.omitted,
            ..Default::default()
        },
        run_progress: RunProgress {
            steps_used: inputs.caps.steps_used,
            max_steps: inputs.caps.max_steps,
        },
    }
}

// ─── Memory selection (PR3, deterministic) ──────────────────────────────────

/// Result of memory selection — kept memories plus omitted count for the
/// `omitted_counts` field on the view.
#[derive(Debug, Default)]
struct MemorySelection {
    kept: Vec<MemoryRef>,
    omitted: u32,
    /// Set when the store opened cleanly but the workflow is empty —
    /// distinct from a store-open failure (which is logged at WARN).
    workflow_empty: bool,
}

/// Select goal-relevant memories from the SQLite store.
///
/// Algorithm:
/// 1. Open `CelStore` at `db_path`. Failure → log + return empty.
/// 2. Pull recent memories for `workflow_id` (top 200 by `created_at`).
/// 3. Score each: `score = keyword_overlap(goal, summary+content) × decay`.
/// 4. Sort by score descending.
/// 5. Take up to `max_memories` whose score > 0.
/// 6. Hydrate to `MemoryRef`s, build the omitted count.
///
/// Decay uses `last_accessed_at` (touched memories ride longer).
fn select_memories(
    db_path: &str,
    workflow_id: &str,
    goal: &str,
    max_memories: u32,
) -> MemorySelection {
    let max = max_memories as usize;
    if max == 0 {
        return MemorySelection::default();
    }

    let store = match cel_store::CelStore::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                db_path,
                error = %e,
                "PR3 planning_view: cortex_memories store open failed; skipping memory hydration",
            );
            return MemorySelection::default();
        }
    };

    // Pull a generous candidate window so the in-Rust scorer can see the
    // most-recent ~200. Prevents pathological cases where a very old but
    // highly goal-relevant memory dominates the catalog.
    let candidates = match store.list_cortex_memories(workflow_id, None, 200) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                workflow_id,
                error = %e,
                "PR3 planning_view: list_cortex_memories failed; skipping memory hydration",
            );
            return MemorySelection::default();
        }
    };

    if candidates.is_empty() {
        return MemorySelection {
            workflow_empty: true,
            ..MemorySelection::default()
        };
    }

    let total = candidates.len() as u32;
    let keywords = extract_keywords(goal);
    let quoted = extract_quoted_phrases(goal);
    let now = cel_store::cortex_memory::now_unix_secs();

    let mut scored: Vec<(f64, cel_store::cortex_memory::CortexMemory)> = candidates
        .into_iter()
        .map(|m| (score_memory(&m, &keywords, &quoted, now), m))
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let kept: Vec<MemoryRef> = scored
        .iter()
        .filter(|(s, _)| *s > 0.0)
        .take(max)
        .map(|(_, m)| compress_memory(m))
        .collect();

    let omitted = total.saturating_sub(kept.len() as u32);
    MemorySelection {
        kept,
        omitted,
        workflow_empty: false,
    }
}

/// Score a memory's relevance to the goal at the given moment.
///
/// `score = base × decay`, where `base` reflects keyword + quoted-phrase
/// overlap, and `decay = exp(-ln(2) × age_days / 90)` against
/// `last_accessed_at`. Memories with no overlap score 0 (filtered out
/// even if very recent — the selector is goal-relevance-first).
fn score_memory(
    memory: &cel_store::cortex_memory::CortexMemory,
    keywords: &[String],
    quoted: &[String],
    now_secs: i64,
) -> f64 {
    let summary = memory.summary.as_deref().unwrap_or("").to_lowercase();
    let content_str = serde_json::to_string(&memory.content)
        .unwrap_or_default()
        .to_lowercase();
    let tags_blob = memory.tags.join(" ").to_lowercase();
    let haystack = format!("{summary} {content_str} {tags_blob}");

    let mut base: f64 = 0.0;

    for kw in keywords {
        if haystack.contains(kw.as_str()) {
            base += 2.0;
            // Bonus if keyword hits the curated summary specifically —
            // summaries are caller-written one-liners; matches there are
            // higher-signal than incidental hits inside JSON content.
            if summary.contains(kw.as_str()) {
                base += 1.0;
            }
        }
    }

    for phrase in quoted {
        if haystack.contains(phrase.as_str()) {
            base += 30.0;
        }
    }

    // Kind-based prior: failures are mildly more useful than outcomes
    // when the goal looks similar to past failed attempts. Small bias.
    if base > 0.0
        && matches!(
            memory.kind,
            cel_store::cortex_memory::MemoryKind::Failure
                | cel_store::cortex_memory::MemoryKind::Preference
        )
    {
        base *= 1.15;
    }

    if base == 0.0 {
        return 0.0;
    }

    let decay = cel_store::cortex_memory::decay_score(memory.last_accessed_at, now_secs);
    base * decay
}

fn compress_memory(memory: &cel_store::cortex_memory::CortexMemory) -> MemoryRef {
    let summary = memory
        .summary
        .clone()
        .unwrap_or_else(|| short_content_preview(&memory.content));
    MemoryRef {
        id: memory.id,
        kind: memory.kind.as_str().to_string(),
        summary,
        content: memory.content.clone(),
        created_at: Some(unix_to_iso(memory.created_at)),
    }
}

fn short_content_preview(content: &serde_json::Value) -> String {
    let s = serde_json::to_string(content).unwrap_or_default();
    if s.len() <= 80 {
        s
    } else {
        format!("{}…", &s[..80])
    }
}

fn unix_to_iso(secs: i64) -> String {
    // Best-effort ISO-8601 without pulling chrono. Same approach as the
    // canonical-runner outcome auto-write.
    let days = secs / 86_400;
    let remaining = secs % 86_400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;
    let (year, month, day) = unix_days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn unix_days_to_ymd(days_from_epoch: i64) -> (i64, u32, u32) {
    let mut days = days_from_epoch;
    let mut year: i64 = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days >= dy {
            days -= dy;
            year += 1;
        } else {
            break;
        }
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    for &dm in &months {
        let dm_actual = if month == 2 && is_leap(year) { 29 } else { dm };
        if days >= dm_actual {
            days -= dm_actual;
            month += 1;
        } else {
            break;
        }
    }
    (year, month, (days + 1) as u32)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn build_rationale(
    kept_elements: usize,
    memory_selection: &MemorySelection,
    omitted_elements: u32,
) -> Option<String> {
    if memory_selection.kept.is_empty()
        && memory_selection.omitted == 0
        && !memory_selection.workflow_empty
    {
        // Pure perception-only build (PR1 behaviour). Don't synthesise a
        // misleading rationale — leave the field absent so callers don't
        // think memory selection happened when it didn't.
        return None;
    }
    let mut parts = Vec::with_capacity(3);
    parts.push(format!(
        "Selected {} element(s) from {} candidate(s).",
        kept_elements,
        kept_elements as u32 + omitted_elements
    ));
    if memory_selection.workflow_empty {
        parts.push("No prior memories for this workflow.".into());
    } else {
        parts.push(format!(
            "Hydrated {} workflow memor{} (dropped {} below the goal-relevance + decay threshold).",
            memory_selection.kept.len(),
            if memory_selection.kept.len() == 1 {
                "y"
            } else {
                "ies"
            },
            memory_selection.omitted,
        ));
    }
    Some(parts.join(" "))
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
            memory_db_path: None,
            workflow_id: None,
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
            memory_db_path: None,
            workflow_id: None,
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
            memory_db_path: None,
            workflow_id: None,
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
            memory_db_path: None,
            workflow_id: None,
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
            memory_db_path: None,
            workflow_id: None,
        });
        assert!(view.elements.iter().all(|e| e.id != "hidden"));
    }

    // ─── PR3: memory-aware hydration ─────────────────────────────────────────

    fn pr3_temp_db(label: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!("cel_pr3_{label}_{nanos}.db"));
        path.to_string_lossy().into_owned()
    }

    fn seed_memory(
        store: &cel_store::CelStore,
        workflow: &str,
        kind: cel_store::cortex_memory::MemoryKind,
        summary: &str,
        content: serde_json::Value,
    ) -> i64 {
        store
            .insert_cortex_memory(&cel_store::cortex_memory::NewCortexMemory {
                workflow_id: workflow.into(),
                kind,
                content,
                summary: Some(summary.into()),
                tags: vec![],
                source_ref: None,
                embedding: None,
            })
            .expect("insert")
    }

    #[test]
    fn pr3_view_stays_empty_when_memory_inputs_missing() {
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: None,
            workflow_id: None,
        });
        assert!(view.memories.is_empty());
        assert_eq!(view.omitted_counts.memories, 0);
        assert!(view.selection_rationale.is_none());
    }

    #[test]
    fn pr3_relevant_memory_outranks_irrelevant_one() {
        let db_path = pr3_temp_db("rank");
        let store = cel_store::CelStore::open(&db_path).expect("open store");
        seed_memory(
            &store,
            "test-pr3",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Submitted invoice via Concur successfully",
            serde_json::json!({"goal": "submit invoice"}),
        );
        seed_memory(
            &store,
            "test-pr3",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Read morning headlines from Hacker News",
            serde_json::json!({"goal": "read news"}),
        );
        drop(store);

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: Some(&db_path),
            workflow_id: Some("test-pr3"),
        });

        assert!(
            !view.memories.is_empty(),
            "expected at least one hydrated memory; got 0"
        );
        // The submit-invoice memory must rank first.
        assert!(
            view.memories[0]
                .summary
                .to_lowercase()
                .contains("submitted invoice"),
            "expected submit-invoice memory first; got {:?}",
            view.memories[0].summary
        );

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn pr3_budget_caps_memory_count_and_records_omitted() {
        let db_path = pr3_temp_db("budget");
        let store = cel_store::CelStore::open(&db_path).expect("open store");
        // Seed 5 memories all referencing the goal keyword "form" so each
        // scores > 0.
        for i in 0..5 {
            seed_memory(
                &store,
                "wf",
                cel_store::cortex_memory::MemoryKind::Outcome,
                &format!("Submitted form attempt {i}"),
                serde_json::json!({"i": i}),
            );
        }
        drop(store);

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget {
            max_memories: 2,
            ..PlanningBudget::default()
        };
        let view = build_planning_view(&PlanningViewInputs {
            goal: "fill out form",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: Some(&db_path),
            workflow_id: Some("wf"),
        });

        assert_eq!(view.memories.len(), 2);
        assert_eq!(view.omitted_counts.memories, 3);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn pr3_workflow_with_no_memories_returns_empty_with_rationale() {
        let db_path = pr3_temp_db("empty_workflow");
        let store = cel_store::CelStore::open(&db_path).expect("open store");
        // Seed only OTHER workflow's memories — should not surface here.
        seed_memory(
            &store,
            "other-wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "did something",
            serde_json::json!({}),
        );
        drop(store);

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: Some(&db_path),
            workflow_id: Some("the-empty-workflow"),
        });

        assert!(view.memories.is_empty());
        assert_eq!(view.omitted_counts.memories, 0);
        let rationale = view.selection_rationale.expect("expected rationale");
        assert!(
            rationale.contains("No prior memories"),
            "expected empty-workflow rationale; got {rationale}"
        );
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn pr3_irrelevant_memories_score_zero_and_are_dropped() {
        let db_path = pr3_temp_db("irrelevant");
        let store = cel_store::CelStore::open(&db_path).expect("open store");
        // Fully off-topic memories with no goal-keyword overlap.
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Watered the plants in the kitchen",
            serde_json::json!({"plants": "many"}),
        );
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Outcome,
            "Rebooted the router after midnight",
            serde_json::json!({"router": "fixed"}),
        );
        drop(store);

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit invoice in Concur",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: Some(&db_path),
            workflow_id: Some("wf"),
        });

        // Both candidates score 0 → both omitted, none kept.
        assert_eq!(view.memories.len(), 0);
        assert_eq!(view.omitted_counts.memories, 2);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn pr3_store_open_failure_returns_empty_view_no_error() {
        // Invalid SQLite path — open will fail. The builder should log
        // and return an empty memory list, NOT panic or surface the error.
        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "any",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: Some("/dev/null/does-not-exist/bad.db"),
            workflow_id: Some("wf"),
        });
        assert!(view.memories.is_empty());
    }

    #[test]
    fn pr3_quoted_phrase_in_goal_boosts_matching_memory() {
        let db_path = pr3_temp_db("quoted");
        let store = cel_store::CelStore::open(&db_path).expect("open store");
        // Two memories — one matches the quoted phrase exactly.
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Prior,
            "Concur uses two-step submit",
            serde_json::json!({"app": "Concur"}),
        );
        seed_memory(
            &store,
            "wf",
            cel_store::cortex_memory::MemoryKind::Prior,
            "Some unrelated submit notes",
            serde_json::json!({}),
        );
        drop(store);

        let perception = make_context(vec![]);
        let caps = RuntimeCaps::default();
        let budget = PlanningBudget::default();
        let view = build_planning_view(&PlanningViewInputs {
            goal: "submit \"two-step submit\" form",
            budget: &budget,
            perception: &perception,
            caps: &caps,
            memory_db_path: Some(&db_path),
            workflow_id: Some("wf"),
        });

        assert!(view.memories[0].summary.contains("two-step"));
        let _ = std::fs::remove_file(&db_path);
    }
}
