//! Chrome DevTools Protocol binding, evaluation, and DOM/input dispatch.
//!
//! CDP client lifecycle (`bind_browser_cdp_url`, `set_cdp_client`, accessors),
//! page helpers (`cdp_*`), and the low-level primitives the browser execution
//! path uses: key / keycombo / type via `Input.dispatchKeyEvent`, click and
//! set-value JS builders, extraction expressions, and the injected JS constants
//! (overlay dismissal, DOM snapshot, `<select>` patch).

use super::dispatch::{action_dom_target, action_expect_after, action_type_str, wait_for_effect};
use super::receipt::{attach_receipt, current_run_id, new_receipt_id, now_ms, record_receipt};
use super::*;
use cel_contracts::{
    DispatchRoute, ExecutionReceipt, ObservedEffect, ObservedStatus, ReceiptStatus,
};

/// Detect whether a selector-string entry is a raw JS expression
/// (common prefixes the LLM uses) vs a bare CSS selector, and wrap
/// bare selectors into `querySelector` calls that safely fall through
/// to `null` on miss.
pub(crate) fn build_extract_expression(sel: &str) -> String {
    let trimmed = sel.trim();
    let looks_like_js = trimmed.starts_with("function")
        || trimmed.starts_with("(function")
        || trimmed.starts_with("(() =>")
        || trimmed.starts_with("(()=>")
        || trimmed.starts_with("return ")
        || trimmed.starts_with("document.")
        || trimmed.starts_with("window.")
        || trimmed.contains("=>");
    if looks_like_js {
        trimmed.to_string()
    } else if trimmed.contains(":contains(") || trimmed.contains(":has(") {
        let escaped = trimmed.replace('\\', "\\\\").replace('\'', "\\'");
        format!(
            "(function() {{
                const selector = '{escaped}';
                const textOf = (el) => (el && el.textContent != null ? el.textContent.trim() : '');
                const includesText = (el, needle) => textOf(el).includes(needle);

                const rowMatch = selector.match(/^([a-zA-Z0-9_-]+):has\\(([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\3\\)\\)\\s+([a-zA-Z0-9_-]+):nth-child\\((\\d+)\\)$/);
                if (rowMatch) {{
                    const [, rowTag, innerTag, , needle, cellTag, nth] = rowMatch;
                    const rows = Array.from(document.querySelectorAll(rowTag));
                    for (const row of rows) {{
                        const match = Array.from(row.querySelectorAll(innerTag)).find((child) => includesText(child, needle));
                        if (!match) continue;
                        const idx = Math.max(parseInt(nth, 10) - 1, 0);
                        const cells = Array.from(row.querySelectorAll(cellTag));
                        const target = cells[idx];
                        return target ? textOf(target) || null : null;
                    }}
                    return null;
                }}

                const adjacentMatch = selector.match(/^([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\2\\)\\s*\\+\\s*([a-zA-Z0-9_-]+)$/);
                if (adjacentMatch) {{
                    const [, baseTag, , needle, siblingTag] = adjacentMatch;
                    const bases = Array.from(document.querySelectorAll(baseTag));
                    for (const base of bases) {{
                        if (!includesText(base, needle)) continue;
                        let sibling = base.nextElementSibling;
                        while (sibling) {{
                            if (sibling.matches(siblingTag)) {{
                                return textOf(sibling) || null;
                            }}
                            sibling = sibling.nextElementSibling;
                        }}
                    }}
                    return null;
                }}

                const siblingNthMatch = selector.match(/^([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\2\\)\\s*~\\s*([a-zA-Z0-9_-]+):nth-of-type\\((\\d+)\\)$/);
                if (siblingNthMatch) {{
                    const [, baseTag, , needle, siblingTag, nth] = siblingNthMatch;
                    const bases = Array.from(document.querySelectorAll(baseTag));
                    for (const base of bases) {{
                        if (!includesText(base, needle) || !base.parentElement) continue;
                        const idx = Math.max(parseInt(nth, 10) - 1, 0);
                        const matches = Array.from(base.parentElement.children).filter((child) => child.matches(siblingTag));
                        const target = matches[idx];
                        return target ? textOf(target) || null : null;
                    }}
                    return null;
                }}

                const containsOnlyMatch = selector.match(/^([a-zA-Z0-9_-]+):contains\\((['\\\"])(.*?)\\2\\)$/);
                if (containsOnlyMatch) {{
                    const [, tag, , needle] = containsOnlyMatch;
                    const match = Array.from(document.querySelectorAll(tag)).find((el) => includesText(el, needle));
                    return match ? textOf(match) || null : null;
                }}

                const fallback = document.querySelector(selector);
                return fallback ? (fallback.textContent == null ? null : fallback.textContent.trim()) : null;
            }})()"
        )
    } else {
        // Bare CSS selector. Escape single quotes for JS-string embedding.
        let escaped = trimmed.replace('\\', "\\\\").replace('\'', "\\'");
        format!(
            "(function() {{ var el = document.querySelector('{escaped}'); \
             return el ? (el.textContent == null ? null : el.textContent.trim()) : null; }})()"
        )
    }
}

/// Flatten a CDP `Runtime.evaluate` result into a string we can parse.
/// Returns `None` when the JS side returned `null`/`undefined` or an
/// empty result object.
pub(crate) fn cdp_value_to_string(v: &serde_json::Value) -> Option<String> {
    // The client already extracts `result.result.value` — the raw value
    // is at the top of `v`. Accept strings, numbers, booleans; reject
    // null/undefined/missing.
    if v.is_null() {
        return None;
    }
    if let Some(s) = v.as_str() {
        if s.is_empty() {
            return None;
        }
        return Some(s.to_string());
    }
    if let Some(n) = v.as_f64() {
        return Some(n.to_string());
    }
    if let Some(b) = v.as_bool() {
        return Some(b.to_string());
    }
    // Object/array: stringify. This catches cases where the JS returned
    // a node ref (rare) or an object.
    let s = serde_json::to_string(v).ok()?;
    if s == "null" || s == "\"\"" {
        return None;
    }
    Some(s)
}

/// Parse the raw string yielded by a selector according to the
/// planner's `parse_as` hint. Unknown hints fall back to "text".
pub(crate) fn parse_extracted(raw: &str, parse_as: &str) -> Option<serde_json::Value> {
    let cleaned = raw.trim();
    match parse_as.to_lowercase().as_str() {
        "float" | "number" => {
            let stripped: String = cleaned
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            stripped.parse::<f64>().ok().map(|n| {
                serde_json::Number::from_f64(n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::String(raw.to_string()))
            })
        }
        "int" | "integer" => {
            let stripped: String = cleaned
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            stripped
                .parse::<i64>()
                .ok()
                .map(|n| serde_json::Value::Number(n.into()))
        }
        _ => Some(serde_json::Value::String(cleaned.to_string())),
    }
}

/// Try to dispatch the action through CDP. Returns:
///  * `Ok(Some(result))` — we handled it (succeeded or failed via CDP)
///  * `Ok(None)` — not a browser-targeted action; caller should fall back
///    to the native execution path
///
/// Targets a `dom:*` element by parsing the embedded backend_node_id (the
/// element id format is `dom:<element_type>:<id>` per the CDP context pump
/// in cel-eval). For typing we use Runtime.evaluate to set the value AND
/// dispatch input/change events (otherwise React/Vue forms ignore the
/// programmatic value). For clicks we use Runtime.evaluate to find the
/// element and call .click() — element-level, not coordinate-level, so it
/// works with shadow DOM and is robust against scroll position.
pub(crate) async fn try_cdp_dispatch(
    client: &cel_cdp::CdpClient,
    action: &PlannedAction,
) -> Result<Option<crate::adapter::ActionResult>, CortexError> {
    let target = action_dom_target(action);
    let Some(target) = target else {
        return Ok(None);
    };
    if !target.starts_with("dom:") {
        return Ok(None);
    }

    // dom:<role>:<id>   — id is the JS-stable handle we wrote into the model.
    // For elements pumped from cel_cdp::extract_page_content, the id is the
    // CDP backend_node_id (when available). The selector we use is
    // `[data-cel-id]` if present, otherwise we fall back to backend_node_id.
    // Practical resolution: query the DOM by walking the interactive index
    // we recorded — but cleanest is to round-trip via JS that reads the
    // backend_node_id off element.dataset.celBackendId or scans for the
    // element's stable signature. For this slice we use a simple scheme:
    // query by matching element_type and falling back to text content.
    let parts: Vec<&str> = target.splitn(3, ':').collect();
    let (role, id_part) = match parts.as_slice() {
        ["dom", role, id] => (*role, *id),
        _ => return Ok(None),
    };

    // Stamp the receipt clock here so timing covers the full CDP handling
    // window (pre-dispatch snapshot + dispatch + effect wait).
    let requested_at_ms = now_ms();

    // Capture a "before" page snapshot when `expect_after` is the
    // diff-based DomChanged variant. Has to happen BEFORE dispatch
    // because dispatch will mutate the very state we're comparing
    // against. Other expectation variants (SelectorAppears, etc.)
    // don't need a baseline — they poll an absolute predicate — so
    // we skip the round-trip for them.
    let before_snapshot = match action_expect_after(action) {
        Some(EffectExpectation::DomChanged { .. }) => {
            // `CdpClient::evaluate` already unwraps `result.value` and
            // returns the JS expression's return value directly — so the
            // snapshot IIFE's stringified output arrives as
            // `Value::String("…")`, not the raw `{result:{value:"…"}}`
            // CDP envelope. Earlier code re-dug into `result.value`,
            // which always produced `None` and silently degraded
            // `wait_for_effect` into a never-matches comparison (see
            // `check_cdp_ok` for the corresponding `unwrap_or(res)`
            // fallback that hid this from click dispatch).
            match client.evaluate(DOM_SNAPSHOT_JS).await {
                Ok(value) => value.as_str().map(str::to_string),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "dom_changed: pre-dispatch snapshot failed; falling back to dispatch-only"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let dispatch_result = match action {
        PlannedAction::Click { .. } | PlannedAction::AxAction { .. } => {
            let js = build_click_js(role, id_part);
            let res = client
                .evaluate(&js)
                .await
                .map_err(|e| CortexError::ExecutionFailed(format!("cdp click: {e}")))?;
            check_cdp_ok(res, "clicked")
        }
        PlannedAction::SetValue { value, .. } => {
            let js = build_set_value_js(role, id_part, value);
            let res = client
                .evaluate(&js)
                .await
                .map_err(|e| CortexError::ExecutionFailed(format!("cdp set_value: {e}")))?;
            check_cdp_ok(res, "set")
        }
        PlannedAction::Type { text, .. } => {
            // Browser-safe Type: focus + set value + dispatch input/change.
            let js = build_set_value_js(role, id_part, text);
            let res = client
                .evaluate(&js)
                .await
                .map_err(|e| CortexError::ExecutionFailed(format!("cdp type: {e}")))?;
            check_cdp_ok(res, "typed")
        }
        _ => return Ok(None),
    };

    // Effect verification: when the planner attached an `expect_after`
    // to the action, the dispatch ok above means "the JS function was
    // called", NOT "the page reacted as expected". A click handler that
    // calls `e.preventDefault()` (form validation), a remounted DOM
    // node the click landed on but is no longer wired up, an animation
    // that swallowed the click — all return ok at dispatch but leave
    // the page unchanged.
    //
    // Poll the page (Runtime.evaluate of the expectation's predicate)
    // until it holds or the timeout fires. If the timeout fires we
    // convert the action's ok into a fail with a structured message
    // that names the expectation and what we observed instead — the
    // planner sees that immediately in next-turn history and can
    // retry / pivot without going through verify_done's screenshot
    // grader.
    let (result, observed) = if !dispatch_result.success {
        // Dispatch itself failed — no effect verification was attempted.
        (dispatch_result, ObservedEffect::not_checked())
    } else if let Some(expectation) = action_expect_after(action) {
        match wait_for_effect(client, expectation, before_snapshot.as_deref()).await {
            Ok(()) => (
                dispatch_result,
                ObservedEffect::selector_observed(expectation.clone()),
            ),
            Err(reason) => (
                crate::adapter::ActionResult::fail(reason.clone()),
                ObservedEffect::selector_timed_out(expectation.clone(), reason),
            ),
        }
    } else {
        (dispatch_result, ObservedEffect::not_checked())
    };

    // Build the canonical execution receipt for this CDP-routed action. Read
    // the Copy `status` before moving `observed` into the receipt.
    let status = if result.success {
        ReceiptStatus::Ok
    } else if observed.status == ObservedStatus::TimedOut {
        ReceiptStatus::TimedOut
    } else {
        ReceiptStatus::Failed
    };
    let completed_at_ms = now_ms();
    let receipt = ExecutionReceipt {
        receipt_id: new_receipt_id(),
        run_id: current_run_id(),
        trace_id: None,
        action_kind: action_type_str(action).to_string(),
        target: Some(target.to_string()),
        route: DispatchRoute::Cdp,
        observed_effect: observed,
        evidence: Vec::new(),
        requested_at_ms,
        completed_at_ms,
        duration_ms: completed_at_ms.saturating_sub(requested_at_ms),
        status,
        error: result.error.clone(),
    };
    record_receipt(&receipt);
    Ok(Some(attach_receipt(result, receipt)))
}

pub(crate) fn check_cdp_ok(
    res: serde_json::Value,
    op: &'static str,
) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    let v = res
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(res);
    match v {
        serde_json::Value::String(s) if s.starts_with("ok:") => ActionResult::ok(),
        serde_json::Value::String(s) => ActionResult::fail(format!("cdp {op}: {s}")),
        serde_json::Value::Bool(true) => ActionResult::ok(),
        other => ActionResult::fail(format!("cdp {op}: unexpected result {other}")),
    }
}

/// Build JS that finds an element by id_part (extracted from a `dom:role:id`
/// element_id) and clicks it. Tries four resolution paths in order:
///   1. `document.getElementById(idPart)` — fast path when the planner
///      passes an HTML id verbatim (e.g. `dom:button:submit-btn` →
///      `getElementById("submit-btn")`). Most common case after PR #66's
///      prompt change steered the planner toward typed `dom:*` actions.
///   2. Numeric → 0-based index into the visible-candidate list.
///   3. Substring search over ALL identifying attributes concatenated
///      (id + name + innerText + value + aria-label). The previous
///      first-truthy-wins chain hid `id`/`name` whenever any other
///      attribute was set, which caused `set_value dom:input:name` to
///      miss inputs that did have `name="name"` because `placeholder`
///      ("John Doe") was tested first. The concat fixes that whole class
///      of false-negatives.
///
/// Decode a JPEG-or-PNG byte buffer into a `cel_display::Frame`. Used by
/// the CDP-screenshot vision fallback installed in `Cortex::boot` so the
/// merger can hand the vision provider a real Frame even when the host
/// display capture (xcap) is unavailable (headless Linux, no monitors).
///
/// CDP's `Page.captureScreenshot` returns PNG by default and JPEG when
/// the request asks for `format: "jpeg"` — `cel_cdp::CdpClient::capture_screenshot`
/// asks for JPEG. We accept either; `image::load_from_memory` sniffs the
/// magic bytes.
pub(crate) fn decode_png_to_frame(bytes: &[u8]) -> Result<cel_display::Frame, image::ImageError> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(cel_display::Frame {
        data: rgba.into_raw(),
        width,
        height,
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    })
}

fn build_click_js(role: &str, id_part: &str) -> String {
    let role_js = serde_json::to_string(role).unwrap_or_else(|_| "\"button\"".into());
    let id_js = serde_json::to_string(id_part).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
            const role = {role_js};
            const idPart = {id_js};
            const tagFor = (r) => ({{
                button: ['button', 'a[role="button"]', 'input[type="submit"]', 'input[type="button"]'],
                link: ['a[href]'],
                input: ['input:not([type="submit"]):not([type="button"])', 'textarea'],
                textarea: ['textarea'],
                select: ['select'],
                checkbox: ['input[type="checkbox"]'],
                radio: ['input[type="radio"]'],
            }})[r] || ['*'];
            const sels = tagFor(role).join(',');
            const candidates = Array.from(document.querySelectorAll(sels))
                .filter(el => el.offsetParent !== null);
            let target = null;
            // 0. EXACT match via `data-cel-tag` (the CDP walker's
            //    in-walk counter). When perception had no HTML id /
            //    name to use, pick_id_part emits `t<n>` and the walker
            //    stamps `data-cel-tag="<n>"` on the SAME DOM node — so
            //    dispatch can resolve back to the EXACT element with
            //    zero string-shape guessing. This is the long-run fix
            //    for the slug ↔ raw-text mismatch class of failures.
            //    Tag survives within the current tick's dispatch
            //    window; gets overwritten on the next perception walk.
            if (typeof idPart === 'string' && /^t\d+$/.test(idPart)) {{
                const n = idPart.slice(1);
                const byTag = document.querySelector('[data-cel-tag="' + n + '"]');
                if (byTag) {{ target = byTag; }}
            }}
            // 1. Exact HTML id match — the most common case once the
            //    planner emits dom:* element_ids derived from id="...".
            //    Restricted to safe id chars so an injected idPart can't
            //    drop a CSS-injection payload into `getElementById`.
            if (!target && typeof idPart === 'string' && /^[A-Za-z][\w:.-]*$/.test(idPart)) {{
                const byId = document.getElementById(idPart);
                if (byId && candidates.includes(byId)) {{
                    target = byId;
                }}
            }}
            // 2. Numeric → index into visible candidates (back-compat
            //    with index-based dom:* ids the older browser walker
            //    produced before PR #49 added id/name capture).
            if (!target) {{
                const asNum = parseInt(idPart, 10);
                if (!isNaN(asNum) && String(asNum) === idPart) {{
                    target = candidates[asNum] || null;
                }}
            }}
            // 3. Substring search over a CONCATENATION of identifying
            //    attributes (was: first-truthy-wins which silently
            //    dropped id/name when innerText/value were also set).
            //    `data-testid` is included because perception's
            //    `pick_id_part` uses it as a stable-id fallback when
            //    the element has no HTML `id`/`name`/`aria-label` —
            //    without it here, the planner emits
            //    `dom:button:approve-payment-gateway` (correctly
            //    derived from `data-testid="approve-payment-gateway"`)
            //    and dispatch returns `no-match` because no
            //    `id`/`name`/`innerText`/`value`/`aria-label` ever
            //    contains the slug. Caught on 2026-05-13 trial of
            //    `recover_from_stale_state`. Same fix mirrored in
            //    `build_set_value_js` below.
            if (!target) {{
                // The needle is the LLM-supplied id_part. Perception
                // SLUGIFIES element text/aria-label before exposing as
                // a dom: id (e.g. "Buy MacBook Air" → "buy-macbook-air").
                // The raw innerText doesn't contain the dashed slug, so
                // a literal substring search misses. Compare on TWO
                // canonical forms: the raw concat AND a normalised
                // version where everything non-alphanumeric collapses
                // to a single dash. Then the same normaliser applied
                // to the candidate's attrs gives "buy-macbook-air"
                // from "Buy MacBook Air" — match. Caught on 2026-05-26
                // WV trace where Apple's hero "Buy" link kept failing
                // because perception said dom:link:buy-macbook-air and
                // build_click_js's literal search couldn't find it.
                const normalize = s => String(s || '')
                    .toLowerCase()
                    .replace(/[^a-z0-9]+/g, '-')
                    .replace(/^-+|-+$/g, '');
                const needle = String(idPart).toLowerCase();
                const needleNorm = normalize(idPart);
                target = candidates.find(el => {{
                    const parts = [
                        el.id,
                        el.name,
                        el.innerText,
                        el.value,
                        el.getAttribute('aria-label'),
                        el.getAttribute('data-testid'),
                        el.getAttribute('data-test'),
                        el.getAttribute('data-cy'),
                        el.getAttribute('data-qa'),
                        el.getAttribute('title'),
                    ];
                    const concat = parts.filter(Boolean).join(' ');
                    const t = concat.toLowerCase();
                    if (t.includes(needle)) return true;
                    // Slug-aware fallback: normalise concat and compare
                    // against the normalised needle. Catches the common
                    // "Buy MacBook Air" → "buy-macbook-air" case.
                    if (needleNorm.length >= 3) {{
                        const tNorm = normalize(concat);
                        if (tNorm.includes(needleNorm)) return true;
                    }}
                    return false;
                }}) || null;
            }}
            if (!target) return 'no-match:' + role + ':' + idPart;
            target.scrollIntoView({{ block: 'center', inline: 'center' }});
            target.click();
            return 'ok:click';
        }})()"#
    )
}

/// CDP fallback for `PlannedAction::Key` when the cortex is bound to
/// a browser. Generates a keyDown+keyUp pair via `Input.dispatchKeyEvent`
/// targeting the bound page directly — no dependency on OS-level
/// keyboard focus.
///
/// Maps the cellar key vocabulary (`Return`, `Tab`, `Down`, single chars)
/// to CDP key-event fields. Names follow the W3C `KeyboardEvent.key` /
/// `KeyboardEvent.code` spec, which is what CDP expects:
/// see https://chromedevtools.github.io/devtools-protocol/tot/Input/#method-dispatchKeyEvent
pub(crate) async fn dispatch_key_via_cdp(
    client: &cel_cdp::CdpClient,
    key: &str,
) -> crate::adapter::ActionResult {
    let event = key_to_cdp_event(key);
    // keyDown — actual key activation.
    let down_params = serde_json::json!({
        "type": "keyDown",
        "key": event.key,
        "code": event.code,
        "text": event.text,
        "unmodifiedText": event.text,
        "windowsVirtualKeyCode": event.vk,
        "nativeVirtualKeyCode": event.vk,
    });
    if let Err(e) = client
        .send_command("Input.dispatchKeyEvent", down_params)
        .await
    {
        return crate::adapter::ActionResult::fail(format!("cdp key keyDown: {e}"));
    }
    // keyUp — paired release. Some sites depend on the keyup event
    // firing (e.g. autocomplete that listens for keyup).
    let up_params = serde_json::json!({
        "type": "keyUp",
        "key": event.key,
        "code": event.code,
        "windowsVirtualKeyCode": event.vk,
        "nativeVirtualKeyCode": event.vk,
    });
    if let Err(e) = client
        .send_command("Input.dispatchKeyEvent", up_params)
        .await
    {
        return crate::adapter::ActionResult::fail(format!("cdp key keyUp: {e}"));
    }
    crate::adapter::ActionResult::ok()
}

/// CDP fallback for `PlannedAction::KeyCombo`. Sends modifier keys
/// down, then the terminal key down+up, then modifier keys up. The
/// modifier-bitmask `modifiers` field lets a single Input.dispatchKeyEvent
/// represent the combined state for OS-level shortcuts (Ctrl+S, Cmd+L).
///
/// Maps the cellar modifier vocabulary to CDP's modifier bitmask:
///   Alt=1, Ctrl=2, Meta/Cmd=4, Shift=8
pub(crate) async fn dispatch_keycombo_via_cdp(
    client: &cel_cdp::CdpClient,
    keys: &[String],
) -> crate::adapter::ActionResult {
    // Partition into modifiers + the terminal (non-modifier) key.
    // Most key combos in the wild are `["Cmd", "S"]` or `["Ctrl", "L"]`
    // — modifier(s) first, then the terminal key. Reverse modifier-first
    // forms (where the terminal key precedes the modifier) are rare
    // enough we don't try to handle them: the runtime treats the LAST
    // non-modifier as the terminal.
    let mut modifier_mask: u32 = 0;
    let mut terminal: Option<&str> = None;
    for k in keys {
        match k.to_lowercase().as_str() {
            "alt" | "option" => modifier_mask |= 1,
            "ctrl" | "control" => modifier_mask |= 2,
            "cmd" | "command" | "meta" | "super" | "win" => modifier_mask |= 4,
            "shift" => modifier_mask |= 8,
            _ => terminal = Some(k.as_str()),
        }
    }
    let Some(terminal_key) = terminal else {
        return crate::adapter::ActionResult::fail(
            "key_combo: no non-modifier terminal key supplied".to_string(),
        );
    };
    let event = key_to_cdp_event(terminal_key);
    let down_params = serde_json::json!({
        "type": "keyDown",
        "key": event.key,
        "code": event.code,
        "text": event.text,
        "unmodifiedText": event.text,
        "windowsVirtualKeyCode": event.vk,
        "nativeVirtualKeyCode": event.vk,
        "modifiers": modifier_mask,
    });
    if let Err(e) = client
        .send_command("Input.dispatchKeyEvent", down_params)
        .await
    {
        return crate::adapter::ActionResult::fail(format!("cdp keycombo keyDown: {e}"));
    }
    let up_params = serde_json::json!({
        "type": "keyUp",
        "key": event.key,
        "code": event.code,
        "windowsVirtualKeyCode": event.vk,
        "nativeVirtualKeyCode": event.vk,
        "modifiers": modifier_mask,
    });
    if let Err(e) = client
        .send_command("Input.dispatchKeyEvent", up_params)
        .await
    {
        return crate::adapter::ActionResult::fail(format!("cdp keycombo keyUp: {e}"));
    }
    crate::adapter::ActionResult::ok()
}

/// CDP fallback for `PlannedAction::Type`. When `target_id` is a
/// `dom:*` element, route through the standard set_value path (which
/// already exists in `try_cdp_dispatch`). Otherwise use
/// `Input.insertText` to write into whatever element currently has
/// focus on the bound page.
///
/// `Input.insertText` is the CDP-native equivalent of "type these
/// characters into the focused element" — it generates the input/
/// change events frameworks listen for and works even when the
/// browser is headless.
pub(crate) async fn dispatch_type_via_cdp(
    client: &cel_cdp::CdpClient,
    target_id: Option<&str>,
    text: &str,
) -> crate::adapter::ActionResult {
    if let Some(tid) = target_id {
        if tid.starts_with("dom:") {
            // Reuse the set_value path's element resolver.
            let parts: Vec<&str> = tid.splitn(3, ':').collect();
            if let ["dom", role, id_part] = parts.as_slice() {
                let js = build_set_value_js(role, id_part, text);
                return match client.evaluate(&js).await {
                    Ok(v) => check_cdp_ok(v, "typed"),
                    Err(e) => {
                        crate::adapter::ActionResult::fail(format!("cdp type set_value: {e}"))
                    }
                };
            }
        }
    }
    let params = serde_json::json!({ "text": text });
    match client.send_command("Input.insertText", params).await {
        Ok(_) => crate::adapter::ActionResult::ok(),
        Err(e) => crate::adapter::ActionResult::fail(format!("cdp type insertText: {e}")),
    }
}

/// CDP key-event fields for a single cellar key-name. Mapping is
/// authoritative for the keys the planner system prompt enumerates
/// (Return, Tab, Escape, Backspace, Delete, Space, arrows, Home/End,
/// PageUp/Down, F1-F12, modifiers, single characters). Anything
/// unrecognised falls through to "treat as single-char text", which
/// covers letters, digits, punctuation, and any obscure key the
/// prompt didn't enumerate.
pub(crate) fn key_to_cdp_event(key: &str) -> CdpKeyEvent {
    let lower = key.trim().to_lowercase();
    match lower.as_str() {
        "return" | "enter" => CdpKeyEvent::named("Enter", "Enter", Some("\r"), 13),
        "tab" => CdpKeyEvent::named("Tab", "Tab", Some("\t"), 9),
        "escape" | "esc" => CdpKeyEvent::named("Escape", "Escape", None, 27),
        "backspace" => CdpKeyEvent::named("Backspace", "Backspace", None, 8),
        "delete" | "del" => CdpKeyEvent::named("Delete", "Delete", None, 46),
        "space" | " " => CdpKeyEvent::named(" ", "Space", Some(" "), 32),
        "up" | "arrowup" => CdpKeyEvent::named("ArrowUp", "ArrowUp", None, 38),
        "down" | "arrowdown" => CdpKeyEvent::named("ArrowDown", "ArrowDown", None, 40),
        "left" | "arrowleft" => CdpKeyEvent::named("ArrowLeft", "ArrowLeft", None, 37),
        "right" | "arrowright" => CdpKeyEvent::named("ArrowRight", "ArrowRight", None, 39),
        "home" => CdpKeyEvent::named("Home", "Home", None, 36),
        "end" => CdpKeyEvent::named("End", "End", None, 35),
        "pageup" => CdpKeyEvent::named("PageUp", "PageUp", None, 33),
        "pagedown" => CdpKeyEvent::named("PageDown", "PageDown", None, 34),
        // Function keys F1..F12. CDP virtual key codes: F1=112 .. F12=123.
        f if f.starts_with('f') && f[1..].parse::<u32>().is_ok() => {
            let n: u32 = f[1..].parse().unwrap();
            if (1..=12).contains(&n) {
                let key_str = Box::leak(format!("F{n}").into_boxed_str());
                CdpKeyEvent::named(key_str, key_str, None, 111 + n)
            } else {
                CdpKeyEvent::char_text(key)
            }
        }
        _ => CdpKeyEvent::char_text(key),
    }
}

/// Compact representation of a single CDP key event. Fields map 1:1
/// to `Input.dispatchKeyEvent` params (`key`, `code`, `text`,
/// `windowsVirtualKeyCode`).
pub(crate) struct CdpKeyEvent {
    pub(crate) key: String,
    pub(crate) code: String,
    /// Character to insert. `None` for non-printable keys (Escape,
    /// arrows, Home/End, function keys); `Some` for chars and the
    /// few named keys that map to text (Enter → "\r", Tab → "\t",
    /// Space → " ").
    pub(crate) text: Option<String>,
    /// `windowsVirtualKeyCode` — CDP wants the legacy Win32 VK
    /// constant for each key.
    pub(crate) vk: u32,
}

impl CdpKeyEvent {
    fn named(key: &str, code: &str, text: Option<&str>, vk: u32) -> Self {
        Self {
            key: key.to_string(),
            code: code.to_string(),
            text: text.map(str::to_string),
            vk,
        }
    }

    /// Single-character key — letters, digits, punctuation. The
    /// `code` is best-effort (`KeyA` for "a"/"A", `Digit0` for "0",
    /// etc.); browsers don't usually validate `code` for char keys
    /// so an approximate value is fine.
    fn char_text(key: &str) -> Self {
        let first = key.chars().next().unwrap_or(' ');
        let code = if first.is_ascii_alphabetic() {
            format!("Key{}", first.to_ascii_uppercase())
        } else if first.is_ascii_digit() {
            format!("Digit{}", first)
        } else {
            "Unidentified".to_string()
        };
        let vk = if first.is_ascii_alphabetic() {
            // VK for letters: A=65 ... Z=90.
            first.to_ascii_uppercase() as u32
        } else if first.is_ascii_digit() {
            // VK for digits: 0=48 ... 9=57.
            first as u32
        } else {
            0
        };
        Self {
            key: key.to_string(),
            code,
            text: Some(key.to_string()),
            vk,
        }
    }
}

pub(crate) fn build_set_value_js(role: &str, id_part: &str, value: &str) -> String {
    let role_js = serde_json::to_string(role).unwrap_or_else(|_| "\"input\"".into());
    let id_js = serde_json::to_string(id_part).unwrap_or_else(|_| "\"\"".into());
    let value_js = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
            const role = {role_js};
            const idPart = {id_js};
            const value = {value_js};
            const tagFor = (r) => ({{
                input: ['input:not([type="submit"]):not([type="button"])', 'textarea'],
                textarea: ['textarea'],
                select: ['select'],
                searchfield: ['input[type="search"]', 'input[type="text"]'],
            }})[r] || ['input', 'textarea', 'select'];
            const sels = tagFor(role).join(',');
            const candidates = Array.from(document.querySelectorAll(sels))
                .filter(el => el.offsetParent !== null);
            let target = null;
            // 0. EXACT match via `data-cel-tag` — see build_click_js for
            //    rationale. The CDP walker stamps this attribute during
            //    perception; pick_id_part emits `t<n>` when no HTML id
            //    is available; dispatch resolves the exact node here.
            if (typeof idPart === 'string' && /^t\d+$/.test(idPart)) {{
                const n = idPart.slice(1);
                const byTag = document.querySelector('[data-cel-tag="' + n + '"]');
                if (byTag) {{ target = byTag; }}
            }}
            // 1. Exact HTML id match — fast path for `dom:input:email`
            //    style ids the planner emits after PR #66's prompt
            //    update. Restricted to safe id chars so an injected
            //    idPart can't reach a CSS-injection sink.
            if (!target && typeof idPart === 'string' && /^[A-Za-z][\w:.-]*$/.test(idPart)) {{
                const byId = document.getElementById(idPart);
                if (byId && candidates.includes(byId)) {{
                    target = byId;
                }}
            }}
            // 2. Numeric → index into visible candidates (back-compat).
            if (!target) {{
                const asNum = parseInt(idPart, 10);
                if (!isNaN(asNum) && String(asNum) === idPart) {{
                    target = candidates[asNum] || null;
                }}
            }}
            // 3. Substring search over CONCATENATION of identifying
            //    attributes. Was: first-truthy-wins which always picked
            //    `placeholder` for inputs, hiding `name`/`id`. So
            //    `set_value dom:input:name` would search "John Doe" for
            //    the string "name" — never matching the input that did
            //    have `name="name"`. The concat surfaces every signal.
            if (!target) {{
                const normalize = s => String(s || '')
                    .toLowerCase()
                    .replace(/[^a-z0-9]+/g, '-')
                    .replace(/^-+|-+$/g, '');
                const needle = String(idPart).toLowerCase();
                const needleNorm = normalize(idPart);
                target = candidates.find(el => {{
                    const parts = [
                        el.id,
                        el.name,
                        el.placeholder,
                        el.value,
                        el.getAttribute('aria-label'),
                        // `data-testid` parity with build_click_js —
                        // when perception's pick_id_part used testid
                        // (input has no `id`/`name`), set_value must
                        // be able to find it the same way.
                        el.getAttribute('data-testid'),
                        el.getAttribute('data-test'),
                        el.getAttribute('data-cy'),
                        el.getAttribute('data-qa'),
                    ];
                    const concat = parts.filter(Boolean).join(' ');
                    const t = concat.toLowerCase();
                    if (t.includes(needle)) return true;
                    // Slug-aware fallback (parity with build_click_js):
                    // perception slugifies labels ("Search query" →
                    // "search-query") but raw attrs have spaces, so a
                    // literal substring miss. Normalise + compare.
                    if (needleNorm.length >= 3) {{
                        const tNorm = normalize(concat);
                        if (tNorm.includes(needleNorm)) return true;
                    }}
                    return false;
                }}) || null;
            }}
            if (!target) return 'no-match:' + role + ':' + idPart;
            target.focus();

            const dispatchValueEvent = (el, type) => {{
                const init = {{
                    bubbles: true,
                    cancelable: type === 'beforeinput',
                    composed: true,
                    inputType: 'insertReplacementText',
                    data: String(value),
                }};
                try {{
                    if (typeof InputEvent === 'function' && (type === 'beforeinput' || type === 'input')) {{
                        return el.dispatchEvent(new InputEvent(type, init));
                    }}
                }} catch (_) {{}}
                return el.dispatchEvent(new Event(type, {{
                    bubbles: true,
                    cancelable: type === 'beforeinput',
                    composed: true,
                }}));
            }};

            const setNativeValue = (el, next) => {{
                if (el.isContentEditable || !('value' in el)) {{
                    el.textContent = String(next);
                    return;
                }}
                const tag = (el.tagName || '').toUpperCase();
                const proto = tag === 'TEXTAREA'
                    ? HTMLTextAreaElement.prototype
                    : tag === 'SELECT'
                        ? HTMLSelectElement.prototype
                        : HTMLInputElement.prototype;
                const ownSetter = Object.getOwnPropertyDescriptor(el, 'value')?.set;
                const protoSetter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
                    || Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value')?.set;
                if (protoSetter && ownSetter !== protoSetter) protoSetter.call(el, next);
                else if (ownSetter) ownSetter.call(el, next);
                else el.value = next;
            }};

            const commitValue = (el, next) => {{
                const proceed = dispatchValueEvent(el, 'beforeinput');
                if (!proceed) return false;
                setNativeValue(el, next);
                dispatchValueEvent(el, 'input');
                el.dispatchEvent(new Event('change', {{ bubbles: true, composed: true }}));
                return true;
            }};

            // <select> elements: HTML spec says `el.value = X` silently fails
            // unless X matches an option's `value` attribute exactly. The
            // planner is often handed the display text ("Technical Support")
            // rather than the option value ("support"), so we resolve against
            // both. Case-insensitive, trim-normalized — tolerant of whitespace
            // drift. Return 'no-option' so the caller sees a distinct signal
            // when the value didn't match any option (vs 'no-match' for
            // missing element).
            if (target.tagName === 'SELECT') {{
                const needle = String(value).trim().toLowerCase();
                const opts = Array.from(target.options);
                let picked = opts.find((o) => o.value === value);
                if (!picked) picked = opts.find((o) => (o.value || '').trim().toLowerCase() === needle);
                if (!picked) picked = opts.find((o) => (o.textContent || '').trim().toLowerCase() === needle);
                if (!picked) picked = opts.find((o) => (o.textContent || '').trim().toLowerCase().includes(needle));
                if (!picked) return 'no-option:' + role + ':' + idPart + ':' + String(value).slice(0, 40);
                setNativeValue(target, picked.value);
                dispatchValueEvent(target, 'input');
                target.dispatchEvent(new Event('change', {{ bubbles: true, composed: true }}));
                return 'ok:select:' + (picked.value || '').slice(0, 60);
            }}

            // React/Vue/Svelte sometimes track value in their own state;
            // firing beforeinput + input + change is the browser-like commit
            // path that keeps filtered lists and framework state in sync.
            if (!commitValue(target, value)) return 'canceled:beforeinput';
            return 'ok:set:' + ((target.value || target.textContent || '')).slice(0, 60);
        }})()"#
    )
}

/// Browser app names recognized for CDP-aware activation.
pub(crate) const CDP_BROWSERS: &[&str] = &[
    "chrome", "chromium", "brave", "edge", "opera", "vivaldi", "arc",
];

/// Injected as a prelude to every PlannedAction::CdpEval. Patches two
/// gotchas that LLMs routinely hit when driving browser forms through JS:
///
/// 1. `<select>.value = X` silently no-ops when X isn't an option's `value`
///    attribute. Planners frequently supply display text ("Technical
///    Support") rather than the underlying value ("support"). We resolve
///    both and set the canonical value.
///
/// 2. `<form>.submit()` programmatic submit BYPASSES the submit event,
///    meaning preventDefault-based handlers (which show success UI, run
///    client-side validation, etc.) never fire. We wrap it so the submit
///    event dispatches first — if nothing prevents it, the native submit
///    still runs as before. Net effect: `form.submit()` now behaves like
///    clicking the submit button for handler-firing purposes.
///
/// Both patches are idempotent (guard flag) and auto-reinstall after
/// navigation (globals evaporate with the page). Patches are best-effort
/// — a failure in Object.defineProperty etc. silently falls through to
/// native behavior rather than blocking the caller's eval.
/// Default `timeout_ms` for `PlannedAction::Navigate` when the caller
/// doesn't supply one. Matches Playwright's `page.goto` default and the
/// historical TS adapter behaviour. Bumping this changes the upper
/// bound for the readyState poll in `dispatch_navigate`.
pub(crate) const NAVIGATE_DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Polling interval for the lifecycle wait inside the in-cortex CDP
/// fallback. Page.navigate fires acknowledgement before the page has
/// actually loaded, and `cel_cdp` has no event-stream subscription
/// surface, so we poll `document.readyState`. 100ms keeps the loop
/// responsive without flooding Runtime.evaluate.
pub(crate) const NAVIGATE_POLL_MS: u64 = 100;

/// Best-effort cookie-banner / overlay dismiss script. Runs after the
/// navigate's lifecycle wait when the caller didn't opt out via
/// `dismiss_overlays: false`. The TS browser adapter has its own
/// (richer) dismiss path inside `process-driver.ts`, so this only
/// fires on the in-cortex fallback path.
///
/// Heuristics, in order:
///   1. Buttons whose visible text contains accept/agree/got it/ok/
///      dismiss — short list, English-leaning, conservative.
///   2. Buttons with common consent IDs / aria-labels.
///
/// Failure is silent — a botched dismiss should never fail the navigate.
pub(crate) const CEL_DISMISS_OVERLAYS_JS: &str = r#"(() => {
    try {
        const KEYWORDS = [
            'accept all', 'accept cookies', 'accept', 'agree',
            'got it', 'ok', 'dismiss', 'allow all', 'i agree',
        ];
        const SELECTOR_HINTS = [
            '#onetrust-accept-btn-handler',
            '#truste-consent-button',
            'button[aria-label*="accept" i]',
            'button[aria-label*="agree" i]',
            'button[id*="cookie" i][id*="accept" i]',
            'button[class*="cookie" i][class*="accept" i]',
        ];
        for (const sel of SELECTOR_HINTS) {
            const el = document.querySelector(sel);
            if (el && typeof el.click === 'function') {
                el.click();
                return 'dismissed:selector';
            }
        }
        const buttons = document.querySelectorAll('button, [role="button"], a');
        for (const btn of buttons) {
            const text = (btn.textContent || '').trim().toLowerCase();
            if (!text || text.length > 40) continue;
            for (const kw of KEYWORDS) {
                if (text === kw || text.startsWith(kw + ' ') || text.endsWith(' ' + kw)) {
                    btn.click();
                    return 'dismissed:text';
                }
            }
        }
    } catch (e) {}
    return 'no-overlay';
})()"#;

/// Body of the page-snapshot computation used by `EffectExpectation::
/// DomChanged`. Defines a local `after` variable that captures three
/// cheap-to-compute signals:
///
///   • `t` — `document.body.innerText.length`. A coarse but reliable
///     proxy for "did visible text change?" (modal opening, success
///     message appearing, row removed).
///   • `c` — count of interactive elements
///     (`a, button, input, select, textarea`). Catches "tab switch
///     swapped the entire panel" / "delete row dropped the action
///     button" / "submit button vanished after form submit" cases.
///   • `u` — `location.href`. Catches submit→thank-you-page
///     navigations the action triggered.
///
/// The shape is serialised as JSON so the after vs before comparison
/// is a single string-inequality check. Tight enough that the
/// snapshot-and-compare round-trip is ~5 ms; cheap enough to call
/// inside the 100ms poll cadence.
///
/// Volatile content (timestamp tickers, animated counters, live
/// feeds) will cause false-positive diffs on the order of the page's
/// natural update rate. The 2s default timeout is short enough that
/// most ticker-driven changes don't matter much in practice; pages
/// where they do should prefer a selector-based expectation if they
/// can name one. Documented in the `DomChanged` rustdoc on
/// `EffectExpectation`.
/// Snapshot fingerprint fields:
/// - `t`: body innerText length (catches "Form submitted!" appearing,
///   counter ticking, banner removing its label, etc.)
/// - `c`: total interactive element count (catches new buttons /
///   inputs being added / removed from the DOM tree)
/// - `v`: VISIBLE interactive count (catches modal show/hide where
///   the elements were already in the DOM but `display:none` /
///   `offsetParent === null`. Run-6 evidence: cookie-consent and
///   re-auth overlays toggle visibility without changing total count.)
/// - `d`: disabled-or-aria-disabled interactive count (catches the
///   common "Send → Sending… → Sent ✓" pattern where the button
///   stays in DOM but flips disabled; and the inverse for forms
///   re-enabling after re-authentication.)
/// - `s`: state-attribute hash on dialog/aria-hidden/aria-expanded
///   elements (catches overlay reveal/dismiss where the aria flag
///   flips but nothing else does, e.g. an outstanding-balance modal
///   that lives in the DOM and toggles `aria-hidden`.)
/// - `u`: location URL (catches SPA navigations that don't otherwise
///   change innerText length, plus full-page nav.)
///
/// All fields stringified together into a single JSON blob so the
/// before/after comparison is a single string-inequality check.
///
/// Volatile content (timestamp tickers, animated counters) will
/// cause false-positive diffs on `t`. The 2s default timeout keeps
/// the blast radius small; pages where this matters should prefer a
/// selector-based expectation if they can name one. Documented in
/// the `DomChanged` rustdoc on `EffectExpectation`.
pub(crate) const DOM_SNAPSHOT_BODY_JS: &str = r#"
    const __cel_interactive_sel = 'a, button, input, select, textarea';
    const __cel_all = document.querySelectorAll(__cel_interactive_sel);
    let __cel_visible = 0;
    for (let __i = 0; __i < __cel_all.length; __i++) {
        if (__cel_all[__i].offsetParent !== null) __cel_visible++;
    }
    const __cel_disabled = document.querySelectorAll(
        'button[disabled], input[disabled], select[disabled], textarea[disabled], [aria-disabled="true"]'
    ).length;
    const __cel_state_nodes = document.querySelectorAll(
        '[aria-hidden], [aria-expanded], [aria-busy], [role="dialog"], [role="alert"], [role="alertdialog"]'
    );
    let __cel_state = '';
    for (let __j = 0; __j < __cel_state_nodes.length; __j++) {
        const __n = __cel_state_nodes[__j];
        __cel_state += (__n.getAttribute('aria-hidden') || '') + ',';
        __cel_state += (__n.getAttribute('aria-expanded') || '') + ',';
        __cel_state += (__n.getAttribute('aria-busy') || '') + '|';
    }
    const after = JSON.stringify({
        t: (document.body && document.body.innerText || '').length,
        c: __cel_all.length,
        v: __cel_visible,
        d: __cel_disabled,
        s: __cel_state,
        u: location.href,
    });
"#;

/// Full snapshot expression that returns the JSON string. Used at
/// pre-dispatch baseline capture in `try_cdp_dispatch`. The polled
/// predicate (built inside `wait_for_effect`) inlines
/// `DOM_SNAPSHOT_BODY_JS` and adds the comparison against the
/// captured baseline.
pub(crate) const DOM_SNAPSHOT_JS: &str = r#"(() => {
    const __cel_interactive_sel = 'a, button, input, select, textarea';
    const __cel_all = document.querySelectorAll(__cel_interactive_sel);
    let __cel_visible = 0;
    for (let __i = 0; __i < __cel_all.length; __i++) {
        if (__cel_all[__i].offsetParent !== null) __cel_visible++;
    }
    const __cel_disabled = document.querySelectorAll(
        'button[disabled], input[disabled], select[disabled], textarea[disabled], [aria-disabled="true"]'
    ).length;
    const __cel_state_nodes = document.querySelectorAll(
        '[aria-hidden], [aria-expanded], [aria-busy], [role="dialog"], [role="alert"], [role="alertdialog"]'
    );
    let __cel_state = '';
    for (let __j = 0; __j < __cel_state_nodes.length; __j++) {
        const __n = __cel_state_nodes[__j];
        __cel_state += (__n.getAttribute('aria-hidden') || '') + ',';
        __cel_state += (__n.getAttribute('aria-expanded') || '') + ',';
        __cel_state += (__n.getAttribute('aria-busy') || '') + '|';
    }
    const after = JSON.stringify({
        t: (document.body && document.body.innerText || '').length,
        c: __cel_all.length,
        v: __cel_visible,
        d: __cel_disabled,
        s: __cel_state,
        u: location.href,
    });
    return after;
})()"#;

pub(crate) const CEL_SELECT_PATCH_PRELUDE: &str = r#"(() => {
    if (window.__celSelectPatched) return;
    try {
        // ── 1. Patch <select>.value for display-text assignments. ──
        const selProto = HTMLSelectElement.prototype;
        const desc = Object.getOwnPropertyDescriptor(selProto, 'value');
        if (desc && desc.set) {
            const originalSet = desc.set;
            const originalGet = desc.get;
            Object.defineProperty(selProto, 'value', {
                configurable: true,
                enumerable: desc.enumerable,
                get() { return originalGet.call(this); },
                set(v) {
                    const opts = Array.from(this.options);
                    if (opts.some((o) => o.value === v)) {
                        originalSet.call(this, v);
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                        return;
                    }
                    const needle = String(v).trim().toLowerCase();
                    const match = opts.find((o) => (o.value || '').trim().toLowerCase() === needle)
                        || opts.find((o) => (o.textContent || '').trim().toLowerCase() === needle)
                        || opts.find((o) => (o.textContent || '').trim().toLowerCase().includes(needle));
                    if (match) {
                        originalSet.call(this, match.value);
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                    } else {
                        originalSet.call(this, v);
                    }
                },
            });
        }
    } catch (e) {}

    try {
        // ── 2. Patch <form>.submit() so it fires a submit event first. ──
        // HTMLFormElement.submit() by spec BYPASSES the submit event,
        // which means preventDefault handlers that show success UI
        // never run. Wrap it: dispatch a cancelable submit event, then
        // fall through to native submit only if not prevented. Pages
        // that use form.submit() without handlers behave identically;
        // pages that have handlers now respect them.
        const formProto = HTMLFormElement.prototype;
        const originalSubmit = formProto.submit;
        formProto.submit = function () {
            const ev = new Event('submit', { bubbles: true, cancelable: true });
            const proceeded = this.dispatchEvent(ev);
            if (proceeded && !ev.defaultPrevented) {
                originalSubmit.call(this);
            }
        };
    } catch (e) {}

    window.__celSelectPatched = true;
})();"#;

impl Cortex {
    /// Mutate-style CDP binding for use during construction — before the cortex
    /// is shared behind an `Arc` and booted (`&mut self` is unobtainable
    /// afterward, and no adapters are registered yet). Sets the slot only; it
    /// does NOT propagate to adapters. The runtime, propagating mutator is
    /// `bind_cdp_client`.
    pub fn set_cdp_client(&mut self, client: Arc<cel_cdp::CdpClient>) {
        *self.cdp_client.lock().unwrap() = Some(client);
    }

    /// THE single runtime entry point for binding a CDP client: sets the
    /// cortex's own slot AND propagates the same client to every registered
    /// adapter (via the adapter `set_cdp_client` hook), so perception (the
    /// in-process browser adapter) and dispatch (the cortex) always ride one
    /// connection — there is no "which client is bound?" divergence at runtime.
    /// `bind_browser_cdp_url` (URL → connect → here), the per-action
    /// `cdp_client_or_ambient` fallback, and the tick-loop ambient auto-bind all
    /// funnel through the shared `install_cdp_client` primitive this delegates
    /// to.
    ///
    /// The sync builders `with_cdp_client` / `set_cdp_client` deliberately do
    /// NOT route through here: they run at construction time, when no adapters
    /// exist to propagate to and `self` is owned exclusively. Once booted,
    /// `self` is shared immutably, so this async method (and the tick loop) is
    /// the only reachable writer of the slot.
    pub async fn bind_cdp_client(&self, client: Arc<cel_cdp::CdpClient>) {
        install_cdp_client(&self.cdp_client, &self.adapters, client).await;
    }

    /// Return the bound CDP client, or — if none is bound — perform a single
    /// ambient `connect_to_focused_app()` discovery, bind it through
    /// `bind_cdp_client` (so the adapter sees it and later calls reuse the same
    /// socket), and return it.
    ///
    /// This is the ONE place the per-action CDP paths resolve a client when the
    /// slot is empty. Before it existed, the six fallback call sites each opened
    /// their own throwaway `connect_to_focused_app()` connection — churning
    /// Chrome's WebSocket table and leaving the cortex slot unbound, so the next
    /// action re-discovered from scratch. Binding the discovered client turns
    /// the second and subsequent calls into cheap slot reads.
    pub(crate) async fn cdp_client_or_ambient(&self) -> Option<Arc<cel_cdp::CdpClient>> {
        // Read + drop the std-Mutex guard in one statement so it never spans the
        // discovery `.await` below.
        let existing = self.cdp_client.lock().unwrap().clone();
        if let Some(client) = existing {
            return Some(client);
        }
        let client = Arc::new(cel_cdp::connect_to_focused_app().await?);
        self.bind_cdp_client(client.clone()).await;
        Some(client)
    }

    /// Connect to a CDP URL and bind the resulting client to the registered
    /// browser adapter. Used by Phase 3 of ADR-unify-browser-ownership:
    /// `cel.ensureBrowser` spawns a Chromium with `--remote-debugging-port`
    /// and then calls this so the cortex's BrowserAdapter has a usable
    /// client without going through `cel_cdp::connect_to_focused_app`
    /// discovery (which can fail for headless browsers).
    ///
    /// Iterates all registered adapters and calls `set_cdp_client` on each.
    /// The default trait impl is no-op so only adapters that actually
    /// consume a CDP client (today: browser-rs) react. Multiple browser
    /// adapters with different runtimes (in-process vs process) would all
    /// receive the same client, which matches today's "one shared
    /// dedicated CDP browser" model.
    ///
    /// The caller typically passes the browser-level endpoint Chromium
    /// announces on startup (`ws://127.0.0.1:PORT/devtools/browser/UUID`).
    /// That endpoint only supports browser-level CDP commands, not the
    /// page-level `Runtime.evaluate` that `BrowserAdapter::probe()` uses
    /// for liveness checks. This function resolves a page-level WebSocket
    /// URL from the HTTP `/json/list` endpoint on the same port before
    /// connecting, falling back to the original URL if no page target is
    /// found yet (e.g. during a very early boot race).
    ///
    /// **Page-level URLs are honored directly**: if the caller passes a
    /// URL containing `/devtools/page/`, this function skips the discovery
    /// step and connects to that exact target. This is the path used by
    /// the bench runner to bind the cortex to the SAME page the TS
    /// BrowserAdapter created (via Playwright's `connectOverCDP` +
    /// `isolatedContext: true` newPage); without honoring page-level URLs
    /// the discovery loop always picked the FIRST target (Chromium's
    /// initial about:blank), so the cortex acted on page A while the
    /// adapter perceived + screenshotted page B. Surfaced as ArXiv /
    /// Apple / BBC WebVoyager FAILs on 2026-05-26 where the agent
    /// successfully completed the task on the cortex's page but the
    /// adapter's separate page (still on the start URL) is what the
    /// GPT-4V evaluator saw.
    pub async fn bind_browser_cdp_url(&self, url: &str) -> Result<(), cel_cdp::CdpError> {
        // Fast path: caller passed a page-level URL (e.g. the WS URL of a
        // Playwright-created page). Connect directly — no discovery — so
        // the cortex binds to the EXACT target the caller intended.
        let is_page_url = url.contains("/devtools/page/");

        // Parse the port from ws://127.0.0.1:PORT/devtools/browser/UUID.
        let port: Option<u16> = url
            .trim_start_matches("ws://")
            .trim_start_matches("wss://")
            .split_once(':')
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once('/').map(|(s, _)| s).or(Some(rest)))
            .and_then(|s| s.parse().ok());

        // Resolve a page-level WebSocket URL from the HTTP /json/list endpoint.
        // list_http_targets uses blocking I/O, so run it on the thread pool.
        // Retry briefly: Chrome may not have created its initial page target
        // the instant it announces the DevTools endpoint on stderr.
        // SKIP when the caller already passed a page-level URL — that means
        // they want THIS specific target, not whatever happens to be first.
        let page_ws_url: Option<String> = if is_page_url {
            None
        } else if let Some(p) = port {
            tokio::task::spawn_blocking(move || {
                for attempt in 0..8u8 {
                    let targets = cel_cdp::list_http_targets(p);
                    if let Some(t) = targets.into_iter().next() {
                        return Some(t.ws_url);
                    }
                    if attempt < 7 {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                }
                None
            })
            .await
            .unwrap_or(None)
        } else {
            None
        };

        let connect_url = page_ws_url.as_deref().unwrap_or(url);
        tracing::debug!(
            browser_url = %url,
            connect_url = %connect_url,
            is_page_url = is_page_url,
            "bind_browser_cdp_url: resolved page-level target"
        );

        let client = std::sync::Arc::new(cel_cdp::CdpClient::connect(connect_url).await?);
        // Funnel through the single runtime mutator: sets the cortex's own slot
        // (so has_cdp_client() / cdp_eval / navigate / screenshot use this
        // connection) AND propagates to every registered adapter, so the adapter
        // perceives the exact target the cortex drives.
        self.bind_cdp_client(client).await;
        Ok(())
    }

    /// Is a CDP client bound? Used by the canonical runner to tell
    /// the planner whether `cdp_eval` / `navigate` will actually
    /// dispatch somewhere vs be blind.
    pub fn has_cdp_client(&self) -> bool {
        self.cdp_client.lock().unwrap().is_some()
    }

    /// Shareable handle to the bound CDP client slot. Used by external
    /// closures (e.g. the merger's CDP-screenshot vision fallback) that
    /// need to read the currently-bound client at call time rather than
    /// capture-time, since `bind_browser_cdp_url` may be called AFTER
    /// the merger is built. The returned Arc shares the same underlying
    /// Mutex as the cortex's own `self.cdp_client`, so any later bind
    /// is visible to holders.
    pub fn cdp_client_handle(&self) -> Arc<std::sync::Mutex<Option<Arc<cel_cdp::CdpClient>>>> {
        Arc::clone(&self.cdp_client)
    }

    /// Fetch the current URL of the CDP-bound page (if any). Used by
    /// the canonical runner to tell the planner whether it's already
    /// on the right page before emitting `navigate`. Returns None
    /// when there is no CDP client, the client is unreachable, or
    /// the bound page is not a URL page (about:blank, devtools, …).
    pub async fn cdp_current_url(&self) -> Option<String> {
        let cdp = self.cdp_client.lock().unwrap().clone();
        let client = cdp?;
        client.get_url().await.ok()
    }

    /// Capture a JPEG screenshot of the CDP-bound page when one is
    /// wired. Returns `None` if there is no CDP client, the call fails,
    /// or the response is malformed — callers should fall back to a
    /// macOS display capture in that case so screenshot capability
    /// degrades rather than disappears.
    ///
    /// This is the path that lets headless-Chrome eval scenarios photograph
    /// the rendered page rather than whatever macOS window happens to be
    /// in front (which is usually an editor or terminal during background
    /// runs).
    pub async fn cdp_screenshot(&self) -> Option<Vec<u8>> {
        let cdp = self.cdp_client.lock().unwrap().clone();
        let client = cdp?;
        client.capture_screenshot().await.ok()
    }

    /// Execute JavaScript in the CDP-bound page and return the JSON-encoded
    /// result. Delegates to `cdp_eval_via_shared_or_focused` so the call
    /// uses the explicitly-bound client (set by `bind_browser_cdp_url`)
    /// rather than ambient `connect_to_focused_app()` discovery.
    ///
    /// This is the correct path for NAPI callers — it ensures the eval
    /// lands on the right Chrome tab even when other tabs are open.
    pub async fn cdp_evaluate(&self, expression: &str) -> Result<String, String> {
        self.cdp_eval_via_shared_or_focused(expression).await
    }

    /// Navigate the CDP-bound page to `url`. Prefers the explicitly-bound
    /// client (set by `bind_browser_cdp_url`) over ambient
    /// `connect_to_focused_app()` discovery, ensuring the navigation lands
    /// on the same Chrome tab that all other cortex operations use.
    ///
    /// Returns `Ok(())` on success or an error string on failure.
    pub async fn cdp_navigate_page(&self, url: &str) -> Result<(), String> {
        match self.cdp_client_or_ambient().await {
            Some(client) => client
                .navigate_resilient(url)
                .await
                .map_err(|e| format!("CDP navigate failed: {e}")),
            None => Err("No CDP target available".into()),
        }
    }

    /// Extract page content from the CDP-bound tab. Prefers the
    /// explicitly-bound client over ambient `connect_to_focused_app()`
    /// discovery so page extraction always targets the correct Chrome tab.
    pub async fn cdp_page_content(&self) -> Option<cel_cdp::PageContent> {
        let client = self.cdp_client_or_ambient().await?;
        cel_cdp::extract_page_content(&client).await.ok()
    }

    /// Run a CDP `Runtime.evaluate` and return the result as a
    /// JSON-encoded string (the shape the caller's `data` field
    /// expects). Prefers the SHARED `self.cdp_client` so the agent's
    /// many `cdp_eval` / `navigate` / `extract_with_fallback` calls
    /// reuse one WebSocket instead of opening a fresh one per
    /// action — which is what was exhausting Chrome's connection
    /// table mid-eval.
    ///
    /// Routes through `cdp_client_or_ambient`: the explicitly-bound client (via
    /// `evaluate_resilient`, which auto-reconnects once if Chrome dropped the
    /// WebSocket) when present, otherwise a single ambient discovery that is
    /// bound for reuse so subsequent calls don't re-discover.
    pub(crate) async fn cdp_eval_via_shared_or_focused(
        &self,
        expression: &str,
    ) -> Result<String, String> {
        match self.cdp_client_or_ambient().await {
            Some(client) => match client.evaluate_resilient(expression).await {
                Ok(result) => Ok(serde_json::to_string(&result).unwrap_or_default()),
                Err(e) => Err(format!("CDP eval failed: {e}")),
            },
            None => Err("No CDP target available".into()),
        }
    }
}
