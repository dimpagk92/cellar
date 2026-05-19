use napi_derive::napi;

/// Build a fresh ScreenContext by spinning up a one-shot ContextMerger.
///
/// Used by the napi-exported `get_context` JSON wrapper AND by the
/// `canonical_build_planning_view` cold-start fallback — when the cortex
/// is freshly booted and `current_context` is still empty, plan_view
/// would otherwise return `elements: []` for the first ~1s. Falling back
/// to a fresh fetch keeps the contract that "plan_view returns a
/// non-empty view as soon as the cortex is up."
pub(crate) fn fetch_fresh_context() -> napi::Result<cel_context::ScreenContext> {
    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();
    let signals = cel_signals::create_signal_bus();
    let mut merger =
        cel_context::ContextMerger::with_all(a11y, display, network).with_signals(signals);

    if let Ok(vision) = cel_vision::create_provider_from_env() {
        let handle = crate::rt_handle()?;
        merger = merger.with_vision(vision).with_runtime(handle);
    }

    Ok(merger.get_context())
}

/// Get the unified screen context — merges all available streams.
/// Returns JSON string of ScreenContext.
#[napi]
pub fn get_context() -> napi::Result<String> {
    let context = fetch_fresh_context()?;
    serde_json::to_string(&context).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Capture a screenshot and return as PNG bytes.
///
/// `display_id`:
///   - Some(n): capture exactly that monitor.
///   - None: capture the monitor containing the frontmost app's key window
///     (resolved via cel-signals). Falls back to the primary monitor when no
///     frontmost window can be located. Behaviour-preserving for callers who
///     don't pass the parameter on a single-display setup, but correctly
///     captures the active display on multi-monitor rigs where the frontmost
///     app is on a secondary display.
#[napi]
pub fn capture_screen(display_id: Option<u32>) -> napi::Result<napi::bindgen_prelude::Buffer> {
    let mut capture = cel_display::create_capture();
    let frame = match display_id {
        Some(id) => capture
            .capture_monitor(id)
            .map_err(|e| napi::Error::from_reason(e.to_string()))?,
        None => match resolve_active_display(&*capture) {
            Some(id) => capture
                .capture_monitor(id)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?,
            None => capture
                .capture_frame()
                .map_err(|e| napi::Error::from_reason(e.to_string()))?,
        },
    };
    let png =
        cel_display::encode_png(&frame).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(png.into())
}

/// Capture a window by platform capture ID and return as PNG bytes.
#[napi]
pub fn capture_window(window_id: u32) -> napi::Result<napi::bindgen_prelude::Buffer> {
    let mut capture = cel_display::create_capture();
    let frame = capture
        .capture_window(window_id)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let png =
        cel_display::encode_png(&frame).map_err(|e| napi::Error::from_reason(e.to_string()))?;
    Ok(png.into())
}

/// List windows from the capture backend. Returns JSON string.
///
/// This exposes the same platform capture IDs that `capture_window` expects.
#[napi]
pub fn list_capture_windows() -> napi::Result<String> {
    let capture = cel_display::create_capture();
    let windows = capture
        .list_windows()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    serde_json::to_string(&windows).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Find the monitor that contains the frontmost app's key window.
///
/// Returns None when:
/// - No frontmost app is detected (lock screen, no AX permissions, etc.).
/// - The frontmost app has no on-screen window with bounds.
/// - No monitor's bounds intersect the window's centre.
///
/// Callers should fall back to `capture_frame` (primary monitor) when this
/// returns None — that preserves the pre-multi-display behaviour.
fn resolve_active_display(capture: &dyn cel_display::ScreenCapture) -> Option<u32> {
    let bus = cel_signals::create_signal_bus();
    let snap = bus.snapshot();
    let frontmost_app = snap.running_apps.iter().find(|a| a.is_frontmost)?;
    let frontmost_window = snap
        .window_list
        .iter()
        .find(|w| w.app_name == frontmost_app.name && w.is_on_screen)?;
    let cx = frontmost_window.x + (frontmost_window.width as i32) / 2;
    let cy = frontmost_window.y + (frontmost_window.height as i32) / 2;
    let monitors = capture.list_monitors().ok()?;
    monitors
        .iter()
        .find(|m| {
            cx >= m.x
                && cx < m.x + m.width as i32
                && cy >= m.y
                && cy < m.y + m.height as i32
        })
        .map(|m| m.id)
}

/// List available monitors. Returns JSON string.
#[napi]
pub fn list_monitors() -> napi::Result<String> {
    let capture = cel_display::create_capture();
    let monitors = capture
        .list_monitors()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    serde_json::to_string(&monitors).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// List all visible windows on screen. Returns JSON string.
/// Uses cel-signals (CGWindowListCopyWindowInfo on macOS) as the canonical source.
#[napi]
pub fn list_windows() -> napi::Result<String> {
    let bus = cel_signals::create_signal_bus();
    let snap = bus.snapshot();
    serde_json::to_string(&snap.window_list).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Create a resilient ContextReference from a ContextElement JSON.
/// Returns JSON string of ContextReference.
#[napi]
pub fn make_reference(
    element_json: String,
    screen_width: u32,
    screen_height: u32,
) -> napi::Result<String> {
    let element: cel_context::ContextElement = serde_json::from_str(&element_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid element JSON: {}", e)))?;
    let reference = element.to_reference(screen_width, screen_height);
    serde_json::to_string(&reference).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Resolve a ContextReference against a ScreenContext snapshot.
/// Returns JSON ContextElement, or "null" if no match.
#[napi]
pub fn resolve_reference(context_json: String, reference_json: String) -> napi::Result<String> {
    let context: cel_context::ScreenContext = serde_json::from_str(&context_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid context JSON: {}", e)))?;
    let reference: cel_context::ContextReference = serde_json::from_str(&reference_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid reference JSON: {}", e)))?;

    match cel_context::resolve_reference(&context, &reference) {
        Some(element) => {
            serde_json::to_string(element).map_err(|e| napi::Error::from_reason(e.to_string()))
        }
        None => Ok("null".to_string()),
    }
}

/// Build a ScreenContext from externally-provided elements (e.g., browser CDP data).
///
/// Routes through the Rust pipeline for unified confidence scoring, element type
/// normalization, noise filtering, action assignment, and sorting.
///
/// This is the browser adapter's entry point into the Rust CEL core.
/// Elements should have `element_type` set to the raw ARIA role string —
/// the pipeline normalizes it to CEL types via `aria_role_to_cel_type()`.
///
/// Parameters:
/// - elements_json: JSON array of ContextElement (raw ARIA roles as element_type)
/// - network_events_json: JSON array of HttpEvent (CDP-level HTTP requests)
/// - app_name: Application name (e.g., "Browser")
/// - window_title: Window/page title
///
/// Returns: JSON string of ScreenContext with scored/normalized elements.
#[napi]
pub fn build_context_from_elements(
    elements_json: String,
    network_events_json: String,
    app_name: String,
    window_title: String,
) -> napi::Result<String> {
    let elements: Vec<cel_context::ContextElement> = serde_json::from_str(&elements_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid elements JSON: {}", e)))?;
    let http_events: Vec<cel_network::HttpEvent> = serde_json::from_str(&network_events_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid network events JSON: {}", e)))?;

    let context = cel_context::build_from_external(elements, http_events, app_name, window_title);
    serde_json::to_string(&context).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Get minimal context: frontmost app + window title only.
/// Skips full tree walk — typically <50ms vs 2-15s.
/// Returns JSON: { "app": "...", "window": "...", "elements": [], "timestamp_ms": ... }
#[napi]
pub fn get_quick_context() -> napi::Result<String> {
    let signals = cel_signals::create_signal_bus();
    let snap = signals.snapshot();

    let app_name = snap
        .running_apps
        .iter()
        .find(|a| a.is_frontmost)
        .map(|a| a.name.clone())
        .unwrap_or_default();

    let window_title = snap
        .window_list
        .iter()
        .find(|w| w.app_name == app_name && w.is_on_screen)
        .map(|w| w.title.clone())
        .unwrap_or_default();

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Return lightweight context — no elements, no tree walk
    let json = serde_json::json!({
        "app": app_name,
        "window": window_title,
        "elements": [],
        "timestamp_ms": ts,
    });

    serde_json::to_string(&json).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Get focused context for a single element by ID.
/// Returns JSON FocusedContext, or "null" if not found.
#[napi]
pub fn get_context_focused(element_id: String) -> napi::Result<String> {
    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();
    let signals = cel_signals::create_signal_bus();
    let mut merger =
        cel_context::ContextMerger::with_all(a11y, display, network).with_signals(signals);
    if let Ok(vision) = cel_vision::create_provider_from_env() {
        let handle = crate::rt_handle()?;
        merger = merger.with_vision(vision).with_runtime(handle);
    }

    match merger.get_context_focused(&element_id) {
        Some(focused) => {
            serde_json::to_string(&focused).map_err(|e| napi::Error::from_reason(e.to_string()))
        }
        None => Ok("null".to_string()),
    }
}
