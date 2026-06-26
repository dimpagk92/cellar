//! Convert `cel_cdp::DomElement`s into the `ContextElement` shape the
//! Cortex / planner already understand.
//!
//! Two responsibilities:
//!   1. Pick a stable, semantically meaningful `dom:*` element_id —
//!      scenario assertions like `target_contains: "submit"` substring-
//!      match against this, so we favour author-controlled identifiers
//!      (HTML id / name) over LLM-perceived ones (text, which shifts
//!      across page updates).
//!   2. Project DOM-specific attributes (placeholder, href, aria_role,
//!      backend_node_id, …) into the generic `ContextElement.properties`
//!      bag so the planner prompt can surface them without the runner
//!      having to special-case browser elements.
//!
//! Kept as a separate module from `lib.rs` because it has no I/O —
//! pure data conversion + 7 unit tests pinning the priority order,
//! sanitiser edge cases, and confidence-tier invariants.

use cel_cdp::DomElement;
use cel_context::{Bounds, ContextElement, ContextSource, ElementState};

/// Confidence assigned to a CDP-sourced element. Source of truth lives in
/// `adapters/browser/manifest.json` (`context.confidence`) and is loaded
/// into `AdapterManifest` via `include_str!` + cortex merge at startup —
/// duplicated as a literal here so this per-element mapper stays free of
/// runtime config loads (called once per DOM element on every tick). The
/// two must agree; `confidence_pinned_to_browser_dom_tier` in `lib.rs`
/// pins the manifest value, and a per-element snapshot test below pins
/// what flows out of this mapper.
const CDP_ELEMENT_CONFIDENCE: f64 = 0.88;

/// Maximum chars in a `dom:*` id_part. 60 covers typical authored
/// `id` / `name` / `aria-label` strings without bloating the planner's
/// element list with absurdly long ids.
const ID_PART_MAX_CHARS: usize = 60;

/// Convert a single `DomElement` into `ContextElement`.
///
/// `index` is the element's 0-based position in the DOM walk. It feeds
/// the last-resort `i{n}` fallback id when the element has no `id`,
/// `name`, `aria-label`, text, or `backend_node_id` — without it,
/// identifier-less elements would all collide on `dom:role:` and the
/// runner's substring dispatch would land on the first match.
pub fn dom_element_to_context_element(dom: &DomElement, index: usize) -> ContextElement {
    let id_part = pick_id_part(dom, index);
    let element_id = format!("dom:{}:{}", dom.element_type, id_part);

    let label = pick_label(dom);
    let bounds = dom.bounds.as_ref().map(|b| Bounds {
        x: b.x,
        y: b.y,
        width: b.width,
        height: b.height,
    });

    // CDP's interactive-element walker doesn't emit per-element focus
    // (it would require a separate document.activeElement lookup per
    // element — wasted JS round-trips). Defaults to false; the runner
    // fills focus from `cdp_current_url` + viewport state when needed.
    let state = ElementState {
        focused: false,
        enabled: dom.is_enabled,
        visible: dom.is_visible,
        selected: false,
        expanded: dom.is_expanded,
        checked: dom.is_checked,
    };

    let mut properties = std::collections::HashMap::new();
    if let Some(id) = dom.dom_id.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("dom_id".into(), id.to_string());
    }
    if let Some(name) = dom.dom_name.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("dom_name".into(), name.to_string());
    }
    if let Some(testid) = dom.data_testid.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("data_testid".into(), testid.to_string());
    }

    // Precompute the canonical CSS selector for this element so the
    // planner can paste it verbatim into `expect_after.selector`
    // (and into raw `cdp_eval` queries) instead of constructing one
    // by translating perception in its head. The 2026-05-13 trial
    // caught the planner emitting `.success-message` (class) when it
    // meant `#success-message` (id) — every click failure was a
    // mistranslation of `id="success-message"` into a class
    // selector. Surfacing the ready-to-use string removes the
    // translation step entirely.
    //
    // Preference order matches `pick_id_part` (the path that drives
    // the element_id): dom_id wins, then data-testid. Below that we
    // skip — anonymous elements get None and the planner falls back
    // to the bracket index, which the runtime resolves itself.
    let css_selector = if let Some(id) = dom.dom_id.as_deref().filter(|s| !s.is_empty()) {
        Some(format!("#{}", id))
    } else {
        dom.data_testid
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|testid| format!("[data-testid=\"{}\"]", testid))
    };
    if let Some(sel) = css_selector {
        properties.insert("css_selector".into(), sel);
    }
    if let Some(placeholder) = dom.placeholder.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("placeholder".into(), placeholder.to_string());
    }
    if let Some(href) = dom.href.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("url".into(), href.to_string());
    }
    if let Some(input_type) = dom.input_type.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("input_type".into(), input_type.to_string());
    }
    if let Some(role) = dom.aria_role.as_deref().filter(|s| !s.is_empty()) {
        properties.insert("aria_role".into(), role.to_string());
    }
    if let Some(node_id) = dom.backend_node_id {
        properties.insert("backend_node_id".into(), node_id.to_string());
    }
    properties.insert("viewport_relation".into(), dom.viewport_relation.clone());

    ContextElement {
        id: element_id,
        label,
        description: dom.aria_label.clone().filter(|s| !s.is_empty()),
        element_type: dom.element_type.clone(),
        value: dom.value.clone().filter(|s| !s.is_empty()),
        bounds,
        state,
        parent_id: None,
        actions: action_hints_for(&dom.element_type),
        confidence: CDP_ELEMENT_CONFIDENCE,
        // Pre-tag as Cdp so anyone reading the adapter's output before
        // it goes through the cortex (tests, telemetry, adapter-level
        // snapshots) sees the source attribution the cortex will end
        // up assigning. The cortex tick loop reads
        // `manifest.context.truth_surface == "browser_dom"` and lands
        // on the same `Cdp` value — pinning here too keeps the
        // pre-merge / post-merge behaviour consistent.
        source: ContextSource::Cdp,
        content_role: cel_context::classify_content_role(
            &dom.element_type,
            &action_hints_for(&dom.element_type),
            &ElementState::default(),
        ),
        properties,
    }
}

/// Pick the most stable, semantically meaningful identifier for the
/// element. Priority: HTML id → HTML name → aria-label → visible text →
/// backend_node_id → DOM-walk index.
///
/// Author-controlled strings (id/name) come first because scenario
/// assertions are author-controlled too — `target_contains: "submit"`
/// matches `id="submit-btn"` predictably across runs. LLM-perceived
/// strings (text) shift when the page updates, so they're a worse base
/// for cross-turn referenceability.
fn pick_id_part(dom: &DomElement, index: usize) -> String {
    // CEL tag is the in-walk counter that the CDP walker stamps onto the
    // DOM as `data-cel-tag="<n>"`. When no HTML id / name is present this
    // is the ONLY 1:1 anchor between perception and dispatch — eliminates
    // the perception-slug ↔ raw-innerText mismatch that caused no-match
    // failures on Apple, allrecipes etc.
    //
    // Format: `t<n>` (the 't' prefix lets dispatch detect it and look up
    // via `[data-cel-tag="<n>"]` selector). Falls through to the existing
    // label-based fallbacks for backwards compat with snapshots / fixtures
    // that may not stamp tags.
    let cel_tag = dom.backend_node_id.filter(|&n| n > 0);
    if let Some(s) = dom.dom_id.as_deref().filter(|s| !s.trim().is_empty()) {
        return sanitize_id_part(s);
    }
    if let Some(s) = dom.dom_name.as_deref().filter(|s| !s.trim().is_empty()) {
        return sanitize_id_part(s);
    }
    // PREFER cel_tag over slug-of-text/aria-label/testid. Tag is exact;
    // the slugs are best-effort heuristics that break on common patterns
    // (label has spaces, raw innerText doesn't have dashes, etc.). Slugs
    // remain available IN PROPERTIES for the planner to read as descriptive
    // hints but they're no longer the dispatch anchor.
    if let Some(n) = cel_tag {
        return format!("t{n}");
    }
    // `data-testid` slots in BEFORE `aria_label` and `text` because it is
    // author-stamped specifically as a stable identifier — most resistant to
    // animation/state churn. Pages with many same-text buttons (a queue of
    // `<button>Approve</button>` rows) rely on this: without it every
    // approve button gets the same `dom:button:approve` and dispatch picks
    // the first match. With it each row gets a unique
    // `dom:button:approve-<row-key>` and the planner can target rows
    // individually.
    if let Some(s) = dom.data_testid.as_deref().filter(|s| !s.trim().is_empty()) {
        return sanitize_id_part(s);
    }
    if let Some(s) = dom.aria_label.as_deref().filter(|s| !s.trim().is_empty()) {
        return sanitize_id_part(s);
    }
    if !dom.text.trim().is_empty() {
        return sanitize_id_part(&dom.text);
    }
    if let Some(node_id) = dom.backend_node_id {
        return format!("n{node_id}");
    }
    format!("i{index}")
}

/// Pick a human-readable label. Buttons surface their visible text;
/// inputs prefer placeholder, then aria-label, then dom_id so the
/// planner sees what the field is for. Missing labels become `None`
/// rather than empty string.
fn pick_label(dom: &DomElement) -> Option<String> {
    if !dom.text.trim().is_empty() {
        return Some(dom.text.clone());
    }
    if let Some(s) = dom.placeholder.as_deref().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(s) = dom.aria_label.as_deref().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    if let Some(s) = dom.dom_id.as_deref().filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    None
}

/// Mirror what AX provides for actionable elements. The runner's
/// action-arbiter reads `actions` to decide whether `set_value` /
/// `ax_action subtype="press"` is allowed for the target — without
/// hints, browser-DOM-sourced inputs would silently fail with the same
/// `no_actions_declared` error AX-sourced elements give.
pub(crate) fn action_hints_for(element_type: &str) -> Vec<String> {
    match element_type {
        "button" | "link" => vec!["press".into(), "click".into()],
        "input" | "textarea" | "searchfield" => vec!["set_value".into(), "click".into()],
        "select" | "combobox" => vec!["set_value".into()],
        "checkbox" | "radio" => vec!["press".into(), "click".into()],
        _ => Vec::new(),
    }
}

/// Lower-case, replace non-`[a-z0-9_-]` runes with `-`, collapse runs
/// of `-`, trim, truncate to `ID_PART_MAX_CHARS`. Keeps `dom:role:id`
/// element_ids well-formed regardless of how chaotic the underlying
/// author markup is — `id="Submit Form!"` becomes `submit-form`,
/// substring-match still hits on `submit`.
pub fn sanitize_id_part(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_was_dash = false;
    for ch in lower.chars().take(ID_PART_MAX_CHARS) {
        let keep = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        if keep {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        // Fully-non-alphanumeric input — fall back to a placeholder so
        // the element_id stays well-formed.
        "x".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_cdp::ElementBounds;

    fn dom(
        element_type: &str,
        dom_id: Option<&str>,
        dom_name: Option<&str>,
        aria_label: Option<&str>,
        text: &str,
    ) -> DomElement {
        DomElement {
            tag: element_type.into(),
            element_type: element_type.into(),
            text: text.into(),
            href: None,
            input_type: None,
            value: None,
            placeholder: None,
            dom_id: dom_id.map(str::to_string),
            dom_name: dom_name.map(str::to_string),
            data_testid: None,
            bounds: Some(ElementBounds {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            }),
            backend_node_id: Some(42),
            aria_role: None,
            aria_label: aria_label.map(str::to_string),
            is_visible: true,
            is_enabled: true,
            is_checked: None,
            is_expanded: None,
            shadow_depth: 0,
            paint_order: 1,
            viewport_relation: "visible".into(),
        }
    }

    #[test]
    fn dom_id_wins_over_name_label_text() {
        // Most authored, stable identifier → drives the element_id so
        // scenarios written ahead of the run still match.
        let el = dom_element_to_context_element(
            &dom(
                "button",
                Some("submit-btn"),
                Some("submit"),
                Some("Submit"),
                "Submit",
            ),
            0,
        );
        assert_eq!(el.id, "dom:button:submit-btn");
    }

    #[test]
    fn falls_back_through_name_then_aria_then_text() {
        // With no `data-cel-tag` stamped (backend_node_id = None) the id
        // falls back through the label chain: name → aria-label → text.
        // (When a tag IS present it wins — see
        // `final_fallback_uses_cel_tag_then_walk_index`.)
        let mut named = dom("input", None, Some("email"), Some("Email"), "");
        named.backend_node_id = None;
        let no_id = dom_element_to_context_element(&named, 0);
        assert_eq!(no_id.id, "dom:input:email");

        let mut aria_only = dom("input", None, None, Some("Email address"), "");
        aria_only.backend_node_id = None;
        let no_id_or_name = dom_element_to_context_element(&aria_only, 0);
        assert_eq!(no_id_or_name.id, "dom:input:email-address");

        let mut text_only = dom("button", None, None, None, "Click me!");
        text_only.backend_node_id = None;
        let only_text = dom_element_to_context_element(&text_only, 0);
        assert_eq!(only_text.id, "dom:button:click-me");
    }

    #[test]
    fn data_testid_breaks_text_collisions_for_button_lists() {
        // Real-world fixture: a deployment queue renders many
        // `<button>Approve</button>` rows that all share the same text.
        // Absent an exact `data-cel-tag` (backend_node_id = None), the
        // `data-testid` carries the row's identity
        // (`approve-payment-gateway`) so the planner can target a specific
        // row instead of every row collapsing to `dom:button:approve`.
        let mut d = dom("button", None, None, None, "Approve");
        d.data_testid = Some("approve-payment-gateway".into());
        d.backend_node_id = None;
        let el = dom_element_to_context_element(&d, 0);
        assert_eq!(el.id, "dom:button:approve-payment-gateway");

        // dom_id still wins when present (testid is fallback).
        let mut d2 = dom("button", Some("btn-approve"), None, None, "Approve");
        d2.data_testid = Some("approve-payment-gateway".into());
        let el2 = dom_element_to_context_element(&d2, 0);
        assert_eq!(el2.id, "dom:button:btn-approve");
    }

    #[test]
    fn data_testid_propagates_to_properties_for_prompt_rendering() {
        // The planner reads `data_testid` out of properties when
        // rendering the perception line so it sees `testid="..."` on
        // the element and can use it as a verbatim id_part.
        let mut d = dom("button", None, None, None, "Approve");
        d.data_testid = Some("approve-row-3".into());
        let el = dom_element_to_context_element(&d, 0);
        assert_eq!(
            el.properties.get("data_testid").map(String::as_str),
            Some("approve-row-3")
        );
    }

    #[test]
    fn css_selector_prefers_dom_id_over_data_testid() {
        // When both are present, `#id` wins — it's shorter and the
        // most universal CSS selector. Matches `pick_id_part`'s
        // priority order, so element_id and selector point at the
        // same element by the same convention.
        let mut d = dom("button", Some("btn-submit"), None, None, "Submit");
        d.data_testid = Some("submit-cta".into());
        let el = dom_element_to_context_element(&d, 0);
        assert_eq!(
            el.properties.get("css_selector").map(String::as_str),
            Some("#btn-submit"),
            "dom_id should win over data_testid"
        );
    }

    #[test]
    fn css_selector_falls_back_to_data_testid_when_no_dom_id() {
        // `data-testid` is the canonical "stable identifier when
        // there's no HTML id" — component libraries (Radix, MUI)
        // and test harnesses all ship it. The selector emits the
        // exact attribute syntax so the planner pastes it verbatim
        // into expect_after.
        let mut d = dom("button", None, None, None, "Approve");
        d.data_testid = Some("approve-pgw".into());
        let el = dom_element_to_context_element(&d, 0);
        assert_eq!(
            el.properties.get("css_selector").map(String::as_str),
            Some("[data-testid=\"approve-pgw\"]"),
            "fallback selector should use data-testid attribute form"
        );
    }

    #[test]
    fn css_selector_absent_when_no_stable_identifier() {
        // No HTML id, no data-testid → no precomputed selector. The
        // planner should fall back to the bracket index. Emitting a
        // tag-only selector (`button`) or a class guess
        // (`.btn-approve`) would be worse than no selector since
        // both are non-unique and prone to mismatch.
        let d = dom("button", None, None, Some("Approve"), "");
        let el = dom_element_to_context_element(&d, 0);
        assert!(
            !el.properties.contains_key("css_selector"),
            "no stable identifier → no css_selector"
        );
    }

    #[test]
    fn final_fallback_uses_cel_tag_then_walk_index() {
        // backend_node_id (> 0) is stamped as `data-cel-tag` and becomes
        // the exact `t<n>` anchor — preferred over slug fallbacks.
        let mut empty = dom("button", None, None, None, "");
        empty.backend_node_id = Some(7);
        let el = dom_element_to_context_element(&empty, 3);
        assert_eq!(el.id, "dom:button:t7");

        // No tag and no labels → DOM-walk index.
        empty.backend_node_id = None;
        let el = dom_element_to_context_element(&empty, 3);
        assert_eq!(el.id, "dom:button:i3");
    }

    #[test]
    fn id_part_sanitised_for_safe_dispatch() {
        // The id_part flows into a JS substring-match string. Surprising
        // characters get normalised to `-` so the planner sees consistent
        // shapes regardless of how chaotic the underlying author markup is.
        assert_eq!(sanitize_id_part("Submit Form!"), "submit-form");
        assert_eq!(sanitize_id_part("  Multi   spaces  "), "multi-spaces");
        assert_eq!(sanitize_id_part("---only-dashes---"), "only-dashes");
        assert_eq!(sanitize_id_part("@@@"), "x");
        assert_eq!(sanitize_id_part("UPPER_lower-MixED"), "upper_lower-mixed");
    }

    #[test]
    fn input_action_hints_include_set_value() {
        // Without these hints, the action-arbiter would refuse
        // `set_value` for browser-DOM-sourced inputs with the same
        // `no_actions_declared` error AX inputs give. Pin the contract.
        let el = dom_element_to_context_element(&dom("input", Some("name"), None, None, ""), 0);
        assert!(el.actions.iter().any(|a| a == "set_value"));

        let el =
            dom_element_to_context_element(&dom("button", Some("submit"), None, None, "Submit"), 0);
        assert!(el.actions.iter().any(|a| a == "press"));
    }

    #[test]
    fn label_and_value_propagate_to_context_element() {
        // The planner prompt prints `[N] type "label"` for each
        // element. label being None when text/placeholder/aria are all
        // empty would render as `[N] input ""` — useless. Pin that the
        // mapper picks something user-facing whenever possible.
        let mut d = dom("input", Some("email"), None, None, "");
        d.placeholder = Some("Enter your email".into());
        d.value = Some("alice@example.com".into());
        let el = dom_element_to_context_element(&d, 0);
        assert_eq!(el.label.as_deref(), Some("Enter your email"));
        assert_eq!(el.value.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn properties_carry_dom_specific_attributes() {
        // The planner doesn't have a dedicated DOM-shape — it reads
        // properties for hints. Pin that the mapper projects authored
        // attributes into the bag so a scenario like
        // `<input id="email" placeholder="Your email" name="email"
        //         type="email">` lets the planner see all four
        // attributes without any browser-special-case prompt code.
        let mut d = dom("input", Some("email"), Some("email"), None, "");
        d.placeholder = Some("Your email".into());
        d.input_type = Some("email".into());
        d.aria_role = Some("textbox".into());
        let el = dom_element_to_context_element(&d, 0);
        assert_eq!(
            el.properties.get("dom_id").map(String::as_str),
            Some("email")
        );
        assert_eq!(
            el.properties.get("dom_name").map(String::as_str),
            Some("email")
        );
        assert_eq!(
            el.properties.get("placeholder").map(String::as_str),
            Some("Your email")
        );
        assert_eq!(
            el.properties.get("input_type").map(String::as_str),
            Some("email")
        );
        assert_eq!(
            el.properties.get("aria_role").map(String::as_str),
            Some("textbox")
        );
        assert!(el.properties.contains_key("backend_node_id"));
        assert!(el.properties.contains_key("viewport_relation"));
    }
}
