//! Trackpad gesture observation via CGEventTap (macOS).
//!
//! Observes pinch, swipe, rotate, and smart-zoom gestures performed by the user.
//! Events are stored semantically as GestureEvent (from cel-input) for recording/replay.
//! The replay layer maps these to keyboard equivalents — this module only observes.

use cel_input::GestureEvent;
use std::sync::{Arc, Mutex};

/// Accumulated gesture events from observation.
type GestureBuffer = Arc<Mutex<Vec<GestureEvent>>>;

/// A running gesture observer that captures trackpad gestures.
pub struct GestureObserver {
    buffer: GestureBuffer,
    #[cfg(target_os = "macos")]
    running: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    _handle: Option<std::thread::JoinHandle<()>>,
}

impl GestureObserver {
    /// Start observing trackpad gestures. Observation runs on a background thread.
    /// Requires accessibility permissions on macOS for CGEventTap.
    pub fn start() -> Result<Self, String> {
        let buffer: GestureBuffer = Arc::new(Mutex::new(Vec::new()));

        #[cfg(target_os = "macos")]
        {
            let buf_clone = buffer.clone();
            let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let run_clone = running.clone();

            let handle = std::thread::spawn(move || {
                macos_gesture_tap(buf_clone, run_clone);
            });

            Ok(Self {
                buffer,
                running,
                _handle: Some(handle),
            })
        }

        #[cfg(not(target_os = "macos"))]
        {
            tracing::warn!("Gesture observation not available on this platform");
            Ok(Self { buffer })
        }
    }

    /// Create a no-op observer that never captures anything.
    /// Used as a fallback when the real observer can't start (e.g., missing permissions).
    pub fn no_op() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            #[cfg(target_os = "macos")]
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(target_os = "macos")]
            _handle: None,
        }
    }

    /// Drain all accumulated gesture events since last call.
    pub fn drain(&self) -> Vec<GestureEvent> {
        if let Ok(mut buf) = self.buffer.lock() {
            std::mem::take(&mut *buf)
        } else {
            vec![]
        }
    }

    /// Stop observing gestures.
    pub fn stop(&self) {
        #[cfg(target_os = "macos")]
        {
            self.running.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// --- macOS CGEventTap implementation ---

#[cfg(target_os = "macos")]
mod macos_ffi {
    use std::ffi::c_void;

    pub type CGEventRef = *const c_void;
    pub type CGEventTapProxy = *const c_void;
    pub type CFMachPortRef = *const c_void;
    pub type CFRunLoopSourceRef = *const c_void;
    pub type CFRunLoopRef = *const c_void;
    pub type CFStringRef = *const c_void;

    pub type CGEventType = u32;
    pub type CGEventField = u32;
    pub type CGEventMask = u64;

    // Gesture event types (not in public headers, but stable since macOS 10.6)
    pub const K_CG_EVENT_GESTURE: CGEventType = 29;
    pub const K_CG_EVENT_MAGNIFY: CGEventType = 30;
    pub const K_CG_EVENT_SWIPE: CGEventType = 31;
    pub const K_CG_EVENT_ROTATE: CGEventType = 32;
    pub const K_CG_EVENT_SMART_MAGNIFY: CGEventType = 34;

    // CGEventField for gesture data
    pub const K_CG_GESTURE_MAGNIFICATION: CGEventField = 113;
    pub const K_CG_GESTURE_ROTATION: CGEventField = 114;
    pub const K_CG_GESTURE_SWIPE_DIRECTION: CGEventField = 117;

    pub type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventTapCreate(
            tap: u32,          // CGEventTapLocation
            place: u32,        // CGEventTapPlacement
            options: u32,      // CGEventTapOptions
            events_of_interest: CGEventMask,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> CFMachPortRef;

        pub fn CGEventGetDoubleValueField(event: CGEventRef, field: CGEventField) -> f64;
        pub fn CGEventGetIntegerValueField(event: CGEventRef, field: CGEventField) -> i64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CFMachPortRef,
            order: i64,
        ) -> CFRunLoopSourceRef;
        pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after: bool) -> i32;
        pub fn CFRelease(cf: *const c_void);

        pub static kCFRunLoopDefaultMode: CFStringRef;
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn gesture_callback(
    _proxy: macos_ffi::CGEventTapProxy,
    event_type: macos_ffi::CGEventType,
    event: macos_ffi::CGEventRef,
    user_info: *mut std::ffi::c_void,
) -> macos_ffi::CGEventRef {
    use macos_ffi::*;

    let buffer = &*(user_info as *const Mutex<Vec<GestureEvent>>);

    let gesture = match event_type {
        K_CG_EVENT_MAGNIFY => {
            let scale = CGEventGetDoubleValueField(event, K_CG_GESTURE_MAGNIFICATION);
            Some(GestureEvent::PinchZoom { scale })
        }
        K_CG_EVENT_SWIPE => {
            let dir_val = CGEventGetIntegerValueField(event, K_CG_GESTURE_SWIPE_DIRECTION);
            let direction = match dir_val {
                1 => "up",
                2 => "down",
                4 => "left",
                8 => "right",
                _ => "unknown",
            };
            Some(GestureEvent::Swipe {
                direction: direction.to_string(),
                finger_count: 2,
            })
        }
        K_CG_EVENT_ROTATE => {
            let angle = CGEventGetDoubleValueField(event, K_CG_GESTURE_ROTATION);
            Some(GestureEvent::Rotate { angle_degrees: angle })
        }
        K_CG_EVENT_SMART_MAGNIFY => Some(GestureEvent::SmartZoom),
        _ => None,
    };

    if let Some(g) = gesture {
        if let Ok(mut buf) = buffer.lock() {
            if buf.len() < 500 {
                buf.push(g);
            }
        }
    }

    // Return the event unchanged (passive observation)
    event
}

#[cfg(target_os = "macos")]
fn macos_gesture_tap(
    buffer: GestureBuffer,
    running: Arc<std::sync::atomic::AtomicBool>,
) {
    use macos_ffi::*;
    use std::ffi::c_void;

    let mask: CGEventMask = (1u64 << K_CG_EVENT_MAGNIFY)
        | (1u64 << K_CG_EVENT_SWIPE)
        | (1u64 << K_CG_EVENT_ROTATE)
        | (1u64 << K_CG_EVENT_SMART_MAGNIFY)
        | (1u64 << K_CG_EVENT_GESTURE);

    // Leak the buffer Arc so it lives as long as the tap.
    // We'll never reclaim this — the observer thread runs until the process exits.
    let buffer_ptr = Arc::into_raw(buffer) as *mut c_void;

    let tap = unsafe {
        CGEventTapCreate(
            0,  // kCGSessionEventTap
            0,  // kCGHeadInsertEventTap
            1,  // kCGEventTapOptionListenOnly (passive — don't modify events)
            mask,
            gesture_callback,
            buffer_ptr,
        )
    };

    if tap.is_null() {
        tracing::warn!("Failed to create gesture event tap — need accessibility permission");
        // Reclaim the leaked Arc
        unsafe { let _ = Arc::from_raw(buffer_ptr as *const Mutex<Vec<GestureEvent>>); }
        return;
    }

    unsafe {
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if source.is_null() {
            tracing::warn!("Failed to create run loop source for gesture tap");
            CFRelease(tap);
            let _ = Arc::from_raw(buffer_ptr as *const Mutex<Vec<GestureEvent>>);
            return;
        }

        let rl = CFRunLoopGetCurrent();
        CFRunLoopAddSource(rl, source, kCFRunLoopDefaultMode);

        while running.load(std::sync::atomic::Ordering::Relaxed) {
            let result = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, true);
            // 2 = kCFRunLoopRunTimedOut, 4 = kCFRunLoopRunHandledSource
            if result != 2 && result != 4 {
                break;
            }
        }

        CFRelease(source);
        CFRelease(tap);
        // Note: we intentionally don't reclaim buffer_ptr here because
        // the callback may still be in-flight during shutdown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gesture_event_serialization() {
        let events = vec![
            GestureEvent::PinchZoom { scale: 1.5 },
            GestureEvent::Swipe { direction: "left".into(), finger_count: 2 },
            GestureEvent::Rotate { angle_degrees: 45.0 },
            GestureEvent::SmartZoom,
            GestureEvent::MomentumScroll { dx: 0.0, dy: -120.0 },
        ];
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _back: GestureEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn test_gesture_observer_drain_empty() {
        let buffer: GestureBuffer = Arc::new(Mutex::new(Vec::new()));
        let observer = GestureObserver {
            buffer,
            #[cfg(target_os = "macos")]
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(target_os = "macos")]
            _handle: None,
        };
        assert!(observer.drain().is_empty());
    }
}
