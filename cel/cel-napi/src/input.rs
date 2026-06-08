use napi_derive::napi;
use std::sync::Mutex;

/// Browser app names recognized for CDP-aware activation.
const CDP_BROWSERS: &[&str] = &[
    "chrome", "chromium", "brave", "edge", "opera", "vivaldi", "arc",
];

static INPUT_CONTROLLER: std::sync::OnceLock<Mutex<Box<dyn cel_input::InputController>>> =
    std::sync::OnceLock::new();

/// Cached accessibility tree — avoids expensive create_tree() + full traversal on every ax_* call.
/// The tree object is lightweight (just holds the provider state); the expensive work is in
/// get_tree()/perform_action()/set_value() which query the OS each time.
/// But create_tree() itself does AXIsProcessTrusted checks and provider setup, so caching avoids that.
static AX_TREE: std::sync::OnceLock<Mutex<Box<dyn cel_accessibility::AccessibilityTree>>> =
    std::sync::OnceLock::new();

fn with_ax_tree<F, R>(f: F) -> napi::Result<R>
where
    F: FnOnce(&dyn cel_accessibility::AccessibilityTree) -> R,
{
    let mutex = AX_TREE.get_or_init(|| Mutex::new(cel_accessibility::create_tree()));
    let guard = mutex
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("AX tree lock poisoned: {}", e)))?;
    Ok(f(&**guard))
}

pub(crate) fn with_controller<F, R>(f: F) -> napi::Result<R>
where
    F: FnOnce(&mut dyn cel_input::InputController) -> Result<R, cel_input::InputError>,
{
    let mutex = INPUT_CONTROLLER.get_or_init(|| {
        let ctrl = cel_input::create_controller().expect("Failed to create input controller");
        Mutex::new(ctrl)
    });
    let mut guard = mutex
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("Controller lock poisoned: {}", e)))?;
    f(&mut **guard).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Move the mouse to absolute screen coordinates.
#[napi]
pub fn mouse_move(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.mouse_move(x, y))
}

/// Left-click at absolute screen coordinates.
#[napi]
pub fn click(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.click(x, y, cel_input::MouseButton::Left))
}

/// Right-click at absolute screen coordinates.
#[napi]
pub fn right_click(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.click(x, y, cel_input::MouseButton::Right))
}

/// Press (and hold) the left mouse button at absolute coordinates — pairs with
/// `mouse_up` for drag-and-hold / press-drag-release (move between them).
#[napi]
pub fn mouse_down(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.mouse_down(x, y, cel_input::MouseButton::Left))
}

/// Release a held left mouse button at absolute coordinates.
#[napi]
pub fn mouse_up(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.mouse_up(x, y, cel_input::MouseButton::Left))
}

// ─── Background (non-focus-stealing) input — WS1 ──────────────────────────
// These post CGEvents directly to a target PID via cel_input::background, so
// the app never comes frontmost. The MCP `cel_act focus_mode: background`
// param resolves the target app's PID (pid_for_app) and calls these instead
// of the frontmost-routing controller methods above. Additive — the
// foreground controller path is unchanged.

/// Resolve a macOS app/process name to its PID (`None` if not running).
#[napi]
pub fn pid_for_app(name: String) -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        let safe = name.replace('"', "\\\"");
        let output = std::process::Command::new("osascript")
            .args([
                "-e",
                &format!(
                    "tell application \"System Events\" to unix id of first process whose name is \"{safe}\""
                ),
            ])
            .output()
            .ok()?;
        if output.status.success() {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<i32>()
                .ok()
        } else {
            None
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = name;
        None
    }
}

/// Left-click at `(x, y)` delivered to `pid` without activating the app.
#[napi]
pub fn click_to_pid(pid: i32, x: i32, y: i32) -> napi::Result<()> {
    cel_input::background::click(pid, x, y, cel_input::MouseButton::Left, 1)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Right-click at `(x, y)` delivered to `pid` without activating the app.
#[napi]
pub fn right_click_to_pid(pid: i32, x: i32, y: i32) -> napi::Result<()> {
    cel_input::background::click(pid, x, y, cel_input::MouseButton::Right, 1)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Double-click at `(x, y)` delivered to `pid` without activating the app.
#[napi]
pub fn double_click_to_pid(pid: i32, x: i32, y: i32) -> napi::Result<()> {
    cel_input::background::click(pid, x, y, cel_input::MouseButton::Left, 2)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Type `text` into `pid` without activating the app.
#[napi]
pub fn type_text_to_pid(pid: i32, text: String) -> napi::Result<()> {
    cel_input::background::type_text(pid, &text)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Press a single key in `pid` without activating the app.
#[napi]
pub fn key_press_to_pid(pid: i32, key: String) -> napi::Result<()> {
    cel_input::background::key_press(pid, &key).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Press a key combination in `pid` without activating the app.
#[napi]
pub fn key_combo_to_pid(pid: i32, keys: Vec<String>) -> napi::Result<()> {
    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    cel_input::background::key_combo(pid, &refs)
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Whether background (non-focus-stealing) input is available on this host
/// (a usable CGEvent source). The authoritative grant is probed by
/// `cellar doctor`'s `background input` row.
#[napi]
pub fn background_input_available() -> bool {
    cel_input::background::available()
}

/// Double-click at absolute screen coordinates.
#[napi]
pub fn double_click(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.double_click(x, y, cel_input::MouseButton::Left))
}

/// Type a string of text.
#[napi]
pub fn type_text(text: String) -> napi::Result<()> {
    with_controller(|c| c.type_text(&text))
}

/// Type text with a per-character delay (ms) for human cadence (WS8).
/// `delay_ms` = 0 types instantly.
#[napi]
pub fn type_text_cadence(text: String, delay_ms: u32) -> napi::Result<()> {
    with_controller(|c| c.type_text_cadence(&text, delay_ms))
}

/// Paste `text` via the clipboard (Cmd+V), then restore the previous clipboard
/// contents — reliable insertion (emoji / newlines, no autocorrect) that
/// doesn't clobber whatever the user had copied.
#[napi]
pub fn paste_with_restore(text: String) -> napi::Result<()> {
    with_controller(|c| cel_input::paste_with_restore(c, &text))
}

/// Press a single key (e.g., "Enter", "Tab", "Escape").
#[napi]
pub fn key_press(key: String) -> napi::Result<()> {
    with_controller(|c| c.key_press(&key))
}

/// Press a key combination (e.g., ["Ctrl", "C"]).
#[napi]
pub fn key_combo(keys: Vec<String>) -> napi::Result<()> {
    with_controller(|c| {
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
        c.key_combo(&key_refs)
    })
}

/// Get the current mouse cursor position as [x, y].
#[napi]
pub fn mouse_position() -> napi::Result<Vec<i32>> {
    with_controller(|c| {
        let (x, y) = c.mouse_position()?;
        Ok(vec![x, y])
    })
}

/// Drag from one point to another (left mouse button).
#[napi]
pub fn drag(from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> napi::Result<()> {
    with_controller(|c| c.drag(from_x, from_y, to_x, to_y))
}

/// Scroll at the current position.
#[napi]
pub fn scroll(dx: i32, dy: i32) -> napi::Result<()> {
    with_controller(|c| c.scroll(dx, dy))
}

/// Swipe in a direction ("up" | "down" | "left" | "right") by `amount` units.
#[napi]
pub fn swipe(direction: String, amount: i32) -> napi::Result<()> {
    with_controller(|c| c.swipe(&direction, amount))
}

/// Triple-click at absolute screen coordinates (selects full line/paragraph).
#[napi]
pub fn triple_click(x: i32, y: i32) -> napi::Result<()> {
    with_controller(|c| c.triple_click(x, y, cel_input::MouseButton::Left))
}

/// Press a key down without releasing. Pair with key_up() for independent modifier control.
#[napi]
pub fn key_down(key: String) -> napi::Result<()> {
    with_controller(|c| c.key_down(&key))
}

/// Release a key that was previously pressed with key_down().
#[napi]
pub fn key_up(key: String) -> napi::Result<()> {
    with_controller(|c| c.key_up(&key))
}

/// Paste from clipboard (Cmd+V on macOS, Ctrl+V on others).
#[napi]
pub fn paste() -> napi::Result<()> {
    with_controller(|c| c.paste())
}

/// Select all text in the focused element (Cmd+A on macOS, Ctrl+A on others).
#[napi]
pub fn select_all() -> napi::Result<()> {
    with_controller(|c| c.select_all())
}

/// Move the mouse smoothly with human-like interpolation (ease-in-out cubic).
/// `duration_ms`: movement duration in milliseconds (0 = instant).
#[napi]
pub fn mouse_move_smooth(x: i32, y: i32, duration_ms: u32) -> napi::Result<()> {
    with_controller(|c| c.mouse_move_smooth(x, y, duration_ms))
}

/// Execute an action on an accessibility element directly via the native API.
/// More reliable than mouse/keyboard injection for buttons, checkboxes, menu items.
/// `action`: "click", "activate", "increment", "decrement", "cancel", "show_menu",
///           "scroll_to_visible", "raise", "pick", "delete".
#[napi]
pub fn ax_perform_action(element_id: String, action: String) -> napi::Result<bool> {
    with_ax_tree(|tree| {
        tree.perform_action(&element_id, &action)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    })?
}

/// Set a value directly on an accessibility element (bypasses mouse/keyboard entirely).
/// For text fields: sets the text content. For checkboxes: "true"/"false".
/// For sliders: numeric string. Most reliable form-filling path.
#[napi]
pub fn ax_set_value(element_id: String, value: String) -> napi::Result<bool> {
    with_ax_tree(|tree| {
        tree.set_value(&element_id, &value)
            .map_err(|e| napi::Error::from_reason(e.to_string()))
    })?
}

/// Check if an element's value can be set directly via the accessibility API.
#[napi]
pub fn ax_is_settable(element_id: String) -> bool {
    with_ax_tree(|tree| tree.is_settable(&element_id)).unwrap_or(false)
}

/// Get the accessibility element at a screen coordinate (hit testing).
/// Returns JSON AccessibilityElement, or "null" if nothing found.
#[napi]
pub fn ax_element_at_position(x: f64, y: f64) -> napi::Result<String> {
    with_ax_tree(|tree| match tree.element_at_position(x as f32, y as f32) {
        Ok(Some(el)) => {
            serde_json::to_string(&el).map_err(|e| napi::Error::from_reason(e.to_string()))
        }
        Ok(None) => Ok("null".to_string()),
        Err(e) => Err(napi::Error::from_reason(e.to_string())),
    })?
}

/// Get the menu bar structure of the focused application.
/// Returns JSON array of MenuBarItem: [{ path, label, shortcut, enabled }, ...]
/// This is the AI's "command palette" — all discoverable app commands.
#[napi]
pub fn ax_get_menu_bar() -> napi::Result<String> {
    with_ax_tree(|tree| match tree.get_menu_bar() {
        Ok(items) => {
            serde_json::to_string(&items).map_err(|e| napi::Error::from_reason(e.to_string()))
        }
        Err(e) => Err(napi::Error::from_reason(e.to_string())),
    })?
}

/// Get ALL windows of the focused application (not just the focused one).
/// Returns JSON array of AccessibilityElement (shallow trees — title/bounds per window).
#[napi]
pub fn ax_get_all_windows() -> napi::Result<String> {
    with_ax_tree(|tree| match tree.get_all_windows() {
        Ok(windows) => {
            serde_json::to_string(&windows).map_err(|e| napi::Error::from_reason(e.to_string()))
        }
        Err(e) => Err(napi::Error::from_reason(e.to_string())),
    })?
}

/// Activate (bring to front) a macOS application by name.
/// Uses `open -a "AppName"` which is the most reliable app switching method.
/// If the app is already running, it activates; if not, it launches.
/// Activate (bring to front) an application by name.
/// For browsers: prefers activating CEL's dedicated CDP browser instance when
/// one already exists. It never quits or relaunches the user's browser.
#[napi]
pub fn activate_app(app_name: String) -> napi::Result<bool> {
    let is_browser = CDP_BROWSERS
        .iter()
        .any(|b| app_name.to_lowercase().contains(b));

    if is_browser {
        if cel_cdp::activate_preferred_browser_target() {
            return Ok(true);
        }

        let output = std::process::Command::new("open")
            .arg("-a")
            .arg(&app_name)
            .output()
            .map_err(|e| napi::Error::from_reason(format!("Failed to run open -a: {}", e)))?;
        Ok(output.status.success())
    } else {
        // Non-browser: simple activate
        let output = std::process::Command::new("open")
            .arg("-a")
            .arg(&app_name)
            .output()
            .map_err(|e| napi::Error::from_reason(format!("Failed to run open -a: {}", e)))?;
        Ok(output.status.success())
    }
}

/// Launch (start) a macOS application by name.
///
/// Unlike `activate_app`, this is about *starting* the app — with
/// `background = true` it launches without stealing focus (`open -g -a`),
/// useful for warming up an app the agent will drive headlessly. If the app is
/// already running, `open` simply no-ops (or re-activates when not background).
/// Returns true when `open` reported success.
#[napi]
pub fn launch_app(app_name: String, background: Option<bool>) -> napi::Result<bool> {
    let mut cmd = std::process::Command::new("open");
    if background.unwrap_or(false) {
        cmd.arg("-g");
    }
    cmd.arg("-a").arg(&app_name);
    let output = cmd
        .output()
        .map_err(|e| napi::Error::from_reason(format!("Failed to run open -a: {}", e)))?;
    Ok(output.status.success())
}

/// Quit a macOS application by name, gracefully (AppleScript `quit`).
///
/// This asks the app to quit the same way ⌘Q does — the app may surface an
/// unsaved-changes dialog and stay open, which is intentional (we never
/// force-kill the user's app). Returns true when the `quit` command dispatched
/// without error.
#[napi]
pub fn quit_app(app_name: String) -> napi::Result<bool> {
    let safe_name = app_name.replace('"', "\\\"");
    let output = std::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"{safe_name}\" to quit")])
        .output()
        .map_err(|e| napi::Error::from_reason(format!("Failed to run osascript quit: {}", e)))?;
    Ok(output.status.success())
}

/// Execute a shell command and return its stdout.
/// Restricted to safe commands: open, osascript, defaults, system_profiler.
/// Returns JSON: { "success": bool, "stdout": string, "stderr": string, "code": number }
#[napi]
pub fn shell_exec(command: String, args: Vec<String>) -> napi::Result<String> {
    // Allowlist of safe commands
    const ALLOWED: &[&str] = &[
        "open",
        "osascript",
        "defaults",
        "system_profiler",
        "pmset",
        "sw_vers",
    ];
    let cmd_name = command.split('/').next_back().unwrap_or(&command);
    if !ALLOWED.contains(&cmd_name) {
        return Err(napi::Error::from_reason(format!(
            "Command '{}' not in allowlist: {:?}",
            cmd_name, ALLOWED
        )));
    }

    let output = std::process::Command::new(&command)
        .args(&args)
        .output()
        .map_err(|e| napi::Error::from_reason(format!("Failed to execute {}: {}", command, e)))?;

    let result = serde_json::json!({
        "success": output.status.success(),
        "stdout": String::from_utf8_lossy(&output.stdout).to_string(),
        "stderr": String::from_utf8_lossy(&output.stderr).to_string(),
        "code": output.status.code().unwrap_or(-1),
    });
    Ok(result.to_string())
}

// --- Gesture Observation ---

static GESTURE_OBSERVER: std::sync::OnceLock<cel_signals::GestureObserver> =
    std::sync::OnceLock::new();

/// Start capturing trackpad gestures (pinch, swipe, rotate, smart zoom).
/// Events accumulate in a buffer — call gesture_drain() to retrieve them.
/// Requires accessibility permissions on macOS.
#[napi]
pub fn gesture_start() -> napi::Result<()> {
    GESTURE_OBSERVER.get_or_init(|| match cel_signals::GestureObserver::start() {
        Ok(observer) => observer,
        Err(e) => {
            tracing::warn!(
                "Gesture observer failed to start (gesture recording unavailable): {}",
                e
            );
            cel_signals::GestureObserver::no_op()
        }
    });
    Ok(())
}

/// Drain all accumulated gesture events since last call.
/// Returns JSON array of GestureEvent.
#[napi]
pub fn gesture_drain() -> napi::Result<String> {
    let observer = GESTURE_OBSERVER.get().ok_or_else(|| {
        napi::Error::from_reason("Gesture observer not started. Call gesture_start() first.")
    })?;
    let events = observer.drain();
    serde_json::to_string(&events).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Stop capturing trackpad gestures.
#[napi]
pub fn gesture_stop() -> napi::Result<()> {
    if let Some(observer) = GESTURE_OBSERVER.get() {
        observer.stop();
    }
    Ok(())
}
