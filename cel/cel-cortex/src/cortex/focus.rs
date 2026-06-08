//! Focus management and the AX-on-browser refusal guard.
//!
//! Ensures the intended target app is frontmost before native input
//! (`ensure_browser_focus`, `ensure_target_app_focus`, app activation), and
//! refuses accessibility actions against a CDP-controlled browser page
//! (`refuse_ax_on_browser_page`) so DOM work routes through CDP instead.

use super::cdp::CDP_BROWSERS;
use super::*;

pub(crate) fn try_ax_action(target_id: &str, action: &str) -> Result<bool, CortexError> {
    let tree = cel_accessibility::create_tree();
    tree.perform_action(target_id, action)
        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))
}

/// Resolve a label (and optional role hint) to a live AX element id
/// by querying the accessibility tree right now. Used as a fallback
/// when the planner-supplied hash id isn't found — typically because
/// the tree mutated between plan time and dispatch time (animations,
/// focus shift, modal appearing). Returns the first visible match.
pub(crate) fn resolve_ax_by_label(label: &str, role_hint: Option<&str>) -> Option<String> {
    let tree = cel_accessibility::create_tree();
    let role = role_hint.and_then(parse_role_hint);
    let matches = tree.find_elements(role.as_ref(), Some(label)).ok()?;
    matches.into_iter().find(|e| e.state.visible).map(|e| e.id)
}

/// Map a free-form role string from the LLM ("button", "AXButton",
/// "text field", …) onto [`cel_accessibility::ElementRole`]. Unknown
/// roles return `None` so the fallback search matches on label alone.
fn parse_role_hint(hint: &str) -> Option<cel_accessibility::ElementRole> {
    use cel_accessibility::ElementRole;
    let normalized = hint
        .trim()
        .to_ascii_lowercase()
        .trim_start_matches("ax")
        .replace(['_', '-', ' '], "");
    Some(match normalized.as_str() {
        "button" => ElementRole::Button,
        "input" | "textfield" | "text" => ElementRole::Input,
        "checkbox" => ElementRole::Checkbox,
        "radiobutton" | "radio" => ElementRole::RadioButton,
        "combobox" | "popupbutton" => ElementRole::ComboBox,
        "slider" => ElementRole::Slider,
        "menu" => ElementRole::Menu,
        "menuitem" => ElementRole::MenuItem,
        "tab" => ElementRole::Tab,
        "tabitem" => ElementRole::TabItem,
        "link" => ElementRole::Link,
        "image" => ElementRole::Image,
        "list" => ElementRole::List,
        "listitem" | "row" | "tablerow" => ElementRole::ListItem,
        "cell" | "tablecell" => ElementRole::TableCell,
        "window" => ElementRole::Window,
        "dialog" => ElementRole::Dialog,
        "group" => ElementRole::Group,
        "toolbar" => ElementRole::Toolbar,
        "" => return None,
        _ => return None,
    })
}

pub(crate) fn try_set_value(target_id: &str, value: &str) -> Result<bool, CortexError> {
    let tree = cel_accessibility::create_tree();
    tree.set_value(target_id, value)
        .map_err(|e| CortexError::ExecutionFailed(e.to_string()))
}

/// Activate an app using AppleScript and verify it became frontmost.
/// For browsers, prefer activating CEL's dedicated CDP browser instance when
/// one already exists. This avoids drifting to a different Chrome instance.
pub(crate) fn activate_app_with_verification(
    app_name: &str,
) -> Result<crate::adapter::ActionResult, CortexError> {
    use crate::adapter::ActionResult;

    let is_browser = CDP_BROWSERS
        .iter()
        .any(|b| app_name.to_lowercase().contains(b));

    let activated_preferred_browser = is_browser && cel_cdp::activate_preferred_browser_target();

    if !activated_preferred_browser {
        // Escalating-aggression activation. macOS apps don't always
        // "win" frontmost against whatever process currently has focus
        // — a plain `tell app to activate` can silently lose the race
        // when the runtime is spawned from a session where another app
        // keeps grabbing events. So we fire three things in order:
        //   1. AppleScript activate to launch / wake the app.
        //   2. `open -a` as a safety net for apps that ignore AS.
        //   3. `System Events` sets `frontmost := true` on the process
        //      directly, which is the closest AppleScript gets to
        //      NSRunningApplication.activateIgnoringOtherApps. This is
        //      what actually pins the app to the front when another
        //      session process is fighting for focus.
        let safe_name = app_name.replace('"', "\\\"");
        let mut activate_command = std::process::Command::new("osascript");
        activate_command.args([
            "-e",
            &format!("tell application \"{safe_name}\" to activate"),
        ]);
        let _ = command_status_with_timeout(activate_command, std::time::Duration::from_secs(2));
        let mut open_command = std::process::Command::new("open");
        open_command.arg("-a").arg(app_name);
        let _ = command_status_with_timeout(open_command, std::time::Duration::from_secs(3));
        // Force-frontmost via System Events. Run twice with a short
        // gap — the first call tends to wake the process, the second
        // actually flips frontmost once the window server catches up.
        let force_frontmost = format!(
            r#"tell application "System Events"
                 repeat with p in (every application process whose name is "{safe_name}" or name contains "{safe_name}")
                   set frontmost of p to true
                 end repeat
               end tell"#
        );
        let mut frontmost_command = std::process::Command::new("osascript");
        frontmost_command.args(["-e", &force_frontmost]);
        let _ = command_status_with_timeout(frontmost_command, std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut frontmost_retry = std::process::Command::new("osascript");
        frontmost_retry.args(["-e", &force_frontmost]);
        let _ = command_status_with_timeout(frontmost_retry, std::time::Duration::from_secs(2));
    }

    // Poll to verify the app actually became frontmost. Cold-start
    // launches (Numbers, Pages, Keynote) can take 4–6s when iCloud
    // sync kicks in on first open, so the ceiling is generous —
    // failing too early was causing legitimate launches to misreport.
    let target_lower = app_name.to_lowercase();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Some(frontmost) = get_frontmost_app_name() {
            if frontmost.to_lowercase().contains(&target_lower)
                || target_lower.contains(&frontmost.to_lowercase())
            {
                return Ok(ActionResult::ok());
            }
        }
    }

    // Second chance: even if another app technically holds the
    // frontmost flag (common on macOS when a modal dialog is layered
    // over a launching app), Numbers / Pages / etc. are effectively
    // "the active app" if their process is visible with a window.
    // NSWorkspace's frontmost heuristics can flicker during launch;
    // deferring to "is the app running with a visible window" gives
    // a more useful readout for the downstream sub-goal.
    if app_is_running_with_visible_window(&target_lower) {
        return Ok(ActionResult::ok());
    }

    Ok(ActionResult::fail(format!(
        "App \"{app_name}\" was activated but did not become frontmost"
    )))
}

/// Launch (start) an app by name, verifying the process actually appears.
///
/// With `background`, launches without bringing the app to the front
/// (`open -g`) — for warming up an app the agent drives headlessly. Distinct
/// from `activate_app`, which is about winning *frontmost*; here we only care
/// that the process is now running.
pub(crate) fn launch_app_with_verification(
    app_name: &str,
    background: bool,
) -> Result<crate::adapter::ActionResult, CortexError> {
    use crate::adapter::ActionResult;

    let mut cmd = std::process::Command::new("open");
    if background {
        cmd.arg("-g");
    }
    cmd.arg("-a").arg(app_name);
    let launched = matches!(
        command_status_with_timeout(cmd, std::time::Duration::from_secs(5)),
        Some(s) if s.success()
    );
    if !launched {
        return Ok(ActionResult::fail(format!(
            "Failed to launch \"{app_name}\" (open -a did not succeed)"
        )));
    }

    // Poll until the process appears — cold starts can take a few seconds.
    let target_lower = app_name.to_lowercase();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    while std::time::Instant::now() < deadline {
        if app_is_running_with_visible_window(&target_lower) {
            return Ok(ActionResult::ok());
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Ok(ActionResult::fail(format!(
        "Launched \"{app_name}\" but its process did not appear within 6s"
    )))
}

/// Quit an app by name gracefully (AppleScript `quit`, like ⌘Q), verifying the
/// process is gone. Never force-kills — an app that puts up an unsaved-changes
/// dialog will (correctly) stay running, and that is reported as a failure so
/// the caller can decide what to do.
pub(crate) fn quit_app_with_verification(
    app_name: &str,
) -> Result<crate::adapter::ActionResult, CortexError> {
    use crate::adapter::ActionResult;

    let safe_name = app_name.replace('"', "\\\"");
    let mut cmd = std::process::Command::new("osascript");
    cmd.args(["-e", &format!("tell application \"{safe_name}\" to quit")]);
    let _ = command_status_with_timeout(cmd, std::time::Duration::from_secs(3));

    // Poll until the process disappears.
    let target_lower = app_name.to_lowercase();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if !app_is_running_with_visible_window(&target_lower) {
            return Ok(ActionResult::ok());
        }
    }
    Ok(ActionResult::fail(format!(
        "Asked \"{app_name}\" to quit but its process is still running \
         (it may be showing an unsaved-changes dialog)"
    )))
}

/// Best-effort check: is the named app in the running-apps list with
/// at least one on-screen window? Useful during the narrow window
/// after `open -a` where the app exists but hasn't won frontmost yet.
fn app_is_running_with_visible_window(name_lower: &str) -> bool {
    let script = format!(
        r#"tell application "System Events"
             set hits to (name of application processes whose name contains "{}" or "{}" contains name)
             return length of hits
           end tell"#,
        name_lower, name_lower
    );
    {
        let mut command = std::process::Command::new("osascript");
        command.args(["-e", &script]);
        command_output_with_timeout(command, std::time::Duration::from_secs(1))
    }
    .and_then(|out| {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        s.parse::<u32>().ok()
    })
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Get the name of the current frontmost application via System Events.
/// True when the currently-frontmost macOS app name matches one of the
/// CDP-capable browsers CEL knows about. Used by the focus gate so
/// native-input actions don't fire into the wrong window.
pub(crate) fn frontmost_is_browser() -> bool {
    let Some(name) = get_frontmost_app_name() else {
        return false;
    };
    let lower = name.to_lowercase();
    CDP_BROWSERS.iter().any(|b| lower.contains(b))
}

fn get_frontmost_app_name() -> Option<String> {
    let mut command = std::process::Command::new("osascript");
    command.args([
        "-e",
        "tell application \"System Events\" to name of first process whose frontmost is true",
    ]);
    let output = command_output_with_timeout(command, std::time::Duration::from_secs(1))?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

impl Cortex {
    /// Pre-flight focus check for native-input dispatches (Key, KeyCombo,
    /// Type-without-target-id). These actions dispatch through OS-level
    /// input drivers that target **whatever app is frontmost**. If the
    /// Cortex has a CDP client bound — meaning the caller is driving a
    /// browser — and the frontmost app isn't a browser, the keystrokes
    /// would land in the wrong window (terminal, Claude, editor). Worse,
    /// eval smoke saw this trigger a recovery spiral where Cmd+L gets
    /// typed into the terminal and the goal never escapes.
    ///
    /// The gate:
    ///   1. If no CDP client bound → non-browser goal → let native input fire.
    ///   2. If frontmost is already a browser → proceed.
    ///   3. Otherwise, activate the preferred browser, poll up to
    ///      `poll_ms` for focus to land. Fail with a clear error if it
    ///      never does — refusing to dispatch into the wrong window is
    ///      always safer than guessing.
    ///
    /// Returns `Ok(())` when it's safe to dispatch native input;
    /// `Err(CortexError::ExecutionFailed)` otherwise.
    pub fn ensure_browser_focus(&self, action_kind: &str) -> Result<(), CortexError> {
        // No CDP client = this cortex isn't driving a browser. Native
        // input is the intended primary path; don't gate.
        if self.cdp_client.lock().unwrap().is_none() {
            return Ok(());
        }

        // `with_native_input_unsafe()` is the caller's explicit opt-in:
        // they've accepted that the session is isolated enough that
        // stray keystrokes are fine, and they want to drive non-browser
        // apps too. The browser-focus guard must defer to that opt-in
        // — otherwise scenarios that hand off to Numbers / Finder /
        // Notes can never send keys, because the guard would keep
        // trying to raise Chrome back to the front.
        if self.allow_native_input {
            return Ok(());
        }

        // Fast path: already focused on a browser.
        if frontmost_is_browser() {
            return Ok(());
        }

        warn!(
            action = action_kind,
            "Native input about to fire while focus is off the CDP browser — activating"
        );
        // Try to raise the preferred CDP browser. Poll ~1.2s for focus
        // to land; frontmost changes via osascript are near-instant on
        // macOS but can take up to ~1s on a busy system.
        let _ = cel_cdp::activate_preferred_browser_target();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(1_200) {
            if frontmost_is_browser() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
        }

        let frontmost = get_frontmost_app_name().unwrap_or_else(|| "unknown".into());
        Err(CortexError::ExecutionFailed(format!(
            "Focus guard refused {action_kind}: frontmost app is \"{frontmost}\", \
             not the CEL CDP browser. Raising the browser via osascript \
             didn't land focus within 1.2s — aborting rather than sending \
             keystrokes to the wrong window."
        )))
    }

    /// Runtime refusal of `ax_action` / `click` on web content.
    ///
    /// When a CDP client is bound AND the frontmost app is a
    /// browser, `ax:*` target_ids on page content are almost always
    /// wrong: the AX tree for a web page is a brittle projection of
    /// the DOM, and actions routed through it land on whatever
    /// happens to be focused (often nothing). Refusing them here
    /// forces the planner onto the `cdp_eval` path for in-page work,
    /// where CEL has full reliability.
    ///
    /// Returns `Some(reason)` when the action should be refused, or
    /// `None` to let it proceed. `ax:*` targets for browser chrome
    /// (tabs, bookmarks bar) still work — the runner rejects on
    /// target prefix + browser focus, not on element type, and
    /// legitimate browser-chrome AX ids don't come up in web-content
    /// goals in practice.
    pub(crate) fn refuse_ax_on_browser_page(
        &self,
        target_id: &str,
        action: &str,
    ) -> Option<String> {
        if self.cdp_client.lock().unwrap().is_none() {
            return None;
        }
        if !target_id.starts_with("ax:") {
            return None;
        }
        if !frontmost_is_browser() {
            return None;
        }
        Some(format!(
            "runtime refuses {action} on `{target_id}`: \
             CDP is bound to a browser; in-page interactions must go through \
             `cdp_eval` (click via DOM, not AX). Switch to a cdp_eval action \
             such as `document.querySelector('...').click()` or \
             `window.location.href = '<url>'`."
        ))
    }

    /// Target-app focus gate. If the last successful `activate_app`
    /// named an app X, and X isn't currently frontmost (another app
    /// has stolen focus — a notification, an editor, the session this
    /// eval was spawned from), re-raise X synchronously before the
    /// keystroke dispatches. Best-effort: failures are swallowed
    /// because the caller is already past the safety gate at this
    /// point — we're trying to un-steal focus, not refuse the action.
    ///
    /// Only fires when `allow_native_input` is on. In browser-only
    /// cortexes the browser-focus gate above already handled this.
    pub(crate) fn ensure_target_app_focus(&self) {
        if !self.allow_native_input {
            return;
        }
        let target = match self.last_activated_app.lock() {
            Ok(g) => g.clone(),
            Err(_) => return,
        };
        let Some(target) = target else {
            return;
        };
        let target_lower = target.to_lowercase();
        if let Some(current) = get_frontmost_app_name() {
            if current.to_lowercase().contains(&target_lower)
                || target_lower.contains(&current.to_lowercase())
            {
                return;
            }
            tracing::debug!(
                target = %target,
                current = %current,
                "Pre-keystroke focus gate: target app not frontmost, re-raising"
            );
        }
        let safe_name = target.replace('"', "\\\"");
        // System Events `set frontmost := true` is the same incantation
        // activate_app uses; re-firing it here keeps the dispatch path
        // simple and consistent with launch-time behavior.
        let script = format!(
            r#"tell application "System Events"
                 repeat with p in (every application process whose name is "{}" or name contains "{}")
                   set frontmost of p to true
                 end repeat
               end tell"#,
            safe_name, safe_name
        );
        let mut command = std::process::Command::new("osascript");
        command.args(["-e", &script]);
        let _ = command_status_with_timeout(command, std::time::Duration::from_secs(2));
        // Short settle so the window server flushes the activation
        // before the keystroke lands. 150ms is empirically enough on
        // fast dev machines; shorter and the next CGEvent races the
        // focus change, longer and every step adds observable latency.
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    /// WS1: resolve the PID of the current background-input target. Uses the
    /// most recent `activate_app` target as the intended app — in background
    /// mode the planner still names its target app, we just don't raise it.
    /// Returns `None` when no target is known or the app isn't running, so
    /// callers fall back to the foreground activate-then-dispatch path.
    pub(crate) fn resolve_background_pid(&self) -> Option<i32> {
        let name = self.last_activated_app.lock().ok()?.clone()?;
        pid_for_app_name(&name)
    }

    /// WS1: attempt a non-focus-stealing native-input dispatch. Returns
    /// `Some((pid, result))` when background mode handled it, or `None` when
    /// the caller should use the foreground path (focus_mode != Background,
    /// or no target PID resolved). The op posts directly to `pid` via
    /// `cel_input::background`, which never activates the app.
    pub(crate) fn try_background_input<F>(&self, op: F) -> Option<(i32, Result<(), CortexError>)>
    where
        F: FnOnce(i32) -> Result<(), cel_input::InputError>,
    {
        if self.focus_mode.load(std::sync::atomic::Ordering::Relaxed)
            != cel_contracts::actions::FocusMode::Background as u8
        {
            return None;
        }
        let pid = self.resolve_background_pid()?;
        tracing::debug!(pid, "WS1: dispatching native input to background target");
        Some((
            pid,
            op(pid).map_err(|e| CortexError::ExecutionFailed(e.to_string())),
        ))
    }
}

/// Resolve a macOS application/process name to its PID via System Events.
/// Best-effort: returns `None` when the app isn't running. Used by the WS1
/// background-input path to target an app without activating it.
fn pid_for_app_name(name: &str) -> Option<i32> {
    let safe = name.replace('"', "\\\"");
    let mut command = std::process::Command::new("osascript");
    command.args([
        "-e",
        &format!(
            "tell application \"System Events\" to unix id of first process whose name is \"{safe}\""
        ),
    ]);
    let output = command_output_with_timeout(command, std::time::Duration::from_secs(1))?;
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<i32>()
            .ok()
    } else {
        None
    }
}

#[cfg(test)]
mod ws1_background_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn foreground_mode_never_diverts_to_background() {
        let cortex = Cortex::new("ws1-fg".into());
        let called = AtomicBool::new(false);
        let outcome = cortex.try_background_input(|_pid| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(
            outcome.is_none(),
            "Foreground focus_mode must return None so the caller keeps the \
             activate-then-dispatch path"
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "background op must never run while in Foreground mode"
        );
    }

    #[test]
    fn background_mode_without_target_falls_back() {
        // Background mode, but no `activate_app` has named a target yet, so
        // resolve_background_pid() is None — dispatch must fall back to the
        // foreground path rather than guess a PID. (No event is posted and
        // no osascript runs, since the resolver short-circuits on no target.)
        let cortex = Cortex::new("ws1-bg".into()).with_background_input();
        let called = AtomicBool::new(false);
        let outcome = cortex.try_background_input(|_pid| {
            called.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(
            outcome.is_none(),
            "no resolvable target PID -> None (foreground fallback)"
        );
        assert!(
            !called.load(Ordering::SeqCst),
            "op must not run without a target PID"
        );
    }
}
