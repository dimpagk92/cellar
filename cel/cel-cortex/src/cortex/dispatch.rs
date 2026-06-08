//! Action execution — `Cortex::execute` and the per-kind dispatch helpers.
//!
//! Routes a `PlannedAction` to the right backend (registered adapter, browser
//! DOM via CDP, Numbers cell read/write, or native input), then waits for the
//! declared `EffectExpectation`. Native input is gated behind the
//! `allow_native_input` safety flag; browser actions prefer the CDP path.

use super::cdp::{
    build_extract_expression, cdp_value_to_string, check_cdp_ok, dispatch_key_via_cdp,
    dispatch_keycombo_via_cdp, dispatch_type_via_cdp, parse_extracted, try_cdp_dispatch,
    CEL_DISMISS_OVERLAYS_JS, CEL_SELECT_PATCH_PRELUDE, DOM_SNAPSHOT_BODY_JS,
    NAVIGATE_DEFAULT_TIMEOUT_MS, NAVIGATE_POLL_MS,
};
use super::focus::{
    activate_app_with_verification, resolve_ax_by_label, try_ax_action, try_set_value,
};
use super::numbers::{
    bootstrap_numbers_document, dismiss_numbers_dialog_if_present,
    should_attempt_numbers_document_bootstrap,
};
use super::targets::{bounds_center, extract_navigation_url, find_element};
use super::*;

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// The optional `expect_after` field from any [`PlannedAction`] that
/// supports it. None for actions without the field (Wait, Scroll, etc.)
/// or when the planner left it unset.
pub(crate) fn action_expect_after(action: &PlannedAction) -> Option<&EffectExpectation> {
    match action {
        PlannedAction::Click { expect_after, .. }
        | PlannedAction::SetValue { expect_after, .. }
        | PlannedAction::AxAction { expect_after, .. } => expect_after.as_ref(),
        _ => None,
    }
}

/// Poll the page via CDP until the [`EffectExpectation`] holds or the
/// expectation's `timeout_ms` fires. Returns `Ok(())` when satisfied;
/// `Err(reason)` when timed out — the reason names the expectation
/// shape and the last observed state so the planner sees exactly what
/// we looked for and what was there instead.
///
/// Poll cadence is 100 ms — fast enough to feel snappy (under one
/// CDP round-trip on a busy page), slow enough not to pin the event
/// loop. Most expectations resolve on the first poll when the action
/// actually fired.
pub(crate) async fn wait_for_effect(
    client: &cel_cdp::CdpClient,
    expectation: &EffectExpectation,
    before_snapshot: Option<&str>,
) -> Result<(), String> {
    let (predicate_js, timeout_ms, label) = match expectation {
        EffectExpectation::SelectorAppears {
            selector,
            timeout_ms,
        } => (
            format!(
                r#"(() => {{
                    const el = document.querySelector({sel});
                    return !!el && el.offsetParent !== null;
                }})()"#,
                sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into()),
            ),
            *timeout_ms,
            format!("selector_appears(\"{}\")", selector),
        ),
        EffectExpectation::SelectorDisappears {
            selector,
            timeout_ms,
        } => (
            format!(
                r#"(() => {{
                    const el = document.querySelector({sel});
                    return !el || el.offsetParent === null;
                }})()"#,
                sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into()),
            ),
            *timeout_ms,
            format!("selector_disappears(\"{}\")", selector),
        ),
        EffectExpectation::SelectorTextContains {
            selector,
            substring,
            timeout_ms,
        } => (
            format!(
                r#"(() => {{
                    const el = document.querySelector({sel});
                    if (!el) return false;
                    return (el.textContent || "").includes({sub});
                }})()"#,
                sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into()),
                sub = serde_json::to_string(substring).unwrap_or_else(|_| "\"\"".into()),
            ),
            *timeout_ms,
            format!(
                "selector_text_contains(\"{}\", \"{}\")",
                selector, substring
            ),
        ),
        EffectExpectation::DomChanged { timeout_ms } => {
            // Compare the post-dispatch snapshot to the baseline we
            // captured BEFORE the dispatch. Returns true when any
            // field differs (text length, interactive element count,
            // or URL). If we couldn't get a baseline (pre-dispatch
            // snapshot failed — see try_cdp_dispatch's None branch),
            // degrade gracefully: treat ANY post-dispatch snapshot
            // success as the effect, since "we got past dispatch
            // without CDP exploding" is at least weak evidence of a
            // healthy page.
            let baseline = before_snapshot.unwrap_or("");
            let baseline_js = serde_json::to_string(baseline).unwrap_or_else(|_| "\"\"".into());
            (
                format!(
                    r#"(() => {{
                        const before = {baseline_js};
                        {snapshot_body}
                        return after !== before;
                    }})()"#,
                    baseline_js = baseline_js,
                    snapshot_body = DOM_SNAPSHOT_BODY_JS,
                ),
                *timeout_ms,
                "dom_changed".to_string(),
            )
        }
    };

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let poll = std::time::Duration::from_millis(100);

    loop {
        match client.evaluate(&predicate_js).await {
            Ok(value) => {
                // `CdpClient::evaluate` returns the JS expression's
                // return value directly (already unwrapped from the
                // `{result:{value:…}}` CDP envelope), so a boolean
                // predicate arrives as `Value::Bool(_)`. The earlier
                // `value.get("result").get("value")` chain was a
                // double-unwrap that always produced `None`, so this
                // loop could NEVER observe success — every DomChanged
                // expectation timed out at `timeout_ms`. Fix: check the
                // value as-returned.
                if value == serde_json::Value::Bool(true) {
                    return Ok(());
                }
            }
            Err(e) => {
                // CDP transient (page navigated mid-poll, socket
                // hiccup): keep polling — the resilient client
                // auto-reconnects underneath.
                tracing::debug!(error = %e, "wait_for_effect: CDP eval transient");
            }
        }
        if start.elapsed() >= timeout {
            return Err(format!(
                "EffectMissing: {label} did not hold within {ms}ms — the action \
                 was dispatched (CDP returned ok) but the expected post-state \
                 never materialised. Likely causes: a validation handler called \
                 e.preventDefault(), the click landed on a remounted/stale node, \
                 or the action targeted the wrong element. Re-read perception, \
                 verify the target_id is current, and either retry with a \
                 different target or a different action.",
                label = label,
                ms = timeout_ms,
            ));
        }
        tokio::time::sleep(poll).await;
    }
}

pub(crate) fn action_dom_target(action: &PlannedAction) -> Option<&str> {
    match action {
        PlannedAction::Click { target_id, .. }
        | PlannedAction::SetValue { target_id, .. }
        | PlannedAction::AxAction { target_id, .. }
        | PlannedAction::Drag {
            from_target_id: target_id,
            ..
        } => target_id.starts_with("dom:").then_some(target_id.as_str()),
        PlannedAction::Type {
            target_id: Some(target_id),
            ..
        } => target_id.starts_with("dom:").then_some(target_id.as_str()),
        _ => None,
    }
}

/// Whether the action would (without CDP routing) dispatch through native
/// macOS input drivers (mouse, keyboard, AX, app activation). These actions
/// can affect any application the user has focused — they must NOT run
/// from eval/CI contexts unless `Cortex::with_native_input_unsafe()` was
/// explicitly opted into.
///
/// Pure data/control actions (Wait, Done, Fail, Extract, NotebookWrites,
/// CdpEval, Batch) never touch native input and always run.
fn action_requires_native_input(action: &PlannedAction) -> bool {
    match action {
        // System I/O — gated.
        PlannedAction::Click { .. }
        | PlannedAction::Type { .. }
        | PlannedAction::Key { .. }
        | PlannedAction::KeyCombo { .. }
        | PlannedAction::SetValue { .. }
        | PlannedAction::Scroll { .. }
        | PlannedAction::Drag { .. }
        | PlannedAction::AxAction { .. }
        | PlannedAction::ActivateApp { .. }
        // launch/quit start or stop the user's apps via open / osascript —
        // mutating system state, so gate them like activate_app.
        | PlannedAction::LaunchApp { .. }
        | PlannedAction::QuitApp { .. }
        | PlannedAction::Select { .. }
        | PlannedAction::Act { .. }
        | PlannedAction::Custom { .. }
        // write_cells fires osascript → system events → target app;
        // treat it like any other native-input action for gating.
        | PlannedAction::WriteCells { .. }
        // window ops mutate the user's windows via AX — gate like native input.
        | PlannedAction::Window { .. }
        // dialog ops click buttons / set fields in the user's dialogs.
        | PlannedAction::Dialog { .. }
        // dock ops act on the Dock (launch / menu / autohide).
        | PlannedAction::Dock { .. }
        // menu extras click system status items.
        | PlannedAction::MenuExtra { .. } => true,
        // Pure / control / browser-safe — always allowed.
        PlannedAction::Wait { .. }
        | PlannedAction::Done { .. }
        | PlannedAction::Fail { .. }
        | PlannedAction::Extract { .. }
        | PlannedAction::NotebookWrites { .. }
        | PlannedAction::CdpEval { .. }
        | PlannedAction::Navigate { .. }
        | PlannedAction::ReadCells { .. }
        // extract_with_fallback runs over CDP only — no native input.
        | PlannedAction::ExtractWithFallback { .. } => false,
        // Batch is a wrapper — recurse and require native input only if any
        // inner action does.
        PlannedAction::Batch { actions } => {
            actions.iter().any(action_requires_native_input)
        }
    }
}

fn action_type_str(action: &PlannedAction) -> &str {
    match action {
        PlannedAction::Click { .. } => "click",
        PlannedAction::Type { .. } => "type",
        PlannedAction::Key { .. } => "key",
        PlannedAction::KeyCombo { .. } => "key_combo",
        PlannedAction::SetValue { .. } => "set_value",
        PlannedAction::Scroll { .. } => "scroll",
        PlannedAction::Drag { .. } => "drag",
        PlannedAction::Wait { .. } => "wait",
        PlannedAction::Custom { .. } => "custom",
        PlannedAction::Extract { .. } => "extract",
        PlannedAction::Batch { .. } => "batch",
        PlannedAction::Act { .. } => "act",
        PlannedAction::Done { .. } => "done",
        PlannedAction::Fail { .. } => "fail",
        PlannedAction::AxAction { .. } => "ax_action",
        PlannedAction::ActivateApp { .. } => "activate_app",
        PlannedAction::LaunchApp { .. } => "launch_app",
        PlannedAction::QuitApp { .. } => "quit_app",
        PlannedAction::Select { .. } => "select",
        PlannedAction::CdpEval { .. } => "cdp_eval",
        PlannedAction::Navigate { .. } => "navigate",
        PlannedAction::NotebookWrites { .. } => "notebook_writes",
        PlannedAction::WriteCells { .. } => "write_cells",
        PlannedAction::ReadCells { .. } => "read_cells",
        PlannedAction::ExtractWithFallback { .. } => "extract_with_fallback",
        PlannedAction::Window { .. } => "window",
        PlannedAction::Dialog { .. } => "dialog",
        PlannedAction::Dock { .. } => "dock",
        PlannedAction::MenuExtra { .. } => "menu_extra",
    }
}

/// Translate a `PlannedAction::Window` into a `cel_accessibility::WindowOp`,
/// dispatch it, and return the window geometry read back afterward on the
/// receipt (verify-by-readback). WS2.
#[allow(clippy::too_many_arguments)]
fn dispatch_window(
    op: &str,
    app: Option<&str>,
    window_index: usize,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    preset: Option<&str>,
    display: Option<usize>,
) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    use cel_accessibility::WindowOp;

    // Tiling presets (WS2.3): resolve preset → bounds over the target display's
    // visible frame, then SetBounds.
    if let Some(preset) = preset {
        let monitors = cel_display::create_capture()
            .list_monitors()
            .unwrap_or_default();
        if monitors.is_empty() {
            return ActionResult::fail("no monitor available for window preset");
        }
        // Resolve the target monitor index: explicit `display`, else the
        // window's current display, else the primary display.
        let target_idx = match display {
            Some(i) if i < monitors.len() => i,
            Some(i) => {
                return ActionResult::fail(format!(
                    "display index {i} out of range ({} displays)",
                    monitors.len()
                ))
            }
            None => cel_accessibility::get_window_geom(app, window_index)
                .ok()
                .and_then(|g| {
                    monitor_index_for_point(&monitors, g.x + g.width / 2.0, g.y + g.height / 2.0)
                })
                .or_else(|| monitors.iter().position(|m| m.is_primary))
                .unwrap_or(0),
        };
        let mon = &monitors[target_idx];
        let Some((x, y, width, height)) = preset_bounds(preset, mon) else {
            return ActionResult::fail(format!("unknown window preset '{preset}'"));
        };
        return match cel_accessibility::perform_window_op(
            app,
            window_index,
            &WindowOp::SetBounds {
                x,
                y,
                width,
                height,
            },
        ) {
            Ok(geom) => {
                let mut result = ActionResult::ok();
                result.data = serde_json::to_value(&geom).ok();
                result
            }
            Err(e) => ActionResult::fail(format!("window preset {preset}: {e}")),
        };
    }

    let win_op = match op {
        "move" => match (x, y) {
            (Some(x), Some(y)) => WindowOp::Move { x, y },
            _ => return ActionResult::fail("window move requires x and y"),
        },
        "resize" => match (width, height) {
            (Some(width), Some(height)) => WindowOp::Resize { width, height },
            _ => return ActionResult::fail("window resize requires width and height"),
        },
        "set_bounds" => match (x, y, width, height) {
            (Some(x), Some(y), Some(width), Some(height)) => WindowOp::SetBounds {
                x,
                y,
                width,
                height,
            },
            _ => return ActionResult::fail("window set_bounds requires x, y, width, height"),
        },
        "minimize" => WindowOp::Minimize,
        "unminimize" | "restore" => WindowOp::Unminimize,
        "maximize" => WindowOp::Maximize,
        "focus" | "raise" => WindowOp::Focus,
        other => return ActionResult::fail(format!("unknown window op '{other}'")),
    };

    match cel_accessibility::perform_window_op(app, window_index, &win_op) {
        Ok(geom) => {
            let mut result = ActionResult::ok();
            result.data = serde_json::to_value(&geom).ok();
            result
        }
        Err(e) => ActionResult::fail(format!("window {op}: {e}")),
    }
}

/// Index of the monitor whose bounds contain the point `(x, y)` (global
/// points), or `None` if the point is off all displays. WS4.
fn monitor_index_for_point(monitors: &[cel_display::MonitorInfo], x: f64, y: f64) -> Option<usize> {
    monitors.iter().position(|m| {
        x >= m.x as f64
            && x < m.x as f64 + m.width as f64
            && y >= m.y as f64
            && y < m.y as f64 + m.height as f64
    })
}

/// Compute window bounds (x, y, width, height in global points) for a tiling
/// preset over a monitor's visible frame. The primary display reserves the top
/// menu-bar strip; the Dock inset is not modeled (windows may overlap it). WS2.3.
fn preset_bounds(preset: &str, mon: &cel_display::MonitorInfo) -> Option<(f64, f64, f64, f64)> {
    let menu_inset = if mon.is_primary { 24.0 } else { 0.0 };
    let mx = mon.x as f64;
    let my = mon.y as f64 + menu_inset;
    let mw = mon.width as f64;
    let mh = mon.height as f64 - menu_inset;
    let (hw, hh) = (mw / 2.0, mh / 2.0);
    let bounds = match preset {
        "left_half" | "left" => (mx, my, hw, mh),
        "right_half" | "right" => (mx + hw, my, hw, mh),
        "top_half" | "top" => (mx, my, mw, hh),
        "bottom_half" | "bottom" => (mx, my + hh, mw, hh),
        "top_left" => (mx, my, hw, hh),
        "top_right" => (mx + hw, my, hw, hh),
        "bottom_left" => (mx, my + hh, hw, hh),
        "bottom_right" => (mx + hw, my + hh, hw, hh),
        "maximize" | "full" | "fill" => (mx, my, mw, mh),
        "center" => {
            let (cw, ch) = (mw * 0.6, mh * 0.6);
            (mx + (mw - cw) / 2.0, my + (mh - ch) / 2.0, cw, ch)
        }
        _ => return None,
    };
    Some(bounds)
}

/// Drive the frontmost macOS dialog / sheet (Open/Save/Print/alert) via the
/// accessibility tree: list its controls, click a button by title, set a text
/// field, or dismiss it. Reuses the AX tree's find/act primitives — no new FFI.
/// WS5.
fn dispatch_dialog(
    op: &str,
    button: Option<&str>,
    value: Option<&str>,
    field_index: usize,
) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    use cel_accessibility::ElementRole;

    let tree = cel_accessibility::create_tree();

    match op {
        "list" => {
            let buttons = tree
                .find_elements(Some(&ElementRole::Button), None)
                .unwrap_or_default();
            let fields = tree
                .find_elements(Some(&ElementRole::Input), None)
                .unwrap_or_default();
            let button_titles: Vec<String> = buttons
                .iter()
                .filter(|e| e.state.visible)
                .filter_map(|e| e.label.clone())
                .collect();
            let field_values: Vec<String> = fields
                .iter()
                .filter(|e| e.state.visible)
                .map(|e| e.label.clone().unwrap_or_default())
                .collect();
            let mut result = ActionResult::ok();
            result.data = Some(serde_json::json!({
                "buttons": button_titles,
                "fields": field_values,
            }));
            result
        }
        "click" => {
            let Some(button) = button else {
                return ActionResult::fail("dialog click requires a button title");
            };
            let matches = tree
                .find_elements(Some(&ElementRole::Button), Some(button))
                .unwrap_or_default();
            let Some(el) = matches.into_iter().find(|e| e.state.visible) else {
                return ActionResult::fail(format!("dialog button '{button}' not found"));
            };
            match tree.perform_action(&el.id, "click") {
                Ok(true) => ActionResult::ok(),
                _ => ActionResult::fail(format!("failed to click dialog button '{button}'")),
            }
        }
        "set_field" => {
            let Some(value) = value else {
                return ActionResult::fail("dialog set_field requires a value");
            };
            let fields = tree
                .find_elements(Some(&ElementRole::Input), None)
                .unwrap_or_default();
            let visible: Vec<_> = fields.into_iter().filter(|e| e.state.visible).collect();
            let Some(el) = visible.get(field_index) else {
                return ActionResult::fail(format!(
                    "dialog field index {field_index} out of range ({} fields)",
                    visible.len()
                ));
            };
            match tree.set_value(&el.id, value) {
                Ok(true) => ActionResult::ok(),
                _ => ActionResult::fail("failed to set dialog field"),
            }
        }
        "dismiss" => {
            for title in ["Cancel", "Don't Save", "Close"] {
                let matches = tree
                    .find_elements(Some(&ElementRole::Button), Some(title))
                    .unwrap_or_default();
                if let Some(el) = matches.into_iter().find(|e| e.state.visible) {
                    if tree.perform_action(&el.id, "click").unwrap_or(false) {
                        return ActionResult::ok();
                    }
                }
            }
            ActionResult::fail("no Cancel/Don't Save/Close button found to dismiss the dialog")
        }
        other => ActionResult::fail(format!("unknown dialog op '{other}'")),
    }
}

/// Receipt for a native-input action delivered via the WS1 background
/// (non-focus-stealing) path — records the route + target pid so the trust
/// loop can see focus was NOT stolen. Foreground dispatches keep the bare
/// `ActionResult::ok()`.
fn background_receipt(kind: &str, pid: i32) -> crate::adapter::ActionResult {
    let mut result = crate::adapter::ActionResult::ok();
    result.data = Some(serde_json::json!({
        "dispatch": "background_pid",
        "action": kind,
        "target_pid": pid,
        "focus_stolen": false,
    }));
    result
}

/// Translate a `PlannedAction::Dock` into a `cel_accessibility::DockOp`,
/// dispatch it, and return the Dock item list (for `list`) on the receipt. WS6.
fn dispatch_dock(op: &str, name: Option<&str>) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    use cel_accessibility::DockOp;

    let dock_op = match op {
        "list" => DockOp::List,
        "launch" => match name {
            Some(n) => DockOp::Launch {
                name: n.to_string(),
            },
            None => return ActionResult::fail("dock launch requires a name"),
        },
        "right_click" => match name {
            Some(n) => DockOp::RightClick {
                name: n.to_string(),
            },
            None => return ActionResult::fail("dock right_click requires a name"),
        },
        "hide" => DockOp::Hide,
        "show" => DockOp::Show,
        other => return ActionResult::fail(format!("unknown dock op '{other}'")),
    };

    match cel_accessibility::perform_dock_op(&dock_op) {
        Ok(result) => {
            let mut r = ActionResult::ok();
            r.data = serde_json::to_value(&result).ok();
            r
        }
        Err(e) => ActionResult::fail(format!("dock {op}: {e}")),
    }
}

/// Translate a `PlannedAction::MenuExtra` into a `cel_accessibility::MenuExtraOp`,
/// dispatch it, and return the item list (for `list`) on the receipt. WS7.
fn dispatch_menu_extra(op: &str, name: Option<&str>) -> crate::adapter::ActionResult {
    use crate::adapter::ActionResult;
    use cel_accessibility::MenuExtraOp;

    let menu_op = match op {
        "list" => MenuExtraOp::List,
        "click" => match name {
            Some(n) => MenuExtraOp::Click {
                name: n.to_string(),
            },
            None => return ActionResult::fail("menu_extra click requires a name"),
        },
        other => return ActionResult::fail(format!("unknown menu_extra op '{other}'")),
    };

    match cel_accessibility::perform_menu_extra_op(&menu_op) {
        Ok(result) => {
            let mut r = ActionResult::ok();
            r.data = serde_json::to_value(&result).ok();
            r
        }
        Err(e) => ActionResult::fail(format!("menu_extra {op}: {e}")),
    }
}

impl Cortex {
    /// Execute a planner action through native CEL primitives.
    ///
    /// This is the first migration slice: native/non-browser actions are owned
    /// by Rust. Adapter-dispatched execution can be layered in afterward.
    pub async fn execute(
        &self,
        action: &PlannedAction,
        context: &ScreenContext,
    ) -> Result<crate::adapter::ActionResult, CortexError> {
        use crate::adapter::ActionResult;

        self.notify_action(action_type_str(action)).await;

        // CDP-direct interception: when bound to a CDP client AND the action
        // targets a `dom:*` element, dispatch via CDP and return immediately.
        // This guarantees the action lands in the bound CDP browser regardless
        // of what app the user has focused — preventing the eval from typing
        // into the user's chat window or destroying their open form.
        let cdp = self.cdp_client.lock().unwrap().clone();
        if let Some(client) = cdp {
            if let Some(result) = try_cdp_dispatch(client.as_ref(), action).await? {
                return Ok(result);
            }
        } else if action_dom_target(action).is_some() {
            // Only a `dom:*` target justifies an ambient bind here — a native
            // action with no CDP client must fall through to the native path,
            // not silently pull a browser into the picture. `cdp_client_or_ambient`
            // binds the discovered client into the shared slot, so perception and
            // any later dispatch reuse this exact connection.
            if let Some(client) = self.cdp_client_or_ambient().await {
                if let Some(result) = try_cdp_dispatch(&client, action).await? {
                    return Ok(result);
                }
            }
        }

        // SAFETY GATE: refuse to fall through to native macOS input drivers
        // unless explicitly opted in via `with_native_input_unsafe()`. Pure
        // data/control actions (Wait, Done, Fail, Extract, NotebookWrites,
        // Batch) and CdpEval still run — only system-I/O actions are gated.
        // See `Cortex::with_native_input_unsafe` for the rationale.
        if !self.allow_native_input && action_requires_native_input(action) {
            return Ok(crate::adapter::ActionResult::fail(format!(
                "cortex refused: action `{}` would dispatch through native macOS input \
                 (mouse/keyboard/AX/app-activation), but allow_native_input=false. \
                 Either bind a CDP client (Cortex::with_cdp_client) for browser-only \
                 execution, or — if you really mean to drive the local machine — \
                 call Cortex::with_native_input_unsafe() at construction. \
                 NEVER enable native input in eval/CI contexts.",
                action_type_str(action),
            )));
        }

        let result = match action {
            PlannedAction::Click { target_id, .. } => {
                if let Some(reason) = self.refuse_ax_on_browser_page(target_id, "click") {
                    return Ok(ActionResult::fail(reason));
                }
                if let Some(element) = find_element(context, target_id) {
                    if try_ax_action(target_id, "click")? {
                        ActionResult::ok()
                    } else if let Some((x, y)) = bounds_center(element) {
                        // AX (try_ax_action above) is already focus-free and
                        // preferred; only this coordinate fallback can steal
                        // focus, so route it through the background path too.
                        if let Some((pid, res)) = self.try_background_input(|pid| {
                            cel_input::background::click(pid, x, y, MouseButton::Left, 1)
                        }) {
                            res?;
                            background_receipt("click", pid)
                        } else {
                            let mut controller = create_controller()
                                .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                            controller
                                .click(x, y, MouseButton::Left)
                                .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                            ActionResult::ok()
                        }
                    } else {
                        ActionResult::fail(format!(
                            "Element \"{target_id}\" has no actionable bounds"
                        ))
                    }
                } else {
                    ActionResult::fail(format!("Element \"{target_id}\" not found"))
                }
            }
            PlannedAction::Type { target_id, text } => {
                // CDP-bound: dispatch text through Input.insertText
                // (or set_value if a dom:* target is supplied). The
                // native path below typed via OS-level enigo, which
                // can't reach a headless Chrome on Linux and
                // mis-targets on macOS when the host window has
                // focus. CDP's insertText writes directly into the
                // bound page's activeElement.
                {
                    let cdp = self.cdp_client.lock().unwrap().clone();
                    if let Some(client) = cdp {
                        return Ok(dispatch_type_via_cdp(
                            client.as_ref(),
                            target_id.as_deref(),
                            text,
                        )
                        .await);
                    }
                }
                // No target_id means "type into whatever's focused" — the
                // exact case the focus gate prevents. Target-bound Type
                // still clicks first, but typing itself still goes OS-level.
                if let Err(e) = self.ensure_browser_focus("type") {
                    return Ok(ActionResult::fail(e.to_string()));
                }
                // Resolve the optional pre-type click target once so both the
                // background and foreground paths share the soft-fail
                // semantics for a missing / unactionable element.
                let click_xy = if let Some(target_id) = target_id {
                    match find_element(context, target_id) {
                        Some(element) => match bounds_center(element) {
                            Some(xy) => Some(xy),
                            None => {
                                return Ok(ActionResult::fail(format!(
                                    "Element \"{target_id}\" has no actionable bounds"
                                )))
                            }
                        },
                        None => {
                            return Ok(ActionResult::fail(format!(
                                "Element \"{target_id}\" not found"
                            )))
                        }
                    }
                } else {
                    None
                };

                if let Some((pid, res)) = self.try_background_input(|pid| {
                    if let Some((x, y)) = click_xy {
                        cel_input::background::click(pid, x, y, MouseButton::Left, 1)?;
                    }
                    cel_input::background::type_text(pid, text)
                }) {
                    res?;
                    background_receipt("type", pid)
                } else {
                    self.ensure_target_app_focus();
                    let mut controller = create_controller()
                        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    if let Some((x, y)) = click_xy {
                        controller
                            .click(x, y, MouseButton::Left)
                            .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    }
                    controller
                        .type_text(text)
                        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    ActionResult::ok()
                }
            }
            PlannedAction::Key { key } => {
                // CDP-bound short-circuit: when the cortex is driving a
                // headless browser there's no OS-level keyboard to send
                // events to — enigo's key_press is a no-op on Linux
                // without an X display, and on macOS it lands in the
                // foreground app (often not the headless Chrome we
                // actually want). Route through CDP's
                // Input.dispatchKeyEvent which targets the bound page
                // directly. See `dispatch_key_via_cdp` for the key-name
                // → CDP-event mapping.
                {
                    let cdp = self.cdp_client.lock().unwrap().clone();
                    if let Some(client) = cdp {
                        return Ok(dispatch_key_via_cdp(&client, key).await);
                    }
                }
                if let Err(e) = self.ensure_browser_focus("key") {
                    return Ok(ActionResult::fail(e.to_string()));
                }
                if let Some((pid, res)) =
                    self.try_background_input(|pid| cel_input::background::key_press(pid, key))
                {
                    res?;
                    background_receipt("key", pid)
                } else {
                    self.ensure_target_app_focus();
                    let mut controller = create_controller()
                        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    controller
                        .key_press(key)
                        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    ActionResult::ok()
                }
            }
            PlannedAction::KeyCombo { keys } => {
                {
                    let cdp = self.cdp_client.lock().unwrap().clone();
                    if let Some(client) = cdp {
                        return Ok(dispatch_keycombo_via_cdp(&client, keys).await);
                    }
                }
                if let Err(e) = self.ensure_browser_focus("key_combo") {
                    return Ok(ActionResult::fail(e.to_string()));
                }
                let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                if let Some((pid, res)) = self
                    .try_background_input(|pid| cel_input::background::key_combo(pid, &key_refs))
                {
                    res?;
                    background_receipt("key_combo", pid)
                } else {
                    self.ensure_target_app_focus();
                    let mut controller = create_controller()
                        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    controller
                        .key_combo(&key_refs)
                        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                    ActionResult::ok()
                }
            }
            PlannedAction::SetValue {
                target_id, value, ..
            } => {
                if try_set_value(target_id, value)? {
                    ActionResult::ok()
                } else {
                    ActionResult::fail(format!("Could not set value on \"{target_id}\""))
                }
            }
            PlannedAction::Scroll { dx, dy } => {
                // CDP-bound: scroll the page directly via JS. Reliable
                // even on a headless Linux server where enigo can't
                // generate scroll events without an X display. The
                // expression also returns the page's new scrollY so
                // the planner's history shows the actual scroll
                // distance (useful when the page hit min/max scroll
                // and didn't move).
                {
                    let cdp = self.cdp_client.lock().unwrap().clone();
                    if let Some(client) = cdp {
                        let js = format!(
                            "(() => {{ window.scrollBy({dx}, {dy}); \
                              return 'ok:scroll:' + Math.round(window.scrollY); }})()"
                        );
                        return Ok(match client.evaluate(&js).await {
                            Ok(v) => check_cdp_ok(v, "scrolled"),
                            Err(e) => ActionResult::fail(format!("cdp scroll: {e}")),
                        });
                    }
                }
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .scroll(*dx, *dy)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Drag {
                from_target_id,
                to_target_id,
            } => {
                let Some(from_element) = find_element(context, from_target_id) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{from_target_id}\" not found"
                    )));
                };
                let Some(to_element) = find_element(context, to_target_id) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{to_target_id}\" not found"
                    )));
                };
                let Some((from_x, from_y)) = bounds_center(from_element) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{from_target_id}\" has no actionable bounds"
                    )));
                };
                let Some((to_x, to_y)) = bounds_center(to_element) else {
                    return Ok(ActionResult::fail(format!(
                        "Element \"{to_target_id}\" has no actionable bounds"
                    )));
                };
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .drag(from_x, from_y, to_x, to_y)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Wait { ms } => {
                tokio::time::sleep(std::time::Duration::from_millis(*ms as u64)).await;
                ActionResult::ok()
            }
            PlannedAction::AxAction {
                target_id,
                action,
                label,
                role_hint,
                ..
            } => {
                if let Some(reason) = self.refuse_ax_on_browser_page(target_id, action) {
                    return Ok(ActionResult::fail(reason));
                }
                // Primary: try the planner-supplied target_id. AX ids
                // are bounds-hashed and therefore fragile across tree
                // mutations (animations, focus shifts, popovers).
                //
                // Skip the primary attempt when target_id is empty —
                // the planner explicitly emitted `null` / left it
                // missing (handled leniently in cel-contracts), which
                // means it only knows the visible label. Calling AX
                // with an empty id wastes a round trip and produces a
                // confusing `failed on ""` in the error message.
                if !target_id.is_empty() {
                    if let Ok(true) = try_ax_action(target_id, action) {
                        return Ok(ActionResult::ok());
                    }
                }
                // Fallback: if the LLM supplied a `label`, ask the
                // live AX tree to resolve role+label → id and try
                // again. Recovers from the common stale-hash failure
                // mode AND from the planner emitting target_id=null
                // with label only.
                if let Some(lbl) = label.as_deref() {
                    if let Some(resolved) = resolve_ax_by_label(lbl, role_hint.as_deref()) {
                        if try_ax_action(&resolved, action).unwrap_or(false) {
                            tracing::info!(
                                target_id = %target_id,
                                resolved = %resolved,
                                label = %lbl,
                                "ax_action fell back to label resolution"
                            );
                            return Ok(ActionResult::ok());
                        }
                    }
                }
                let target_repr = if target_id.is_empty() {
                    "<missing>".to_string()
                } else {
                    format!("\"{target_id}\"")
                };
                ActionResult::fail(format!(
                    "AX action \"{action}\" failed on {target_repr}{}",
                    label
                        .as_ref()
                        .map(|l| format!(" (label=\"{l}\" also not found)"))
                        .unwrap_or_default()
                ))
            }
            PlannedAction::ActivateApp { app_name } => {
                let result = activate_app_with_verification(app_name)?;
                if result.success {
                    // Remember the target so subsequent Key/KeyCombo/Type
                    // actions can re-raise it if focus drifts. See
                    // `ensure_target_app_focus`.
                    if let Ok(mut guard) = self.last_activated_app.lock() {
                        *guard = Some(app_name.clone());
                    }
                }
                result
            }
            PlannedAction::LaunchApp {
                app_name,
                background,
            } => {
                let result =
                    crate::cortex::focus::launch_app_with_verification(app_name, *background)?;
                // A foreground launch also wins focus — remember it like
                // activate_app so follow-up native input can re-raise it.
                if result.success && !*background {
                    if let Ok(mut guard) = self.last_activated_app.lock() {
                        *guard = Some(app_name.clone());
                    }
                }
                result
            }
            PlannedAction::QuitApp { app_name } => {
                let result = crate::cortex::focus::quit_app_with_verification(app_name)?;
                // If we just quit the app we were tracking for focus, forget it.
                if result.success {
                    if let Ok(mut guard) = self.last_activated_app.lock() {
                        if guard.as_deref() == Some(app_name.as_str()) {
                            *guard = None;
                        }
                    }
                }
                result
            }
            PlannedAction::Select {
                from_x,
                from_y,
                to_x,
                to_y,
            } => {
                let mut controller =
                    create_controller().map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                controller
                    .drag(*from_x, *from_y, *to_x, *to_y)
                    .map_err(|e| CortexError::ExecutionFailed(e.to_string()))?;
                ActionResult::ok()
            }
            PlannedAction::Custom {
                adapter,
                action,
                params,
            } => {
                // Route to registered adapter if available
                let adapters = self.adapters.read().await;
                if let Some(registered) = adapters
                    .iter()
                    .find(|a| a.driver.manifest().name == *adapter)
                {
                    if registered.state == crate::adapter::AdapterState::Active {
                        let action_decl = registered.driver.manifest().actions.get(action).cloned();
                        let caller_opted_out_of_verify =
                            params.get("verify").and_then(serde_json::Value::as_bool)
                                == Some(false);
                        match registered.driver.execute(action, params.clone()).await {
                            Ok(result) => {
                                if result.success
                                    && !caller_opted_out_of_verify
                                    && action_decl
                                        .as_ref()
                                        .map(|decl| decl.requires_verification)
                                        .unwrap_or(false)
                                {
                                    match registered
                                        .driver
                                        .verify_action(action, params, &result)
                                        .await
                                    {
                                        Ok(Some(verified)) => verified,
                                        Ok(None) => result,
                                        Err(e) => ActionResult::fail(format!(
                                            "Adapter \"{adapter}\" verification error: {e}"
                                        )),
                                    }
                                } else {
                                    result
                                }
                            }
                            Err(e) => {
                                ActionResult::fail(format!("Adapter \"{adapter}\" error: {e}"))
                            }
                        }
                    } else {
                        ActionResult::fail(format!(
                            "Adapter \"{adapter}\" is not active (state: {:?})",
                            registered.state
                        ))
                    }
                } else {
                    ActionResult::fail(format!(
                        "No adapter registered for \"{adapter}\". Register it in the Cortex first."
                    ))
                }
            }
            PlannedAction::Batch { actions } => {
                // Execute batch sequentially, stop on first failure
                for (i, sub_action) in actions.iter().enumerate() {
                    let sub_result = Box::pin(self.execute(sub_action, context)).await?;
                    if !sub_result.success {
                        return Ok(ActionResult::fail(format!(
                            "Batch action {}/{} failed: {}",
                            i + 1,
                            actions.len(),
                            sub_result.error.unwrap_or_default()
                        )));
                    }
                }
                ActionResult::ok()
            }
            PlannedAction::Act { instruction } => {
                // Semantic action resolution: find best matching element and click it
                // Simple heuristic — match instruction keywords against element labels
                let lower = instruction.to_lowercase();
                if let Some(el) = context.elements.iter().find(|el| {
                    el.state.visible
                        && !el.actions.is_empty()
                        && el
                            .label
                            .as_ref()
                            .is_some_and(|l| lower.contains(&l.to_lowercase()))
                }) {
                    let click = PlannedAction::Click {
                        target_id: el.id.clone(),
                        expect_after: None,
                    };
                    return Box::pin(self.execute(&click, context)).await;
                }
                ActionResult::fail(format!("Could not resolve: {instruction}"))
            }
            PlannedAction::CdpEval { expression } => {
                // Navigation-style cdp_eval (window.location.href = '<url>')
                // must not be dispatched into whatever stale page target
                // connect_to_focused_app() happens to bind. Detect it here
                // and reset CEL's dedicated automation browser to a fresh
                // page target at the requested URL before falling through.
                if let Some(nav_url) = extract_navigation_url(expression) {
                    if let Err(e) = cel_cdp::reset_preferred_target(&nav_url) {
                        tracing::debug!("reset_preferred_target({}) failed: {}", nav_url, e);
                    }
                }
                // Every cdp_eval is preceded by a small, idempotent prelude
                // that patches `HTMLSelectElement.prototype.value` to also
                // match by option text when the supplied value doesn't
                // match an option's `value` attribute. Without this, LLMs
                // writing `select.value = "Technical Support"` hit the HTML
                // spec no-op and forms silently fail validation. Patching
                // at the prototype level means the fix applies regardless
                // of whether the LLM used set_value, cdp_eval, or whatever
                // selector it built internally.
                let full_expression = format!("{CEL_SELECT_PATCH_PRELUDE}\n{expression}");
                match self.cdp_eval_via_shared_or_focused(&full_expression).await {
                    Ok(result) => ActionResult {
                        success: true,
                        error: None,
                        data: Some(serde_json::Value::String(result)),
                    },
                    Err(e) => ActionResult::fail(e),
                }
            }
            PlannedAction::Navigate {
                url,
                wait_until,
                timeout_ms,
                dismiss_overlays,
            } => {
                self.dispatch_navigate(
                    url,
                    wait_until.as_deref(),
                    timeout_ms.unwrap_or(NAVIGATE_DEFAULT_TIMEOUT_MS),
                    dismiss_overlays.unwrap_or(true),
                )
                .await
            }
            PlannedAction::WriteCells {
                app,
                sheet,
                table,
                writes,
                verify,
            } => {
                self.dispatch_write_cells(app, sheet.as_deref(), table.as_deref(), writes, *verify)
                    .await
            }
            PlannedAction::ReadCells {
                app,
                sheet,
                table,
                cell_refs,
            } => {
                self.dispatch_read_cells(app, sheet.as_deref(), table.as_deref(), cell_refs)
                    .await
            }
            PlannedAction::ExtractWithFallback {
                name,
                selectors,
                parse_as,
            } => self.dispatch_extract_with_fallback(name, selectors, parse_as),
            PlannedAction::Window {
                op,
                app,
                window_index,
                x,
                y,
                width,
                height,
                preset,
                display,
            } => dispatch_window(
                op,
                app.as_deref(),
                *window_index,
                *x,
                *y,
                *width,
                *height,
                preset.as_deref(),
                *display,
            ),
            PlannedAction::Dialog {
                op,
                button,
                value,
                field_index,
            } => dispatch_dialog(op, button.as_deref(), value.as_deref(), *field_index),
            PlannedAction::Dock { op, name } => dispatch_dock(op, name.as_deref()),
            PlannedAction::MenuExtra { op, name } => dispatch_menu_extra(op, name.as_deref()),
            PlannedAction::Extract { .. }
            | PlannedAction::Done { .. }
            | PlannedAction::Fail { .. }
            | PlannedAction::NotebookWrites { .. } => ActionResult::ok(),
        };

        if result.success {
            self.report_action_success().await;
        } else {
            self.report_action_failure().await;
        }

        Ok(result)
    }

    /// Extract a single value from the focused CDP page by trying a
    /// list of candidate selectors in order and parsing the first
    /// match. Replaces the "LLM hand-writes document.querySelector in
    /// a loop" failure mode — the runtime owns the retry/parse
    /// machinery and the planner just supplies the selector candidates
    /// plus a logical `name` under which the result is persisted.
    ///
    /// Selector entry is auto-detected:
    ///   * Starts with `function` / `(function` / `(() =>` / `return` →
    ///     treated as a raw JS expression, evaluated directly.
    ///   * Otherwise treated as a CSS selector; wrapped into
    ///     `document.querySelector(SEL)?.textContent ?? null`.
    ///
    /// Returns `ActionResult::ok` with `data = { "name": ..., "value":
    /// <parsed>, "selector_used": <which one hit>, "raw": <raw string> }`
    /// on success. On failure (no selector yielded a non-null value)
    /// returns `ActionResult::fail` with a diagnostic listing every
    /// selector tried and what each yielded — this goes into the
    /// planner's history so the next turn sees exactly what was
    /// tried and why it didn't work.
    fn dispatch_extract_with_fallback(
        &self,
        name: &str,
        selectors: &[String],
        parse_as: &str,
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;
        if selectors.is_empty() {
            return ActionResult::fail(format!(
                "extract_with_fallback({name}): empty selector list — provide at least one candidate"
            ));
        }
        let mut diagnostics: Vec<String> = Vec::with_capacity(selectors.len());
        for sel in selectors {
            let expr = build_extract_expression(sel);
            // Same shared-client preference as `cdp_eval_via_shared_or_focused` —
            // every selector probe in this loop fans out to a new
            // CDP call, and on a 4-selector fallback list with N
            // extractions per scenario that quickly piles up.
            let eval = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    match self.cdp_client_or_ambient().await {
                        Some(client) => client
                            .evaluate_resilient(&expr)
                            .await
                            .map_err(|e| e.to_string()),
                        None => Err("No CDP target available".into()),
                    }
                })
            });
            let raw = match eval {
                Ok(v) => v,
                Err(e) => {
                    diagnostics.push(format!("[{sel}] cdp error: {e}"));
                    continue;
                }
            };
            let raw_str = cdp_value_to_string(&raw);
            if raw_str.is_none() {
                diagnostics.push(format!("[{sel}] selector yielded null"));
                continue;
            }
            let raw_str = raw_str.unwrap();
            let parsed = match parse_extracted(&raw_str, parse_as) {
                Some(v) => v,
                None => {
                    diagnostics.push(format!(
                        "[{sel}] parse_as={parse_as} failed on raw={:?}",
                        truncate(&raw_str, 60)
                    ));
                    continue;
                }
            };
            let data = serde_json::json!({
                "name": name,
                "value": parsed,
                "selector_used": sel,
                "raw": raw_str,
            });
            return ActionResult {
                success: true,
                error: None,
                data: Some(data),
            };
        }
        ActionResult::fail(format!(
            "extract_with_fallback({name}): no selector yielded a usable value — tried {} candidates. {}",
            selectors.len(),
            diagnostics.join("; ")
        ))
    }

    /// Route a `WriteCells` action to the correct scripting backend.
    /// Currently only Numbers is wired up; other apps return a clean
    /// runtime error so the planner can pivot instead of silently
    /// falling back to a path we know produces garbage (keystrokes).
    #[cfg(target_os = "macos")]
    fn with_numbers_document_bootstrap<T, F>(
        &self,
        operation_name: &str,
        mut operation: F,
    ) -> Result<T, InputError>
    where
        F: FnMut() -> Result<T, InputError>,
    {
        match operation() {
            Ok(value) => Ok(value),
            Err(original_error) if should_attempt_numbers_document_bootstrap(&original_error) => {
                warn!(
                    operation = operation_name,
                    error = %original_error,
                    "Numbers scripting unavailable; attempting document bootstrap"
                );
                let bootstrap_result = bootstrap_numbers_document();
                if let Err(ref bootstrap_error) = bootstrap_result {
                    warn!(
                        operation = operation_name,
                        error = bootstrap_error,
                        "Numbers document bootstrap did not confirm success before retry"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(900));
                match operation() {
                    Ok(value) => Ok(value),
                    Err(retry_error) => {
                        if let Err(bootstrap_error) = bootstrap_result {
                            Err(InputError::Failed(format!(
                                "{operation_name} retry failed after Numbers bootstrap attempt ({bootstrap_error}). \
                                 Original error: {original_error}. Retry error: {retry_error}"
                            )))
                        } else {
                            Err(retry_error)
                        }
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    async fn dispatch_adapter_standard_action(
        &self,
        app: &str,
        action: &str,
        params: serde_json::Value,
    ) -> Option<crate::adapter::ActionResult> {
        let adapters = self.adapters.read().await;
        let registered = adapters.iter().find(|candidate| {
            candidate.state == crate::adapter::AdapterState::Active
                && (candidate.driver.manifest().name.eq_ignore_ascii_case(app)
                    || candidate.matches_app(app))
                && candidate.driver.manifest().actions.contains_key(action)
        })?;
        Some(
            self.run_registered_adapter_action(registered, action, params)
                .await,
        )
    }

    /// Standard execute → verify_action lifecycle for a specific registered
    /// adapter, including the `verify: false` opt-out. Shared between the
    /// Active-adapter path (`dispatch_adapter_standard_action`) and
    /// adapter-specific routes that need to bypass the Active gate (e.g. the
    /// Numbers `write_cells` bootstrap fallback in `dispatch_write_cells`).
    async fn run_registered_adapter_action(
        &self,
        registered: &crate::adapter::RegisteredAdapter,
        action: &str,
        params: serde_json::Value,
    ) -> crate::adapter::ActionResult {
        let action_decl = registered.driver.manifest().actions.get(action).cloned();
        let caller_opted_out_of_verify =
            params.get("verify").and_then(serde_json::Value::as_bool) == Some(false);
        match registered.driver.execute(action, params.clone()).await {
            Ok(result) => {
                if result.success
                    && !caller_opted_out_of_verify
                    && action_decl
                        .as_ref()
                        .map(|decl| decl.requires_verification)
                        .unwrap_or(false)
                {
                    match registered
                        .driver
                        .verify_action(action, &params, &result)
                        .await
                    {
                        Ok(Some(verified)) => verified,
                        Ok(None) => result,
                        Err(err) => crate::adapter::ActionResult::fail(format!(
                            "Adapter \"{}\" verification error: {err}",
                            registered.driver.manifest().name
                        )),
                    }
                } else {
                    result
                }
            }
            Err(err) => crate::adapter::ActionResult::fail(format!(
                "Adapter \"{}\" execution error for {action}: {err}",
                registered.driver.manifest().name
            )),
        }
    }

    /// App-agnostic counterpart to [`Self::dispatch_adapter_standard_action`].
    /// Picks the first active adapter declaring `truth_surface == "browser_dom"`
    /// AND the requested action — used by `Navigate`, where there is no
    /// `app` field to key on, just "the browser." When two browser-dom
    /// adapters race (TS Playwright + Rust browser-rs), TS wins by being
    /// the only one declaring `navigate` (browser-rs intentionally exposes
    /// no actions — see `adapters/browser-rs/src/lib.rs:execute`).
    pub(crate) async fn dispatch_browser_dom_action(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Option<crate::adapter::ActionResult> {
        let adapters = self.adapters.read().await;
        let registered = adapters.iter().find(|candidate| {
            candidate.state == crate::adapter::AdapterState::Active
                && candidate.driver.manifest().context.truth_surface == "browser_dom"
                && candidate.driver.manifest().actions.contains_key(action)
        })?;

        let action_decl = registered.driver.manifest().actions.get(action).cloned();
        match registered.driver.execute(action, params.clone()).await {
            Ok(result) => {
                if result.success
                    && action_decl
                        .as_ref()
                        .map(|decl| decl.requires_verification)
                        .unwrap_or(false)
                {
                    match registered
                        .driver
                        .verify_action(action, &params, &result)
                        .await
                    {
                        Ok(Some(verified)) => Some(verified),
                        Ok(None) => Some(result),
                        Err(err) => Some(crate::adapter::ActionResult::fail(format!(
                            "Adapter \"{}\" verification error: {err}",
                            registered.driver.manifest().name
                        ))),
                    }
                } else {
                    Some(result)
                }
            }
            Err(err) => Some(crate::adapter::ActionResult::fail(format!(
                "Adapter \"{}\" execution error for {action}: {err}",
                registered.driver.manifest().name
            ))),
        }
    }

    /// Canonical navigate dispatch.
    ///
    /// Routing order:
    /// 1. **Browser-DOM adapter** (typically the TS Playwright peer) when
    ///    one is registered + active and declares `navigate`. Inherits its
    ///    own waitUntil + cookie-banner handling.
    /// 2. **In-cortex CDP fallback** when no adapter handles it. Calls
    ///    `cel_cdp::Page.navigate` (true in-place navigation, not a new
    ///    tab — fixes the about:blank stray-tab regression that
    ///    `cdp_eval('window.location.href = ...')` produced) and then
    ///    polls `document.readyState` to honour `wait_until`. Optionally
    ///    runs a best-effort cookie/overlay dismiss script.
    ///
    /// `wait_until` is matched leniently — unknown strings collapse to
    /// the default `"domcontentloaded"`. `"none"` skips the poll
    /// entirely; `"networkidle"` is best-effort (no real network
    /// quiescence tracking — readyState=complete + small idle).
    async fn dispatch_navigate(
        &self,
        url: &str,
        wait_until: Option<&str>,
        timeout_ms: u64,
        dismiss_overlays: bool,
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;

        // 1. Adapter-first dispatch. Pass through every canonical knob so
        // adapters that grow richer wait/dismiss semantics later can opt
        // in without a contract bump.
        let adapter_params = serde_json::json!({
            "url": url,
            "wait_until": wait_until.unwrap_or("domcontentloaded"),
            "timeout_ms": timeout_ms,
            "dismiss_overlays": dismiss_overlays,
        });
        if let Some(result) = self
            .dispatch_browser_dom_action("navigate", adapter_params)
            .await
        {
            return result;
        }

        // 2. In-cortex CDP fallback. Best-effort reset of the
        // CEL-dedicated browser's preferred target FIRST so that any
        // freshly-opened page lands at `url` instead of about:blank.
        // No-op (debug-logged) when no CEL-dedicated browser is running
        // on the preferred port.
        if let Err(e) = cel_cdp::reset_preferred_target(url) {
            tracing::debug!("reset_preferred_target({}) failed: {}", url, e);
        }

        let started = std::time::Instant::now();
        let nav_result = match self.cdp_client_or_ambient().await {
            Some(client) => client
                .navigate_resilient(url)
                .await
                .map_err(|e| e.to_string()),
            None => Err("No CDP target available".into()),
        };
        if let Err(e) = nav_result {
            return ActionResult::fail(format!("CDP navigate failed: {e}"));
        }

        // 3. Lifecycle wait via document.readyState polling. Skip
        // entirely on wait_until="none" (callers that want to fire
        // their own verification immediately).
        let target_states = match wait_until.unwrap_or("domcontentloaded") {
            "none" => &[][..],
            "load" | "networkidle" => &["complete"][..],
            _ => &["interactive", "complete"][..],
        };
        let mut load_ms = 0u64;
        if !target_states.is_empty() {
            let deadline = started + std::time::Duration::from_millis(timeout_ms);
            loop {
                let elapsed = started.elapsed();
                if elapsed >= std::time::Duration::from_millis(timeout_ms) {
                    return ActionResult::fail(format!(
                        "navigate({url}): timed out after {timeout_ms}ms waiting for \
                         readyState in {target_states:?}"
                    ));
                }
                if let Ok(state_json) = self
                    .cdp_eval_via_shared_or_focused("document.readyState")
                    .await
                {
                    let state = state_json.trim_matches('"').to_string();
                    if target_states.contains(&state.as_str()) {
                        load_ms = started.elapsed().as_millis() as u64;
                        break;
                    }
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let sleep_for = remaining.min(std::time::Duration::from_millis(NAVIGATE_POLL_MS));
                if sleep_for.is_zero() {
                    return ActionResult::fail(format!(
                        "navigate({url}): timed out after {timeout_ms}ms waiting for \
                         readyState in {target_states:?}"
                    ));
                }
                tokio::time::sleep(sleep_for).await;
            }
            // Best-effort networkidle approximation — small fixed idle
            // beyond readyState=complete. cel-cdp has no real network
            // quiescence tracking, so callers needing precise idle
            // should rely on the TS adapter path.
            if wait_until == Some("networkidle") {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                load_ms = started.elapsed().as_millis() as u64;
            }
        }

        // 4. Optional overlay dismiss. Failures are swallowed — a
        // botched cookie-banner dismiss should never fail the navigate.
        let dismissed = dismiss_overlays
            && self
                .cdp_eval_via_shared_or_focused(CEL_DISMISS_OVERLAYS_JS)
                .await
                .is_ok();

        // 5. Read back final URL so callers can detect redirects. Best-effort.
        let final_url = self
            .cdp_eval_via_shared_or_focused("window.location.href")
            .await
            .ok()
            .map(|s| s.trim_matches('"').to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| url.to_string());
        let redirected = final_url != url;

        ActionResult {
            success: true,
            error: None,
            data: Some(serde_json::json!({
                "url": url,
                "final_url": final_url,
                "redirected": redirected,
                "load_ms": load_ms,
                "dismissed_overlays": dismissed,
                "wait_until": wait_until.unwrap_or("domcontentloaded"),
            })),
        }
    }

    async fn dispatch_write_cells(
        &self,
        app: &str,
        sheet: Option<&str>,
        table: Option<&str>,
        writes: &[cel_contracts::CellWrite],
        verify: bool,
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;
        let adapter_params = serde_json::json!({
            "sheet": sheet,
            "table": table,
            "verify": verify,
            "writes": writes
                .iter()
                .map(|write| serde_json::json!({
                    "cell_ref": write.cell_ref,
                    "value": write.value,
                }))
                .collect::<Vec<_>>(),
        });
        if let Some(result) = self
            .dispatch_adapter_standard_action(app, "write_cells", adapter_params.clone())
            .await
        {
            return result;
        }
        if !app.eq_ignore_ascii_case("Numbers") {
            return ActionResult::fail(format!(
                "write_cells currently only supports app=\"Numbers\"; got \"{app}\". \
                 Use Numbers or fall back to cdp_eval for web spreadsheets."
            ));
        }
        // Numbers adapter is registered but isn't Active (e.g., Cortex started
        // before Numbers became frontmost). Bootstrap the document, then route
        // the call through the same adapter — we deliberately don't reproduce
        // the write/verify/payload logic inline because it diverges from the
        // adapter over time (this fallback used to predate the fix that adds
        // `verified` to the payload, the `cell_refs`-from-`writes` derivation,
        // and the readback-count contract check).
        #[cfg(target_os = "macos")]
        {
            let probe_refs = [String::from("A1")];
            if let Err(e) = self.with_numbers_document_bootstrap("write_cells", || {
                cel_input::read_numbers_cells(sheet, table, &probe_refs).map(|_| ())
            }) {
                return ActionResult::fail(format!("write_cells failed: {e}"));
            }
            let adapters = self.adapters.read().await;
            let Some(registered) = adapters.iter().find(|candidate| {
                candidate
                    .driver
                    .manifest()
                    .name
                    .eq_ignore_ascii_case("numbers")
                    && candidate
                        .driver
                        .manifest()
                        .actions
                        .contains_key("write_cells")
            }) else {
                return ActionResult::fail(
                    "Numbers adapter is not registered in this Cortex.".to_string(),
                );
            };
            let result = self
                .run_registered_adapter_action(registered, "write_cells", adapter_params)
                .await;
            dismiss_numbers_dialog_if_present();
            result
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sheet, table, writes, verify);
            ActionResult::fail("write_cells requires macOS (AppleScript backend)".to_string())
        }
    }

    /// Deterministic spreadsheet cell reads from the app model.
    ///
    /// This is the read-side twin of `write_cells`: use it when AX does
    /// not faithfully surface spreadsheet contents and we need app truth
    /// instead of UI guesswork.
    async fn dispatch_read_cells(
        &self,
        app: &str,
        sheet: Option<&str>,
        table: Option<&str>,
        cell_refs: &[String],
    ) -> crate::adapter::ActionResult {
        use crate::adapter::ActionResult;
        let adapter_params = serde_json::json!({
            "sheet": sheet,
            "table": table,
            "cell_refs": cell_refs,
        });
        if let Some(result) = self
            .dispatch_adapter_standard_action(app, "read_cells", adapter_params)
            .await
        {
            return result;
        }
        if !app.eq_ignore_ascii_case("Numbers") {
            return ActionResult::fail(format!(
                "read_cells currently only supports app=\"Numbers\"; got \"{app}\"."
            ));
        }
        #[cfg(target_os = "macos")]
        {
            match self.with_numbers_document_bootstrap("read_cells", || {
                cel_input::read_numbers_cells(sheet, table, cell_refs)
            }) {
                Ok(readbacks) => {
                    dismiss_numbers_dialog_if_present();
                    let data = serde_json::json!({
                        "app": app,
                        "reads": cell_refs
                            .iter()
                            .zip(readbacks.iter())
                            .map(|(cell_ref, value)| {
                                serde_json::json!({
                                    "ref": cell_ref,
                                    "value": value,
                                })
                            })
                            .collect::<Vec<_>>(),
                    });
                    ActionResult {
                        success: true,
                        error: None,
                        data: Some(data),
                    }
                }
                Err(e) => ActionResult::fail(format!("read_cells failed: {e}")),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (sheet, table, cell_refs);
            ActionResult::fail("read_cells requires macOS (AppleScript backend)".to_string())
        }
    }
}

#[cfg(test)]
mod ws2_window_tests {
    use super::preset_bounds;
    use cel_display::MonitorInfo;

    fn primary_1000x800() -> MonitorInfo {
        MonitorInfo {
            id: 1,
            name: "test".into(),
            x: 0,
            y: 0,
            width: 1000,
            height: 800,
            is_primary: true,
            scale_factor: 1.0,
        }
    }

    #[test]
    fn left_half_reserves_menu_bar() {
        // Primary display reserves a 24pt menu-bar strip at the top.
        assert_eq!(
            preset_bounds("left_half", &primary_1000x800()),
            Some((0.0, 24.0, 500.0, 776.0))
        );
    }

    #[test]
    fn right_half_offsets_x_by_half_width() {
        assert_eq!(
            preset_bounds("right_half", &primary_1000x800()),
            Some((500.0, 24.0, 500.0, 776.0))
        );
    }

    #[test]
    fn maximize_fills_visible_frame() {
        assert_eq!(
            preset_bounds("maximize", &primary_1000x800()),
            Some((0.0, 24.0, 1000.0, 776.0))
        );
    }

    #[test]
    fn bottom_right_quarter() {
        // half = (500, 388); origin offset by half-width and (inset + half-height).
        assert_eq!(
            preset_bounds("bottom_right", &primary_1000x800()),
            Some((500.0, 412.0, 500.0, 388.0))
        );
    }

    #[test]
    fn non_primary_display_has_no_menu_inset() {
        let mut m = primary_1000x800();
        m.is_primary = false;
        assert_eq!(
            preset_bounds("maximize", &m),
            Some((0.0, 0.0, 1000.0, 800.0))
        );
    }

    #[test]
    fn unknown_preset_is_none() {
        assert_eq!(preset_bounds("diagonal", &primary_1000x800()), None);
    }

    #[test]
    fn monitor_index_for_point_finds_containing_display() {
        let displays = vec![
            primary_1000x800(),
            MonitorInfo {
                id: 2,
                name: "ext".into(),
                x: 1000,
                y: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
                scale_factor: 1.0,
            },
        ];
        assert_eq!(
            super::monitor_index_for_point(&displays, 500.0, 400.0),
            Some(0)
        );
        assert_eq!(
            super::monitor_index_for_point(&displays, 1500.0, 500.0),
            Some(1)
        );
        assert_eq!(
            super::monitor_index_for_point(&displays, 5000.0, 5000.0),
            None
        );
    }
}
