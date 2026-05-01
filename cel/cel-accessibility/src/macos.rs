//! macOS Accessibility Bridge via AXUIElement API
//!
//! Uses Apple's Accessibility framework (ApplicationServices) to read the
//! accessibility tree of the focused application. Requires the calling process
//! to have Accessibility permission granted in System Settings.

use crate::tree::*;
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use rayon::prelude::*;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Wrapper to make AXUIElementRef Send+Sync for rayon.
/// macOS AX API reads are thread-safe (per Apple docs).
#[derive(Clone, Copy)]
struct SendableAXRef(AXUIElementRef);
unsafe impl Send for SendableAXRef {}
unsafe impl Sync for SendableAXRef {}

/// Max depth for parallel processing. Beyond this, use sequential to reduce overhead.
const PARALLEL_DEPTH_LIMIT: usize = 2;

// ─── Retina / HiDPI scaling ─────────────────────────────────────────────────

/// Opaque type for CGDisplayMode.
#[repr(C)]
struct __CGDisplayMode(c_void);
type CGDisplayModeRef = *const __CGDisplayMode;
type CGDirectDisplayID = u32;

// CGRect in points (not pixels) for monitor geometry.
#[repr(C)]
struct CGPointF { x: f64, y: f64 }
#[repr(C)]
struct CGSizeF { width: f64, height: f64 }
#[repr(C)]
struct CGRectF { origin: CGPointF, size: CGSizeF }

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRectF;
}

/// Cached display scale factor (Retina = 2.0, non-Retina = 1.0).
/// AX API returns coordinates in Cocoa points; multiply by this to get pixels.
fn get_display_scale_factor() -> f64 {
    use std::sync::OnceLock;
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        unsafe {
            let display = CGMainDisplayID();
            let mode = CGDisplayCopyDisplayMode(display);
            if mode.is_null() {
                return 1.0;
            }
            let pixel_w = CGDisplayModeGetPixelWidth(mode) as f64;
            let point_w = CGDisplayModeGetWidth(mode) as f64;
            CGDisplayModeRelease(mode);
            if point_w > 0.0 { pixel_w / point_w } else { 1.0 }
        }
    })
}

/// Main monitor bounds in Cocoa points (same coordinate space as AX bounds).
fn get_main_monitor_bounds() -> (f64, f64, f64, f64) {
    unsafe {
        let display = CGMainDisplayID();
        let rect = CGDisplayBounds(display);
        (rect.origin.x, rect.origin.y, rect.size.width, rect.size.height)
    }
}

/// Opaque type for AXUIElement.
#[repr(C)]
pub struct __AXUIElement(c_void);
pub type AXUIElementRef = *const __AXUIElement;

/// AXError codes.
#[allow(dead_code)]
pub type AXError = i32;
pub const K_AX_ERROR_SUCCESS: AXError = 0;
#[allow(dead_code)]
pub const K_AX_ERROR_API_DISABLED: AXError = -25211;
#[allow(dead_code)]
pub const K_AX_ERROR_NO_VALUE: AXError = -25212;
#[allow(dead_code)]
pub const K_AX_ERROR_NOT_IMPLEMENTED: AXError = -25208;
#[allow(dead_code)]
pub const K_AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;

// Treat AXUIElementRef as a CFType for memory management
use core_foundation::base::CFTypeRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementCopyActionNames(
        element: AXUIElementRef,
        names: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut bool,
    ) -> AXError;
    fn AXUIElementCopyElementAtPosition(
        application: AXUIElementRef,
        x: f32,
        y: f32,
        element: *mut AXUIElementRef,
    ) -> AXError;
    #[allow(dead_code)]
    fn AXUIElementCopyAttributeNames(
        element: AXUIElementRef,
        names: *mut CFTypeRef,
    ) -> AXError;
    fn AXIsProcessTrusted() -> bool;
    fn CFRelease(cf: *const c_void);
    #[allow(dead_code)]
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

// AXObserver FFI — push-based accessibility notifications
type AXObserverRef = *const c_void;
type AXObserverCallback = unsafe extern "C" fn(
    observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
);

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXObserverCreate(
        application: i32,
        callback: AXObserverCallback,
        out_observer: *mut AXObserverRef,
    ) -> AXError;
    fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> AXError;
    fn AXObserverGetRunLoopSource(
        observer: AXObserverRef,
    ) -> *const c_void; // CFRunLoopSourceRef
}

// CFRunLoop FFI
type CFRunLoopRef = *const c_void;
type CFRunLoopSourceRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: bool) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
}

// kCFRunLoopDefaultMode is a global CFStringRef constant
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: CFStringRef;
}

const MAX_TREE_DEPTH: usize = 15;
const MAX_ELEMENTS: usize = 500;
const _AX_TIMEOUT: Duration = Duration::from_secs(2);

/// Shared event buffer for AXObserver callback.
type EventBuffer = std::sync::Arc<std::sync::Mutex<Vec<AccessibilityEvent>>>;

/// Max age for cached tree before forced refresh (2 seconds).
const CACHE_MAX_AGE_MS: u128 = 2000;

/// The AXObserver callback — called on the CFRunLoop thread when an AX notification fires.
unsafe extern "C" fn ax_observer_callback(
    _observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
) {
    let buffer = &*(refcon as *const std::sync::Mutex<Vec<AccessibilityEvent>>);
    let notification_str = CFString::wrap_under_get_rule(notification).to_string();

    let event = match notification_str.as_str() {
        "AXFocusedUIElementChanged" => {
            let label = get_ax_string(element, "AXTitle");
            AccessibilityEvent::FocusChanged { element_id: label }
        }
        "AXValueChanged" => {
            let label = get_ax_string(element, "AXTitle").unwrap_or_default();
            let value = get_ax_string(element, "AXValue");
            AccessibilityEvent::ValueChanged {
                element_id: label,
                new_value: value,
            }
        }
        "AXLayoutChanged" => AccessibilityEvent::LayoutChanged,
        "AXWindowCreated" => {
            let title = get_ax_string(element, "AXTitle");
            AccessibilityEvent::WindowCreated { title }
        }
        "AXMenuOpened" => AccessibilityEvent::MenuOpened,
        "AXMenuClosed" => AccessibilityEvent::MenuClosed,
        "AXSheetCreated" => AccessibilityEvent::SheetCreated,
        "AXTitleChanged" => {
            let title = get_ax_string(element, "AXTitle");
            AccessibilityEvent::TitleChanged {
                element_id: None,
                new_title: title,
            }
        }
        "AXApplicationActivated" => {
            let app_name = get_ax_string(element, "AXTitle");
            AccessibilityEvent::AppActivated { app_name }
        }
        "AXApplicationDeactivated" => {
            let app_name = get_ax_string(element, "AXTitle");
            AccessibilityEvent::AppDeactivated { app_name }
        }
        "AXWindowMoved" => AccessibilityEvent::WindowMoved,
        "AXWindowResized" => AccessibilityEvent::WindowResized,
        "AXWindowMiniaturized" => AccessibilityEvent::WindowMinimized,
        "AXWindowDeminiaturized" => AccessibilityEvent::WindowRestored,
        "AXSelectedTextChanged" | "AXSelectedChildrenChanged" => AccessibilityEvent::SelectionChanged,
        "AXRowCountChanged" => AccessibilityEvent::RowCountChanged,
        "AXRowExpanded" | "AXRowCollapsed" | "AXSelectedRowsChanged" | "AXSelectedCellsChanged" => {
            AccessibilityEvent::LayoutChanged
        }
        "AXUIElementDestroyed" => AccessibilityEvent::ElementDestroyed,
        "AXMainWindowChanged" => AccessibilityEvent::MainWindowChanged,
        "AXApplicationHidden" => {
            let app_name = get_ax_string(element, "AXTitle");
            AccessibilityEvent::AppHidden { app_name }
        }
        "AXApplicationShown" => {
            let app_name = get_ax_string(element, "AXTitle");
            AccessibilityEvent::AppShown { app_name }
        }
        "AXAnnouncementRequested" => {
            AccessibilityEvent::AnnouncementRequested { text: None }
        }
        "AXHelpTagCreated" => AccessibilityEvent::HelpTagShown,
        _ => return, // Ignore unknown notifications
    };

    if let Ok(mut events) = buffer.lock() {
        if events.len() < 200 { // Cap buffer to prevent unbounded growth
            events.push(event);
        }
    }
}

/// macOS accessibility provider using the AXUIElement API.
pub struct MacAccessibility {
    /// Shared event buffer populated by the AXObserver callback thread.
    events: EventBuffer,
    /// Handle to stop the observer thread.
    observer_thread: Option<ObserverHandle>,
    /// Cached tree from last get_tree() call (Mutex for Sync trait requirement).
    /// Invalidated when: events buffer has new entries, PID changed, or cache is stale.
    cached_tree: std::sync::Mutex<Option<CachedTree>>,
    /// Number of events seen when cache was created (invalidation signal).
    cache_event_count: AtomicUsize,
    /// Per-app adaptive walk throttle — backs off for Electron/heavy apps.
    budget: std::sync::Mutex<crate::budget::AppWalkBudget>,
    /// Fuzzy-dedup cache — skips storing near-identical snapshots.
    snap_cache: std::sync::Mutex<crate::cache::SnapshotCache>,
}

struct CachedTree {
    tree: AccessibilityElement,
    pid: i32,
    created_at: std::time::Instant,
}

struct ObserverHandle {
    run_loop: CFRunLoopRef,
    _thread: std::thread::JoinHandle<()>,
}

// CFRunLoopRef is safe to send across threads (Apple docs: CFRunLoop is thread-safe).
unsafe impl Send for ObserverHandle {}
unsafe impl Sync for ObserverHandle {}

impl MacAccessibility {
    pub fn new() -> Result<Self, AccessibilityError> {
        if !unsafe { AXIsProcessTrusted() } {
            return Err(AccessibilityError::QueryFailed(
                "Accessibility permission not granted. Go to System Settings > Privacy & Security > Accessibility and add this application.".into()
            ));
        }
        Ok(Self {
            events: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            observer_thread: None,
            cached_tree: std::sync::Mutex::new(None),
            cache_event_count: AtomicUsize::new(0),
            budget: std::sync::Mutex::new(crate::budget::AppWalkBudget::new()),
            snap_cache: std::sync::Mutex::new(crate::cache::SnapshotCache::new()),
        })
    }

    /// Start a background AXObserver thread for the given PID.
    fn spawn_observer(&mut self, pid: i32) -> Result<(), AccessibilityError> {
        // Stop existing observer if any
        self.stop_observer();

        let events = self.events.clone();
        let (rl_tx, rl_rx) = std::sync::mpsc::channel::<usize>();

        let thread = std::thread::Builder::new()
            .name("ax-observer".into())
            .spawn(move || {
                unsafe {
                    let mut observer: AXObserverRef = ptr::null();
                    let err = AXObserverCreate(pid, ax_observer_callback, &mut observer);
                    if err != K_AX_ERROR_SUCCESS || observer.is_null() {
                        tracing::warn!("AXObserverCreate failed: {}", err);
                        let _ = rl_tx.send(0);
                        return;
                    }

                    let app = AXUIElementCreateApplication(pid);
                    if app.is_null() {
                        CFRelease(observer);
                        let _ = rl_tx.send(0);
                        return;
                    }

                    // Register for key notifications
                    let notifications = [
                        "AXFocusedUIElementChanged",
                        "AXValueChanged",
                        "AXLayoutChanged",
                        "AXWindowCreated",
                        "AXMenuOpened",
                        "AXMenuClosed",
                        "AXSheetCreated",
                        "AXTitleChanged",
                        "AXApplicationActivated",
                        "AXApplicationDeactivated",
                        "AXWindowMoved",
                        "AXWindowResized",
                        "AXWindowMiniaturized",
                        "AXWindowDeminiaturized",
                        "AXSelectedTextChanged",
                        "AXSelectedChildrenChanged",
                        "AXRowCountChanged",
                        "AXRowExpanded",
                        "AXRowCollapsed",
                        "AXSelectedRowsChanged",
                        "AXSelectedCellsChanged",
                        "AXUIElementDestroyed",
                        "AXMainWindowChanged",
                        "AXApplicationHidden",
                        "AXApplicationShown",
                        "AXAnnouncementRequested",
                        "AXHelpTagCreated",
                    ];

                    let refcon = std::sync::Arc::into_raw(events) as *mut c_void;

                    for name in &notifications {
                        let cf_name = CFString::new(name);
                        AXObserverAddNotification(
                            observer,
                            app,
                            cf_name.as_concrete_TypeRef(),
                            refcon,
                        );
                    }

                    // Add the observer's run loop source to the current thread's run loop
                    let source = AXObserverGetRunLoopSource(observer);
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(rl, source, kCFRunLoopDefaultMode);

                    // Send the run loop ref back so stop_observer() can call CFRunLoopStop
                    let _ = rl_tx.send(rl as usize);

                    // Run the loop — blocks until CFRunLoopStop is called
                    loop {
                        let result = CFRunLoopRunInMode(
                            kCFRunLoopDefaultMode,
                            1.0,
                            false,
                        );
                        // result == 1 means the run loop was stopped via CFRunLoopStop
                        if result == 1 {
                            break;
                        }
                    }

                    // Reconstruct the Arc to drop it properly
                    let _ = std::sync::Arc::from_raw(refcon as *const std::sync::Mutex<Vec<AccessibilityEvent>>);

                    CFRelease(app as *const c_void);
                    CFRelease(observer);
                }
            })
            .map_err(|e| AccessibilityError::QueryFailed(format!("Failed to spawn observer thread: {}", e)))?;

        // Wait for the thread to send us its CFRunLoopRef
        let run_loop_addr = rl_rx.recv_timeout(Duration::from_secs(2)).unwrap_or(0);

        self.observer_thread = Some(ObserverHandle {
            run_loop: run_loop_addr as CFRunLoopRef,
            _thread: thread,
        });

        Ok(())
    }

    fn stop_observer(&mut self) {
        if let Some(handle) = self.observer_thread.take() {
            if !handle.run_loop.is_null() {
                unsafe { CFRunLoopStop(handle.run_loop) };
            }
            // Thread will exit on its own within 1 second (RunInMode timeout)
        }
    }

    /// Walk the AX tree to find an element by its content-hash ID,
    /// then execute an action on it via AXUIElementPerformAction.
    fn find_and_perform_action(
        &self,
        element: AXUIElementRef,
        target_id: &str,
        action: &str,
        depth: usize,
        count: &mut usize,
    ) -> Result<bool, AccessibilityError> {
        if depth >= MAX_TREE_DEPTH || *count >= MAX_ELEMENTS || element.is_null() {
            return Ok(false);
        }
        *count += 1;

        // Recompute the content-hash ID using the same logic as build_element
        let role_str = get_ax_string(element, "AXRole").unwrap_or_default();
        let label = get_ax_string(element, "AXTitle")
            .or_else(|| get_ax_string(element, "AXDescription"))
            .or_else(|| get_ax_string(element, "AXHelp"));

        let id = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            role_str.hash(&mut hasher);
            label.hash(&mut hasher);
            // NOTE: bounds (x/y/width/height) are intentionally NOT hashed.
            // Including them made IDs rotate whenever a window moved or content reflowed,
            // breaking ax_action lookups (stored ID no longer matched the live element).
            // Known limitation: sibling-index (my_count/*count) still causes rotation when
            // tree membership changes. Fixing that requires replacing the global counter
            // with a content-derived sibling key — separate follow-up.
            depth.hash(&mut hasher);
            (*count).hash(&mut hasher);
            format!("ax:{:016x}", hasher.finish())
        };

        // Match found — perform the action
        if id == target_id {
            let ax_action = match action {
                "click" => "AXPress",
                "activate" => "AXConfirm",
                "increment" => "AXIncrement",
                "decrement" => "AXDecrement",
                "cancel" => "AXCancel",
                "show_menu" => "AXShowMenu",
                "scroll_to_visible" => "AXScrollToVisible",
                "raise" => "AXRaise",
                "pick" => "AXPick",
                "delete" => "AXDelete",
                other => other, // Pass through raw AX action names
            };
            let action_cf = CFString::new(ax_action);
            let err = unsafe {
                AXUIElementPerformAction(element, action_cf.as_concrete_TypeRef())
            };
            return if err == K_AX_ERROR_SUCCESS {
                Ok(true)
            } else {
                Err(AccessibilityError::OperationFailed(format!(
                    "AXPerformAction '{}' failed with error {}", action, err
                )))
            };
        }

        // Not found — recurse into children
        if let Some(kids_cf) = get_ax_attribute(element, "AXChildren") {
            let kids_ref = kids_cf.as_CFTypeRef();
            if unsafe { core_foundation::array::CFArrayGetTypeID() }
                == unsafe { core_foundation::base::CFGetTypeID(kids_ref) }
            {
                let arr: CFArray<CFType> = unsafe {
                    CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef)
                };
                for i in 0..arr.len() {
                    if *count >= MAX_ELEMENTS {
                        break;
                    }
                    if let Some(child_ref) = arr.get(i).map(|c| c.as_CFTypeRef() as AXUIElementRef) {
                        match self.find_and_perform_action(child_ref, target_id, action, depth + 1, count) {
                            Ok(true) => return Ok(true),
                            Err(e) => return Err(e),
                            Ok(false) => {} // Keep searching
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Get the PID of the focused application.
    /// Tries AXUIElement system-wide first, falls back to NSWorkspace frontmostApplication.
    fn get_focused_app_pid(&self) -> Result<i32, AccessibilityError> {
        // Try 1: AXUIElement system-wide focused application
        let system_wide = unsafe { AXUIElementCreateSystemWide() };
        if !system_wide.is_null() {
            let focused_app = get_ax_attribute(system_wide, "AXFocusedApplication");
            unsafe { CFRelease(system_wide as *const c_void) };

            if let Some(focused_app) = focused_app {
                let app_ref = focused_app.as_CFTypeRef() as AXUIElementRef;
                let mut pid: i32 = 0;
                let err = unsafe { AXUIElementGetPid(app_ref, &mut pid) };
                if err == K_AX_ERROR_SUCCESS && pid > 0 {
                    return Ok(pid);
                }
            }
        }

        // Try 2: Use NSWorkspace to get frontmost application PID
        // This works even when the AX system-wide query fails
        let output = std::process::Command::new("osascript")
            .args(["-e", "tell application \"System Events\" to unix id of first process whose frontmost is true"])
            .output()
            .map_err(|e| AccessibilityError::QueryFailed(format!("osascript failed: {}", e)))?;

        if output.status.success() {
            let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let pid: i32 = pid_str.parse().map_err(|_| {
                AccessibilityError::QueryFailed(format!("Failed to parse PID: {}", pid_str))
            })?;
            return Ok(pid);
        }

        Err(AccessibilityError::QueryFailed(
            "Could not determine focused application PID".into(),
        ))
    }

    /// Recursively build the accessibility tree from an AXUIElement.
    /// Uses AtomicUsize for thread-safe element counting (needed for rayon parallelism).
    /// `deadline` prevents the traversal from taking too long on heavy pages.
    fn build_element(
        &self,
        element: AXUIElementRef,
        parent_id: Option<&str>,
        depth: usize,
        count: &AtomicUsize,
        deadline: &std::time::Instant,
        max_nodes: usize,
    ) -> Option<AccessibilityElement> {
        let current_count = count.load(Ordering::Relaxed);
        if depth >= MAX_TREE_DEPTH || current_count >= max_nodes || element.is_null() {
            return None;
        }
        // Timeout check — only check every 10 elements to reduce clock overhead
        if current_count % 10 == 0 && std::time::Instant::now() > *deadline {
            tracing::debug!("AX tree traversal hit timeout at depth {}, count {}", depth, current_count);
            return None;
        }

        // ─── EARLY EXIT: check visibility BEFORE expensive attribute queries ───
        if let Some(true) = get_ax_bool(element, "AXHidden") {
            return None;
        }
        let bounds = get_ax_bounds(element);
        if let Some(ref b) = bounds {
            let right = b.x + b.width as i32;
            let bottom = b.y + b.height as i32;
            if right <= 0 || bottom <= 0 || b.x >= 7680 || b.y >= 4320 {
                return None;
            }
            if b.width == 0 && b.height == 0 {
                return None;
            }
        }

        let my_count = count.fetch_add(1, Ordering::Relaxed);

        // ─── Core properties (FAST: ~6 FFI calls) ───
        let role_str = get_ax_string(element, "AXRole").unwrap_or_default();
        let role = map_role(&role_str, None);

        let label = get_ax_string(element, "AXTitle")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| get_ax_string(element, "AXDescription"));

        // Filter out empty text elements and spacers early
        if role_str == "AXStaticText"
            && label.as_deref().map_or(true, |l| l.trim().is_empty())
        {
            let value_check = get_ax_string(element, "AXValue");
            if value_check.as_deref().map_or(true, |v| v.trim().is_empty()) {
                return None;
            }
        }

        let value = get_ax_string(element, "AXValue");
        let state = get_ax_state_fast(element, &role_str);

        let actions = if is_interactive_role(&role_str) {
            get_ax_actions(element)
        } else {
            Vec::new()
        };

        let description = if label.is_none() {
            get_ax_string(element, "AXHelp")
        } else {
            None
        };

        let mut properties = std::collections::HashMap::new();
        // Extract rich properties for form filling and link identification
        if let Some(v) = get_ax_string(element, "AXPlaceholderValue") {
            properties.insert("placeholder".into(), v);
        }
        if let Some(v) = get_ax_string(element, "AXURL") {
            properties.insert("url".into(), v);
        }
        if let Some(v) = get_ax_string(element, "AXRoleDescription") {
            properties.insert("role_desc".into(), v);
        }
        if let Some(v) = get_ax_string(element, "AXSelectedText") {
            properties.insert("selected_text".into(), v);
        }
        if get_ax_bool(element, "AXRequired").unwrap_or(false) {
            properties.insert("required".into(), "true".into());
        }
        if let Some(v) = get_ax_string(element, "AXInvalid") {
            if v != "false" { properties.insert("invalid".into(), v); }
        }
        // Slider bounds
        if role_str == "AXSlider" || role_str == "AXValueIndicator" {
            if let Some(v) = get_ax_string(element, "AXMinValue") {
                properties.insert("min_value".into(), v);
            }
            if let Some(v) = get_ax_string(element, "AXMaxValue") {
                properties.insert("max_value".into(), v);
            }
        }
        if role_str == "AXTable" || role_str == "AXOutline" {
            if let Some(v) = get_ax_array_len(element, "AXRows") {
                properties.insert("row_count".into(), v.to_string());
            }
            if let Some(v) = get_ax_array_len(element, "AXColumns") {
                properties.insert("column_count".into(), v.to_string());
            }
        }
        if matches!(role_str.as_str(), "AXDialog" | "AXSheet") {
            properties.insert("dialog".into(), "true".into());
        }
        if role_str == "AXRow" {
            properties.insert("table_row".into(), "true".into());
        }
        if role_str == "AXCell" {
            properties.insert("table_cell".into(), "true".into());
        }

        // Generate stable ID from content hash
        let id = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            role_str.hash(&mut hasher);
            label.hash(&mut hasher);
            // NOTE: bounds (x/y/width/height) are intentionally NOT hashed.
            // Including them made IDs rotate whenever a window moved or content reflowed,
            // breaking ax_action lookups (stored ID no longer matched the live element).
            // Known limitation: sibling-index (my_count/*count) still causes rotation when
            // tree membership changes. Fixing that requires replacing the global counter
            // with a content-derived sibling key — separate follow-up.
            depth.hash(&mut hasher);
            my_count.hash(&mut hasher);
            format!("ax:{:016x}", hasher.finish())
        };

        // ─── Children: PARALLEL for shallow depths, sequential for deep ───
        let children = if depth + 1 < MAX_TREE_DEPTH && count.load(Ordering::Relaxed) < max_nodes {
            if let Some(kids_cf) = get_ax_attribute(element, "AXChildren") {
                let kids_ref = kids_cf.as_CFTypeRef();
                if unsafe { core_foundation::array::CFArrayGetTypeID() }
                    == unsafe { core_foundation::base::CFGetTypeID(kids_ref) }
                {
                    let arr: CFArray<CFType> = unsafe {
                        CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef)
                    };

                    // Collect child refs
                    let child_refs: Vec<SendableAXRef> = (0..arr.len())
                        .filter_map(|i| {
                            arr.get(i).map(|c| SendableAXRef(c.as_CFTypeRef() as AXUIElementRef))
                        })
                        .collect();

                    if depth < PARALLEL_DEPTH_LIMIT && child_refs.len() > 1 {
                        // PARALLEL: process siblings on multiple cores (top 2 levels only)
                        child_refs
                            .par_iter()
                            .filter_map(|child| {
                                if count.load(Ordering::Relaxed) >= max_nodes {
                                    return None;
                                }
                                self.build_element(child.0, Some(&id), depth + 1, count, deadline, max_nodes)
                            })
                            .collect()
                    } else {
                        // SEQUENTIAL: deep levels use single-threaded walk (less overhead)
                        let mut kids = Vec::new();
                        for child in &child_refs {
                            if count.load(Ordering::Relaxed) >= max_nodes {
                                break;
                            }
                            if let Some(child_elem) = self.build_element(child.0, Some(&id), depth + 1, count, deadline, max_nodes) {
                                kids.push(child_elem);
                            }
                        }
                        kids
                    }
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let normalized_bounds = bounds.as_ref().and_then(|b| {
            let (mx, my, mw, mh) = get_main_monitor_bounds();
            NormalizedBounds::from_pixels(b, mx, my, mw, mh)
        });

        Some(AccessibilityElement {
            id,
            role,
            label,
            description,
            value,
            bounds,
            normalized_bounds,
            state,
            parent_id: parent_id.map(|s| s.to_string()),
            actions,
            properties,
            children,
            automation_id: get_ax_string(element, "AXIdentifier"),
            subrole: get_ax_string(element, "AXSubrole"),
            role_description: get_ax_string(element, "AXRoleDescription"),
            placeholder: get_ax_string(element, "AXPlaceholderValue"),
            help_text: get_ax_string(element, "AXHelp"),
            url: get_ax_string(element, "AXURL"),
            is_password: Some(role_str == "AXSecureTextField"),
            is_keyboard_focusable: Some(is_interactive_role(&role_str)),
            ..Default::default()
        })
    }
}

impl MacAccessibility {
    /// Build the full tree for a given PID.
    fn build_tree_for_pid(
        &self,
        pid: i32,
        max_nodes: usize,
        timeout: std::time::Duration,
    ) -> Result<(AccessibilityElement, TruncationReason), AccessibilityError> {
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return Err(AccessibilityError::QueryFailed("Failed to create app element".into()));
        }

        let app_label = get_ax_string(app, "AXTitle");

        let window_cf = get_ax_attribute(app, "AXFocusedWindow");
        let window_ref = window_cf
            .as_ref()
            .map(|w| w.as_CFTypeRef() as AXUIElementRef);

        // Incognito check — skip private browser windows before expensive walk
        if let Some(wref) = window_ref {
            if let Some(ref title) = get_ax_string(wref, "AXTitle") {
                if crate::incognito::is_title_private(title) {
                    tracing::debug!("Skipping private window: {:?}", title);
                    unsafe { CFRelease(app as *const c_void) };
                    let stub = AccessibilityElement {
                        id: "private-window".into(),
                        role: ElementRole::Window,
                        label: Some(title.clone()),
                        is_password: Some(true),
                        ..Default::default()
                    };
                    return Ok((stub, TruncationReason::None));
                }
            }
        }

        let target = window_ref.unwrap_or(app);

        let count = AtomicUsize::new(0);
        let walk_start = std::time::Instant::now();
        let deadline = walk_start + timeout;
        let root = self.build_element(target, None, 0, &count, &deadline, max_nodes);

        let final_count = count.load(Ordering::Relaxed);
        let elapsed = walk_start.elapsed();
        let truncation = if final_count >= max_nodes {
            TruncationReason::MaxNodes
        } else if elapsed >= timeout {
            TruncationReason::Timeout
        } else {
            TruncationReason::None
        };

        unsafe { CFRelease(app as *const c_void) };

        match root {
            Some(mut elem) => {
                if elem.role == ElementRole::Window || matches!(elem.role, ElementRole::Custom(_)) {
                    if let Some(name) = &app_label {
                        if elem.label.is_none() || elem.label.as_deref() == Some("") {
                            elem.label = Some(name.clone());
                        }
                    }
                }
                Ok((elem, truncation))
            }
            None => Err(AccessibilityError::QueryFailed(
                "Failed to build tree from focused window".into(),
            )),
        }
    }

    /// Get the display name of an app by PID (cheap — single AX attribute read).
    fn get_app_name_for_pid(&self, pid: i32) -> String {
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() { return pid.to_string(); }
        let name = get_ax_string(app, "AXTitle").unwrap_or_else(|| pid.to_string());
        unsafe { CFRelease(app as *const c_void) };
        name
    }
}

/// Collect all label/value text from a tree for SimHash computation.
fn collect_element_text(elem: &AccessibilityElement) -> String {
    let mut buf = String::new();
    fn recurse(e: &AccessibilityElement, b: &mut String) {
        if let Some(ref l) = e.label { b.push_str(l); b.push(' '); }
        if let Some(ref v) = e.value { b.push_str(v); b.push(' '); }
        for child in &e.children { recurse(child, b); }
    }
    recurse(elem, &mut buf);
    buf
}

/// Count actionable elements (buttons, inputs, links, etc.) in a tree.
fn count_actionable(el: &AccessibilityElement) -> usize {
    let self_actionable = matches!(
        el.role,
        ElementRole::Button
            | ElementRole::Input
            | ElementRole::Link
            | ElementRole::Checkbox
            | ElementRole::RadioButton
            | ElementRole::ComboBox
            | ElementRole::Slider
            | ElementRole::MenuItem
            | ElementRole::Tab
            | ElementRole::TabItem
    );
    let children_count: usize = el.children.iter().map(count_actionable).sum();
    (if self_actionable { 1 } else { 0 }) + children_count
}

impl AccessibilityTree for MacAccessibility {
    fn get_tree(&self) -> Result<AccessibilityElement, AccessibilityError> {
        let pid = self.get_focused_app_pid()?;
        let app_name = self.get_app_name_for_pid(pid);

        // ── BUDGET CHECK: may throttle walk for heavy apps ──
        let decision = self.budget.lock()
            .map(|mut b| b.should_walk(&app_name))
            .unwrap_or(crate::budget::WalkDecision {
                walk: true,
                max_nodes: MAX_ELEMENTS,
                timeout: std::time::Duration::from_secs(5),
                tier: crate::budget::WalkTier::Light,
            });

        let current_event_count = self.events.lock().map(|e| e.len()).unwrap_or(0);
        let cached_event_count = self.cache_event_count.load(Ordering::Relaxed);

        // If budget says don't walk yet, return cached tree if valid
        if !decision.walk {
            if let Ok(guard) = self.cached_tree.lock() {
                if let Some(ref cached) = *guard {
                    if cached.pid == pid {
                        tracing::debug!("AX walk throttled by budget (tier: {:?})", decision.tier);
                        return Ok(cached.tree.clone());
                    }
                }
            }
            // No valid cache — fall through and walk anyway
        }

        // ── CACHE CHECK: return cached tree if no AX events fired and same PID ──
        if let Ok(guard) = self.cached_tree.lock() {
            if let Some(ref cached) = *guard {
                let age_ms = cached.created_at.elapsed().as_millis();
                if cached.pid == pid
                    && current_event_count == cached_event_count
                    && age_ms < CACHE_MAX_AGE_MS
                {
                    tracing::debug!("AX tree cache hit (age: {}ms, events: {})", age_ms, current_event_count);
                    return Ok(cached.tree.clone());
                }
            }
        }

        // ── FULL TREE WALK ──
        let walk_start = std::time::Instant::now();
        let (mut elem, truncation) = self.build_tree_for_pid(pid, decision.max_nodes, decision.timeout)?;
        let walk_duration = walk_start.elapsed();

        // Record walk outcome in budget
        if let Ok(mut b) = self.budget.lock() {
            b.record_walk(&app_name, walk_duration, truncation != TruncationReason::None);
        }

        // Chromium warmup retry
        let actionable = count_actionable(&elem);
        if actionable < 5 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Ok((retry, retry_trunc)) = self.build_tree_for_pid(pid, decision.max_nodes, decision.timeout) {
                if count_actionable(&retry) > actionable {
                    if let Ok(mut b) = self.budget.lock() {
                        b.record_walk(&app_name, walk_duration, retry_trunc != TruncationReason::None);
                    }
                    elem = retry;
                }
            }
        }

        // ── SIMHASH DEDUP: log near-identical snapshots ──
        let window_title = elem.label.clone().unwrap_or_default();
        let text = collect_element_text(&elem);
        let hash = crate::simhash::simhash(&text);
        if let Ok(mut sc) = self.snap_cache.lock() {
            if sc.should_store(&app_name, &window_title, hash) {
                sc.record(&app_name, &window_title, hash);
            } else {
                tracing::debug!(
                    "AX snapshot dedup: near-identical content (app={}, window={})",
                    app_name, window_title
                );
            }
        }

        // Update AX event cache
        if let Ok(mut guard) = self.cached_tree.lock() {
            *guard = Some(CachedTree {
                tree: elem.clone(),
                pid,
                created_at: std::time::Instant::now(),
            });
        }
        self.cache_event_count.store(current_event_count, Ordering::Relaxed);

        Ok(elem)
    }

    fn find_elements(
        &self,
        role: Option<&ElementRole>,
        label: Option<&str>,
    ) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        let tree = self.get_tree()?;
        let mut results = Vec::new();
        find_in_tree(&tree, role, label, &mut results);
        Ok(results)
    }

    fn focused_element(&self) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        let system_wide = unsafe { AXUIElementCreateSystemWide() };
        if system_wide.is_null() {
            return Ok(None);
        }

        let focused_cf = get_ax_attribute(system_wide, "AXFocusedUIElement");
        unsafe { CFRelease(system_wide as *const c_void) };

        let focused_ref = match focused_cf {
            Some(ref cf) => cf.as_CFTypeRef() as AXUIElementRef,
            None => return Ok(None),
        };

        let count = AtomicUsize::new(0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        Ok(self.build_element(focused_ref, None, 0, &count, &deadline, MAX_ELEMENTS))
    }

    fn start_observing(&mut self) -> Result<(), AccessibilityError> {
        let pid = self.get_focused_app_pid()?;
        self.spawn_observer(pid)?;
        tracing::info!("AXObserver started for PID {}", pid);
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<AccessibilityEvent> {
        if let Ok(mut events) = self.events.lock() {
            std::mem::take(&mut *events)
        } else {
            vec![]
        }
    }

    fn stop_observing(&mut self) {
        self.stop_observer();
        tracing::info!("AXObserver stopped");
    }

    fn perform_action(&self, element_id: &str, action: &str) -> Result<bool, AccessibilityError> {
        let pid = self.get_focused_app_pid()?;
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return Err(AccessibilityError::QueryFailed("Failed to create app element".into()));
        }

        // Get the target element — prefer focused window, fall back to app
        let window_cf = get_ax_attribute(app, "AXFocusedWindow");
        let target = window_cf
            .as_ref()
            .map(|w| w.as_CFTypeRef() as AXUIElementRef)
            .unwrap_or(app);

        let mut count = 0;
        let result = self.find_and_perform_action(target, element_id, action, 0, &mut count);

        unsafe { CFRelease(app as *const c_void) };

        match result {
            Ok(true) => {
                tracing::info!("AXPerformAction '{}' on element '{}'", action, element_id);
                Ok(true)
            }
            Ok(false) => {
                Err(AccessibilityError::NotFound(format!(
                    "Element '{}' not found in accessibility tree", element_id
                )))
            }
            Err(e) => Err(e),
        }
    }

    fn set_value(&self, element_id: &str, value: &str) -> Result<bool, AccessibilityError> {
        let pid = self.get_focused_app_pid()?;
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return Err(AccessibilityError::QueryFailed("Failed to create app element".into()));
        }

        let window_cf = get_ax_attribute(app, "AXFocusedWindow");
        let target = window_cf
            .as_ref()
            .map(|w| w.as_CFTypeRef() as AXUIElementRef)
            .unwrap_or(app);

        let mut count = 0;
        let result = find_and_set_value(target, element_id, value, 0, &mut count);

        unsafe { CFRelease(app as *const c_void) };

        match result {
            Ok(true) => {
                tracing::info!("AXSetAttributeValue on element '{}' = '{}'", element_id, value);
                Ok(true)
            }
            Ok(false) => {
                Err(AccessibilityError::NotFound(format!(
                    "Element '{}' not found in accessibility tree", element_id
                )))
            }
            Err(e) => Err(e),
        }
    }

    fn is_settable(&self, element_id: &str) -> bool {
        let pid = match self.get_focused_app_pid() {
            Ok(pid) => pid,
            Err(_) => return false,
        };
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return false;
        }

        let window_cf = get_ax_attribute(app, "AXFocusedWindow");
        let target = window_cf
            .as_ref()
            .map(|w| w.as_CFTypeRef() as AXUIElementRef)
            .unwrap_or(app);

        let mut count = 0;
        let result = find_and_check_settable(target, element_id, 0, &mut count);

        unsafe { CFRelease(app as *const c_void) };

        result.unwrap_or(false)
    }

    fn element_at_position(&self, x: f32, y: f32) -> Result<Option<AccessibilityElement>, AccessibilityError> {
        let system_wide = unsafe { AXUIElementCreateSystemWide() };
        if system_wide.is_null() {
            return Ok(None);
        }

        let mut element_ref: AXUIElementRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyElementAtPosition(system_wide, x, y, &mut element_ref)
        };
        unsafe { CFRelease(system_wide as *const c_void) };

        if err != K_AX_ERROR_SUCCESS || element_ref.is_null() {
            return Ok(None);
        }

        let count = AtomicUsize::new(0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let result = self.build_element(element_ref, None, 0, &count, &deadline, MAX_ELEMENTS);

        unsafe { CFRelease(element_ref as *const c_void) };

        Ok(result)
    }

    fn get_menu_bar(&self) -> Result<Vec<crate::tree::MenuBarItem>, AccessibilityError> {
        let pid = self.get_focused_app_pid()?;
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return Ok(vec![]);
        }

        let mut items = Vec::new();
        if let Some(menu_bar_cf) = get_ax_attribute(app, "AXMenuBar") {
            let menu_bar_ref = menu_bar_cf.as_CFTypeRef() as AXUIElementRef;
            if let Some(children_cf) = get_ax_attribute(menu_bar_ref, "AXChildren") {
                let children_ref = children_cf.as_CFTypeRef();
                if unsafe { core_foundation::array::CFArrayGetTypeID() }
                    == unsafe { core_foundation::base::CFGetTypeID(children_ref) }
                {
                    let arr: CFArray<CFType> = unsafe {
                        CFArray::wrap_under_get_rule(children_ref as core_foundation::array::CFArrayRef)
                    };
                    for i in 0..arr.len() {
                        if let Some(top_menu) = arr.get(i) {
                            let top_ref = top_menu.as_CFTypeRef() as AXUIElementRef;
                            let top_label = get_ax_string(top_ref, "AXTitle")
                                .unwrap_or_else(|| "?".to_string());
                            // Traverse submenu items
                            extract_menu_items(top_ref, &top_label, &mut items, 0);
                        }
                    }
                }
            }
        }

        unsafe { CFRelease(app as *const c_void) };
        Ok(items)
    }

    fn get_all_windows(&self) -> Result<Vec<AccessibilityElement>, AccessibilityError> {
        let pid = self.get_focused_app_pid()?;
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return Ok(vec![]);
        }

        let mut windows = Vec::new();
        if let Some(windows_cf) = get_ax_attribute(app, "AXWindows") {
            let windows_ref = windows_cf.as_CFTypeRef();
            if unsafe { core_foundation::array::CFArrayGetTypeID() }
                == unsafe { core_foundation::base::CFGetTypeID(windows_ref) }
            {
                let arr: CFArray<CFType> = unsafe {
                    CFArray::wrap_under_get_rule(windows_ref as core_foundation::array::CFArrayRef)
                };
                for i in 0..arr.len() {
                    if let Some(win) = arr.get(i) {
                        let win_ref = win.as_CFTypeRef() as AXUIElementRef;
                        let count = AtomicUsize::new(0);
                        // Shallow tree (depth 2) for each window — just enough for title/bounds
                        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
                        if let Some(elem) = self.build_element(win_ref, None, 0, &count, &deadline, MAX_ELEMENTS) {
                            windows.push(elem);
                        }
                    }
                }
            }
        }

        unsafe { CFRelease(app as *const c_void) };
        Ok(windows)
    }
}

/// Recursively extract menu items into a flat list with path prefixes.
fn extract_menu_items(
    element: AXUIElementRef,
    path_prefix: &str,
    items: &mut Vec<crate::tree::MenuBarItem>,
    depth: usize,
) {
    if depth > 5 || element.is_null() {
        return; // Prevent infinite recursion in deeply nested menus
    }

    // Get the submenu children
    if let Some(children_cf) = get_ax_attribute(element, "AXChildren") {
        let children_ref = children_cf.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() }
            == unsafe { core_foundation::base::CFGetTypeID(children_ref) }
        {
            let arr: CFArray<CFType> = unsafe {
                CFArray::wrap_under_get_rule(children_ref as core_foundation::array::CFArrayRef)
            };
            for i in 0..arr.len() {
                if let Some(child) = arr.get(i) {
                    let child_ref = child.as_CFTypeRef() as AXUIElementRef;
                    let role = get_ax_string(child_ref, "AXRole").unwrap_or_default();

                    if role == "AXMenu" {
                        // This is a submenu container — recurse into it
                        extract_menu_items(child_ref, path_prefix, items, depth + 1);
                    } else if role == "AXMenuItem" {
                        let label = get_ax_string(child_ref, "AXTitle").unwrap_or_default();
                        if label.is_empty() {
                            continue; // Skip separators
                        }
                        let enabled = get_ax_bool(child_ref, "AXEnabled").unwrap_or(true);
                        let shortcut = get_ax_string(child_ref, "AXMenuItemCmdChar")
                            .and_then(|key| {
                                if key.is_empty() {
                                    return None;
                                }
                                // Read modifier keys
                                let modifiers = get_ax_string(child_ref, "AXMenuItemCmdModifiers");
                                let mod_str = match modifiers.as_deref() {
                                    Some("0") | None => "⌘",
                                    Some("1") => "⇧⌘",
                                    Some("2") => "⌥⌘",
                                    Some("3") => "⇧⌥⌘",
                                    Some("4") => "⌃⌘",
                                    _ => "⌘",
                                };
                                Some(format!("{}{}", mod_str, key))
                            });

                        let path = format!("{} > {}", path_prefix, label);
                        items.push(crate::tree::MenuBarItem {
                            path,
                            label: label.clone(),
                            shortcut,
                            enabled,
                        });

                        // Check if this menu item has a submenu
                        if let Some(submenu_cf) = get_ax_attribute(child_ref, "AXChildren") {
                            let sub_ref = submenu_cf.as_CFTypeRef();
                            if unsafe { core_foundation::array::CFArrayGetTypeID() }
                                == unsafe { core_foundation::base::CFGetTypeID(sub_ref) }
                            {
                                let sub_arr: CFArray<CFType> = unsafe {
                                    CFArray::wrap_under_get_rule(sub_ref as core_foundation::array::CFArrayRef)
                                };
                                for j in 0..sub_arr.len() {
                                    if let Some(sub_child) = sub_arr.get(j) {
                                        let sub_child_ref = sub_child.as_CFTypeRef() as AXUIElementRef;
                                        let sub_path = format!("{} > {}", path_prefix, label);
                                        extract_menu_items(sub_child_ref, &sub_path, items, depth + 1);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- Helper functions for set_value / is_settable tree traversal ---

/// Walk the AX tree to find an element by ID and set its AXValue attribute.
fn find_and_set_value(
    element: AXUIElementRef,
    target_id: &str,
    value: &str,
    depth: usize,
    count: &mut usize,
) -> Result<bool, AccessibilityError> {
    if depth >= MAX_TREE_DEPTH || *count >= MAX_ELEMENTS || element.is_null() {
        return Ok(false);
    }
    *count += 1;

    // Recompute the content-hash ID using the same logic as build_element
    let role_str = get_ax_string(element, "AXRole").unwrap_or_default();
    let label = get_ax_string(element, "AXTitle")
        .or_else(|| get_ax_string(element, "AXDescription"))
        .or_else(|| get_ax_string(element, "AXHelp"));

    let id = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        role_str.hash(&mut hasher);
        label.hash(&mut hasher);
        // NOTE: bounds (x/y/width/height) are intentionally NOT hashed.
        // See corresponding comment in build_element and find_and_perform_action.
        depth.hash(&mut hasher);
        (*count).hash(&mut hasher);
        format!("ax:{:016x}", hasher.finish())
    };

    // Match found — check settable, then set the value
    if id == target_id {
        let attr_cf = CFString::new("AXValue");
        let mut settable = false;
        let err = unsafe {
            AXUIElementIsAttributeSettable(element, attr_cf.as_concrete_TypeRef(), &mut settable)
        };
        if err != K_AX_ERROR_SUCCESS || !settable {
            return Err(AccessibilityError::OperationFailed(
                "AXValue is not settable on this element".into(),
            ));
        }

        // Determine value type based on role
        let set_err = if matches!(role_str.as_str(), "AXCheckBox" | "AXRadioButton") {
            // Boolean: parse "true"/"false" or "1"/"0"
            let bool_val = match value {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => {
                    return Err(AccessibilityError::OperationFailed(format!(
                        "Invalid boolean value '{}' for checkbox/radio", value
                    )));
                }
            };
            let cf_bool = if bool_val {
                CFBoolean::true_value()
            } else {
                CFBoolean::false_value()
            };
            unsafe {
                AXUIElementSetAttributeValue(
                    element,
                    attr_cf.as_concrete_TypeRef(),
                    cf_bool.as_CFTypeRef(),
                )
            }
        } else if let Ok(num) = value.parse::<f64>() {
            // Try numeric for sliders, progress indicators, etc.
            if matches!(role_str.as_str(), "AXSlider" | "AXScrollBar" | "AXValueIndicator") {
                let cf_num = CFNumber::from(num);
                unsafe {
                    AXUIElementSetAttributeValue(
                        element,
                        attr_cf.as_concrete_TypeRef(),
                        cf_num.as_CFTypeRef(),
                    )
                }
            } else {
                // For text fields, even if it looks numeric, set as string
                let cf_str = CFString::new(value);
                unsafe {
                    AXUIElementSetAttributeValue(
                        element,
                        attr_cf.as_concrete_TypeRef(),
                        cf_str.as_CFTypeRef(),
                    )
                }
            }
        } else {
            // Text: set as CFString
            let cf_str = CFString::new(value);
            unsafe {
                AXUIElementSetAttributeValue(
                    element,
                    attr_cf.as_concrete_TypeRef(),
                    cf_str.as_CFTypeRef(),
                )
            }
        };

        return if set_err == K_AX_ERROR_SUCCESS {
            Ok(true)
        } else {
            Err(AccessibilityError::OperationFailed(format!(
                "AXUIElementSetAttributeValue failed with error {}", set_err
            )))
        };
    }

    // Not found — recurse into children
    if let Some(kids_cf) = get_ax_attribute(element, "AXChildren") {
        let kids_ref = kids_cf.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() }
            == unsafe { core_foundation::base::CFGetTypeID(kids_ref) }
        {
            let arr: CFArray<CFType> = unsafe {
                CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef)
            };
            for i in 0..arr.len() {
                if *count >= MAX_ELEMENTS {
                    break;
                }
                if let Some(child_ref) = arr.get(i).map(|c| c.as_CFTypeRef() as AXUIElementRef) {
                    match find_and_set_value(child_ref, target_id, value, depth + 1, count) {
                        Ok(true) => return Ok(true),
                        Err(e) => return Err(e),
                        Ok(false) => {} // Keep searching
                    }
                }
            }
        }
    }

    Ok(false)
}

/// Walk the AX tree to find an element by ID and check if its AXValue is settable.
fn find_and_check_settable(
    element: AXUIElementRef,
    target_id: &str,
    depth: usize,
    count: &mut usize,
) -> Option<bool> {
    if depth >= MAX_TREE_DEPTH || *count >= MAX_ELEMENTS || element.is_null() {
        return None;
    }
    *count += 1;

    let role_str = get_ax_string(element, "AXRole").unwrap_or_default();
    let label = get_ax_string(element, "AXTitle")
        .or_else(|| get_ax_string(element, "AXDescription"))
        .or_else(|| get_ax_string(element, "AXHelp"));

    let id = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        role_str.hash(&mut hasher);
        label.hash(&mut hasher);
        // NOTE: bounds (x/y/width/height) are intentionally NOT hashed.
        // See corresponding comment in build_element and find_and_perform_action.
        depth.hash(&mut hasher);
        (*count).hash(&mut hasher);
        format!("ax:{:016x}", hasher.finish())
    };

    if id == target_id {
        let attr_cf = CFString::new("AXValue");
        let mut settable = false;
        let err = unsafe {
            AXUIElementIsAttributeSettable(element, attr_cf.as_concrete_TypeRef(), &mut settable)
        };
        return Some(err == K_AX_ERROR_SUCCESS && settable);
    }

    // Recurse into children
    if let Some(kids_cf) = get_ax_attribute(element, "AXChildren") {
        let kids_ref = kids_cf.as_CFTypeRef();
        if unsafe { core_foundation::array::CFArrayGetTypeID() }
            == unsafe { core_foundation::base::CFGetTypeID(kids_ref) }
        {
            let arr: CFArray<CFType> = unsafe {
                CFArray::wrap_under_get_rule(kids_ref as core_foundation::array::CFArrayRef)
            };
            for i in 0..arr.len() {
                if *count >= MAX_ELEMENTS {
                    break;
                }
                if let Some(child_ref) = arr.get(i).map(|c| c.as_CFTypeRef() as AXUIElementRef) {
                    if let Some(result) = find_and_check_settable(child_ref, target_id, depth + 1, count) {
                        return Some(result);
                    }
                }
            }
        }
    }

    None
}

// --- Helper functions ---

/// Get an AX attribute as a CFType.
fn get_ax_attribute(element: AXUIElementRef, attr: &str) -> Option<CFType> {
    let attr_cf = CFString::new(attr);
    let mut value: CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
    };
    if err != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }
    // We own the value (Copy rule), wrap it
    Some(unsafe { CFType::wrap_under_create_rule(value) })
}

/// Get an AX attribute as a String.
fn get_ax_string(element: AXUIElementRef, attr: &str) -> Option<String> {
    let cf = get_ax_attribute(element, attr)?;
    let cf_ref = cf.as_CFTypeRef();
    // Check if it's a CFString
    if unsafe { core_foundation::string::CFStringGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        let s: CFString = unsafe { CFString::wrap_under_get_rule(cf_ref as CFStringRef) };
        let result = s.to_string();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    } else if unsafe { core_foundation::number::CFNumberGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        // Sometimes AXValue is a number (e.g., checkbox state)
        let n: CFNumber = unsafe {
            CFNumber::wrap_under_get_rule(cf_ref as core_foundation::number::CFNumberRef)
        };
        n.to_f64().map(|v| v.to_string())
    } else {
        None
    }
}

/// Get an AX attribute as a bool.
fn get_ax_bool(element: AXUIElementRef, attr: &str) -> Option<bool> {
    let cf = get_ax_attribute(element, attr)?;
    let cf_ref = cf.as_CFTypeRef();
    if unsafe { core_foundation::boolean::CFBooleanGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        let b: CFBoolean = unsafe {
            CFBoolean::wrap_under_get_rule(cf_ref as core_foundation::boolean::CFBooleanRef)
        };
        Some(b == CFBoolean::true_value())
    } else {
        // Sometimes it's a number 0/1
        get_ax_string(element, attr).and_then(|s| match s.as_str() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        })
    }
}

fn get_ax_array_len(element: AXUIElementRef, attr: &str) -> Option<usize> {
    let cf = get_ax_attribute(element, attr)?;
    let cf_ref = cf.as_CFTypeRef();
    if unsafe { core_foundation::array::CFArrayGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        let arr: CFArray<CFType> = unsafe {
            CFArray::wrap_under_get_rule(cf_ref as core_foundation::array::CFArrayRef)
        };
        Some(arr.len() as usize)
    } else {
        None
    }
}

/// Get element bounds from AXPosition + AXSize.
fn get_ax_bounds(element: AXUIElementRef) -> Option<Bounds> {
    // AXPosition and AXSize return AXValue types that wrap CGPoint/CGSize
    let pos_cf = get_ax_attribute(element, "AXPosition")?;
    let size_cf = get_ax_attribute(element, "AXSize")?;

    let mut point = core_graphics::geometry::CGPoint::new(0.0, 0.0);
    let mut size = core_graphics::geometry::CGSize::new(0.0, 0.0);

    let pos_ok = unsafe {
        AXValueGetValue(
            pos_cf.as_CFTypeRef() as AXValueRef,
            AX_VALUE_TYPE_CG_POINT,
            &mut point as *mut _ as *mut c_void,
        )
    };
    let size_ok = unsafe {
        AXValueGetValue(
            size_cf.as_CFTypeRef() as AXValueRef,
            AX_VALUE_TYPE_CG_SIZE,
            &mut size as *mut _ as *mut c_void,
        )
    };

    if pos_ok && size_ok {
        let scale = get_display_scale_factor();
        Some(Bounds {
            x: (point.x * scale) as i32,
            y: (point.y * scale) as i32,
            width: (size.width * scale).max(1.0) as u32,
            height: (size.height * scale).max(1.0) as u32,
        })
    } else {
        None
    }
}

/// FAST state extraction — skips redundant AXHidden (already checked in build_element).
/// Saves 1 FFI call per element. Also skips AXSelected for most roles.
fn get_ax_state_fast(element: AXUIElementRef, role: &str) -> ElementState {
    // AXHidden already checked in build_element early exit — element is visible
    let enabled = get_ax_bool(element, "AXEnabled").unwrap_or(true);
    let focused = get_ax_bool(element, "AXFocused").unwrap_or(false);

    // Only query selected for roles that support selection
    let selected = if matches!(role, "AXRow" | "AXCell" | "AXMenuItem" | "AXListItem" | "AXTabButton" | "AXRadioButton") {
        get_ax_bool(element, "AXSelected").unwrap_or(false)
    } else {
        false
    };

    // Expanded — only for disclosure/outline/group
    let expanded = if matches!(role, "AXDisclosureTriangle" | "AXOutline" | "AXGroup") {
        get_ax_bool(element, "AXExpanded")
    } else {
        None
    };

    // Checked — only for checkboxes/radio buttons
    let checked = if role == "AXCheckBox" || role == "AXRadioButton" {
        get_ax_string(element, "AXValue").and_then(|v| match v.as_str() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        })
    } else {
        None
    };

    ElementState {
        focused,
        enabled,
        visible: true, // already filtered by build_element
        selected,
        expanded,
        checked,
    }
}

/// Check if a role is interactive (worth querying actions for).
fn is_interactive_role(role: &str) -> bool {
    matches!(role,
        "AXButton" | "AXLink" | "AXTextField" | "AXTextArea" | "AXSearchField"
        | "AXSecureTextField" | "AXCheckBox" | "AXRadioButton" | "AXPopUpButton"
        | "AXComboBox" | "AXSlider" | "AXMenuItem" | "AXMenuBarItem" | "AXMenuButton"
        | "AXTab" | "AXTabButton" | "AXDisclosureTriangle" | "AXIncrementor"
        | "AXColorWell" | "AXDateField" | "AXToolbar" | "AXStepper"
        | "AXSplitGroup" | "AXImage" | "AXRow" | "AXCell"
    )
}

/// Get available actions for an element.
fn get_ax_actions(element: AXUIElementRef) -> Vec<String> {
    let mut names_ref: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyActionNames(element, &mut names_ref) };
    if err != K_AX_ERROR_SUCCESS || names_ref.is_null() {
        return vec![];
    }

    let arr: CFArray<CFType> = unsafe {
        CFArray::wrap_under_create_rule(names_ref as core_foundation::array::CFArrayRef)
    };

    let mut actions = Vec::new();
    for i in 0..arr.len() {
        if let Some(item) = arr.get(i) {
            let cf_ref = item.as_CFTypeRef();
            if unsafe { core_foundation::string::CFStringGetTypeID() }
                == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
            {
                let s: CFString = unsafe { CFString::wrap_under_get_rule(cf_ref as CFStringRef) };
                let action = s.to_string();
                // Map AX action names to our convention
                let mapped = match action.as_str() {
                    "AXPress" => "click",
                    "AXConfirm" => "activate",
                    "AXIncrement" => "increment",
                    "AXDecrement" => "decrement",
                    "AXCancel" => "cancel",
                    "AXShowMenu" => "show_menu",
                    "AXScrollToVisible" => "scroll_to_visible",
                    "AXRaise" => "raise",
                    "AXPick" => "pick",
                    "AXDelete" => "delete",
                    _ => continue,
                };
                actions.push(mapped.to_string());
            }
        }
    }
    actions
}

/// Map AX role string to ElementRole.
fn map_role(role: &str, subrole: Option<&str>) -> ElementRole {
    match role {
        "AXButton" => ElementRole::Button,
        "AXMenuButton" => ElementRole::Button,
        "AXTextField" | "AXTextArea" | "AXSearchField" | "AXSecureTextField" => ElementRole::Input,
        "AXStaticText" => ElementRole::Text,
        "AXWindow" => ElementRole::Window,
        "AXList" | "AXTable" => {
            if role == "AXTable" {
                ElementRole::Table
            } else {
                ElementRole::List
            }
        }
        "AXRow" => ElementRole::TableRow,
        "AXCell" => ElementRole::TableCell,
        "AXMenu" | "AXMenuBar" | "AXMenuBarItem" => ElementRole::Menu,
        "AXMenuItem" => ElementRole::MenuItem,
        "AXCheckBox" => ElementRole::Checkbox,
        "AXRadioButton" => ElementRole::RadioButton,
        "AXComboBox" | "AXPopUpButton" => ElementRole::ComboBox,
        "AXSlider" => ElementRole::Slider,
        "AXScrollBar" | "AXScrollArea" => ElementRole::ScrollBar,
        "AXTabGroup" => ElementRole::Tab,
        "AXTabButton" => ElementRole::TabItem,
        "AXRadioGroup" => ElementRole::Group,
        "AXOutline" => ElementRole::TreeView,
        "AXDisclosureTriangle" => ElementRole::TreeItem,
        "AXToolbar" => ElementRole::Toolbar,
        "AXGroup" => {
            // Check subrole for more specific types
            match subrole {
                Some("AXTabPanel") => ElementRole::TabItem,
                Some("AXContentList") => ElementRole::List,
                _ => ElementRole::Group,
            }
        }
        "AXImage" => ElementRole::Image,
        "AXLink" => ElementRole::Link,
        "AXSheet" | "AXDialog" => ElementRole::Dialog,
        "AXStatusBar" | "AXValueIndicator" => ElementRole::StatusBar,
        "AXWebArea" | "AXLayoutArea" => ElementRole::Group,
        _ => ElementRole::Custom(role.to_string()),
    }
}

/// Recursively find elements matching criteria.
fn find_in_tree(
    element: &AccessibilityElement,
    role: Option<&ElementRole>,
    label: Option<&str>,
    results: &mut Vec<AccessibilityElement>,
) {
    let role_match = role.map_or(true, |r| std::mem::discriminant(&element.role) == std::mem::discriminant(r));
    let label_match = label.map_or(true, |l| {
        element.label.as_deref().map_or(false, |el| el.to_lowercase().contains(&l.to_lowercase()))
    });

    if role_match && label_match {
        results.push(element.clone());
    }

    for child in &element.children {
        find_in_tree(child, role, label, results);
    }
}

// --- AXValue FFI for CGPoint/CGSize extraction ---

type AXValueRef = *const c_void;
const AX_VALUE_TYPE_CG_POINT: i32 = 1;
const AX_VALUE_TYPE_CG_SIZE: i32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXValueGetValue(value: AXValueRef, value_type: i32, out: *mut c_void) -> bool;
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_role_basic() {
        assert!(matches!(map_role("AXButton", None), ElementRole::Button));
        assert!(matches!(map_role("AXTextField", None), ElementRole::Input));
        assert!(matches!(map_role("AXTextArea", None), ElementRole::Input));
        assert!(matches!(map_role("AXStaticText", None), ElementRole::Text));
        assert!(matches!(map_role("AXWindow", None), ElementRole::Window));
        assert!(matches!(map_role("AXCheckBox", None), ElementRole::Checkbox));
        assert!(matches!(map_role("AXRadioButton", None), ElementRole::RadioButton));
        assert!(matches!(map_role("AXSlider", None), ElementRole::Slider));
        assert!(matches!(map_role("AXLink", None), ElementRole::Link));
        assert!(matches!(map_role("AXImage", None), ElementRole::Image));
        assert!(matches!(map_role("AXDialog", None), ElementRole::Dialog));
        assert!(matches!(map_role("AXSheet", None), ElementRole::Dialog));
        assert!(matches!(map_role("AXToolbar", None), ElementRole::Toolbar));
        assert!(matches!(map_role("AXOutline", None), ElementRole::TreeView));
    }

    #[test]
    fn test_map_role_with_subrole() {
        assert!(matches!(map_role("AXGroup", Some("AXTabPanel")), ElementRole::TabItem));
        assert!(matches!(map_role("AXGroup", Some("AXContentList")), ElementRole::List));
        assert!(matches!(map_role("AXGroup", None), ElementRole::Group));
    }

    #[test]
    fn test_map_role_unknown() {
        match map_role("AXSomethingNew", None) {
            ElementRole::Custom(s) => assert_eq!(s, "AXSomethingNew"),
            _ => panic!("Expected Custom variant"),
        }
    }

    #[test]
    fn test_map_role_table_and_dialog_variants() {
        assert!(matches!(map_role("AXRow", None), ElementRole::TableRow));
        assert!(matches!(map_role("AXCell", None), ElementRole::TableCell));
        assert!(matches!(map_role("AXTabButton", None), ElementRole::TabItem));
        assert!(matches!(map_role("AXDialog", None), ElementRole::Dialog));
    }

    #[test]
    fn test_find_in_tree() {
        let tree = AccessibilityElement {
            id: "root".into(),
            role: ElementRole::Window,
            label: Some("Test Window".into()),
            description: None,
            value: None,
            bounds: None,
            state: ElementState::default_visible(),
            parent_id: None,
            actions: vec![],
            properties: std::collections::HashMap::new(),
            children: vec![
                AccessibilityElement {
                    id: "btn1".into(),
                    role: ElementRole::Button,
                    label: Some("OK".into()),
                    description: None,
                    value: None,
                    bounds: None,
                    state: ElementState::default_visible(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
                AccessibilityElement {
                    id: "btn2".into(),
                    role: ElementRole::Button,
                    label: Some("Cancel".into()),
                    description: None,
                    value: None,
                    bounds: None,
                    state: ElementState::default_visible(),
                    parent_id: Some("root".into()),
                    actions: vec![],
                    properties: std::collections::HashMap::new(),
                    children: vec![],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let mut results = Vec::new();
        find_in_tree(&tree, Some(&ElementRole::Button), None, &mut results);
        assert_eq!(results.len(), 2);

        let mut results = Vec::new();
        find_in_tree(&tree, None, Some("OK"), &mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "btn1");
    }

    #[test]
    #[ignore] // Requires Accessibility permission — run manually
    fn live_macos_accessibility() {
        let mac = MacAccessibility::new().expect("Accessibility permission required");
        let tree = mac.get_tree().expect("Failed to get tree");
        assert!(!tree.id.is_empty());
        // Should have some children from the focused window
        println!("Root: {:?} - children: {}", tree.label, tree.children.len());
        for child in &tree.children {
            println!(
                "  {:?} {:?} {:?}",
                child.role, child.label, child.bounds
            );
        }
    }
}
