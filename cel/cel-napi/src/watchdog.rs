use napi_derive::napi;
use std::sync::Mutex;

static WATCHDOG: std::sync::OnceLock<Mutex<cel_context::ContextWatchdog>> =
    std::sync::OnceLock::new();

/// Persistent accessibility tree with AXObserver support.
static AX_OBSERVER: std::sync::OnceLock<Mutex<Box<dyn cel_accessibility::AccessibilityTree>>> =
    std::sync::OnceLock::new();

/// Initialize the context watchdog for change detection.
/// Also starts the AXObserver for push-based accessibility notifications.
#[napi]
pub fn start_watchdog() -> napi::Result<()> {
    let _ = WATCHDOG.get_or_init(|| Mutex::new(cel_context::ContextWatchdog::new()));

    // Start AXObserver for push-based events (supplements polling, doesn't replace it)
    let _ = AX_OBSERVER.get_or_init(|| {
        let mut tree = cel_accessibility::create_tree();
        if let Err(e) = tree.start_observing() {
            tracing::warn!("AXObserver start failed (polling-only mode): {}", e);
        }
        Mutex::new(tree)
    });

    Ok(())
}

/// Poll for watchdog events by comparing current context against the last snapshot.
/// Also drains AXObserver push events and merges them into the output.
/// Returns JSON array of CelEvents.
#[napi]
pub fn poll_events() -> napi::Result<String> {
    let wd_mutex = WATCHDOG.get().ok_or_else(|| {
        napi::Error::from_reason("Watchdog not started. Call start_watchdog() first.".to_string())
    })?;

    let mut wd = wd_mutex
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("Watchdog lock poisoned: {}", e)))?;

    let a11y = cel_accessibility::create_tree();
    let display = cel_display::create_capture();
    let network = cel_network::create_monitor();
    let mut merger = cel_context::ContextMerger::with_all(a11y, display, network);
    let context = merger.get_context();
    let network_idle = merger.recent_network_events().is_empty();

    // Polling-based events (existing behavior)
    let mut events = wd.tick(&context, network_idle);

    // Push-based events from AXObserver (new)
    if let Some(observer_mutex) = AX_OBSERVER.get() {
        if let Ok(mut observer) = observer_mutex.lock() {
            let ax_events = observer.drain_events();
            if !ax_events.is_empty() {
                events.extend(wd.merge_ax_events(ax_events));
            }
        }
    }

    serde_json::to_string(&events).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Stop and reset the watchdog. Also stops the AXObserver.
#[napi]
pub fn stop_watchdog() -> napi::Result<()> {
    if let Some(wd_mutex) = WATCHDOG.get() {
        if let Ok(mut wd) = wd_mutex.lock() {
            wd.reset();
        }
    }
    if let Some(observer_mutex) = AX_OBSERVER.get() {
        if let Ok(mut observer) = observer_mutex.lock() {
            observer.stop_observing();
        }
    }
    Ok(())
}
