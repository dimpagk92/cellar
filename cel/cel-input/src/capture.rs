//! Input capture — observe (not inject) system-wide keyboard and mouse events.
//!
//! This is the perception counterpart to [`crate::inject`]. On macOS it runs a
//! CGEventTap in listen-only mode on a dedicated CFRunLoop thread; observed
//! events are buffered and drained by the caller (e.g. the cortex tick loop),
//! mirroring the `drain_events` contract of `cel_audio::AudioCapture` and
//! `cel_network::NetworkMonitor`.
//!
//! PRIVACY: keystroke CONTENT (typed characters) is only ever populated when
//! `capture_chars` is enabled at construction. Keycodes and pointer events
//! carry no typed text. The macOS tap requires Input Monitoring permission;
//! without it `start()` returns an error.

use crate::{InputError, MouseButton};

/// A single observed input event. Pointer coordinates are global (screen)
/// points. `KeyDown::chars` is `Some` only when content capture is enabled.
#[derive(Debug, Clone, PartialEq)]
pub enum CapturedInput {
    /// A key was pressed. `chars` holds the typed text only when content
    /// capture is enabled (otherwise `None`).
    KeyDown { keycode: u16, chars: Option<String> },
    /// A key was released.
    KeyUp { keycode: u16 },
    /// The pointer moved (or dragged) to a new global position.
    MouseMoved { x: f64, y: f64 },
    /// A mouse button changed state at a global position.
    MouseButton {
        button: MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
    },
    /// A scroll wheel event with vertical (`dy`) and horizontal (`dx`) deltas.
    Scroll { dx: i64, dy: i64 },
}

/// Observe system input. Implementations must keep `drain_events` non-blocking.
pub trait InputCapture: Send + Sync {
    /// Begin capturing. Idempotent — a second call while running is a no-op.
    fn start(&mut self) -> Result<(), InputError>;
    /// Stop capturing and release any OS resources / threads.
    fn stop(&mut self) -> Result<(), InputError>;
    /// Take everything observed since the last call.
    fn drain_events(&mut self) -> Vec<CapturedInput>;
    /// Whether a capture session is currently active.
    fn is_running(&self) -> bool;
}

/// No-op / test capture: lets tests inject events without a real event tap,
/// and is the fallback on non-macOS platforms.
#[derive(Default)]
pub struct StubInputCapture {
    running: bool,
    pending: Vec<CapturedInput>,
}

impl StubInputCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an event to be returned by the next `drain_events`.
    pub fn push(&mut self, ev: CapturedInput) {
        self.pending.push(ev);
    }
}

impl InputCapture for StubInputCapture {
    fn start(&mut self) -> Result<(), InputError> {
        self.running = true;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), InputError> {
        self.running = false;
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<CapturedInput> {
        std::mem::take(&mut self.pending)
    }
    fn is_running(&self) -> bool {
        self.running
    }
}

/// Create a platform input capture. On macOS this is a CGEventTap capture
/// (requires Input Monitoring permission); on other platforms it is a stub.
///
/// `capture_chars` enables extraction of typed characters into
/// [`CapturedInput::KeyDown::chars`]. Leave it `false` unless the caller has a
/// governance reason to observe keystroke content.
pub fn create_input_capture(capture_chars: bool) -> Box<dyn InputCapture> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::CGEventTapCapture::new(capture_chars))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = capture_chars;
        Box::new(StubInputCapture::new())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{CapturedInput, InputCapture};
    use crate::{InputError, MouseButton};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
    use core_graphics::event::{
        CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        EventField,
    };
    use foreign_types::ForeignType;

    /// Cap on buffered events so a capture with no draining consumer (e.g. no
    /// daemon bridge) can't grow without bound.
    const MAX_BUFFERED: usize = 4096;

    pub struct CGEventTapCapture {
        capture_chars: bool,
        running: Arc<AtomicBool>,
        buffer: Arc<Mutex<Vec<CapturedInput>>>,
        /// Count of events tail-dropped because the buffer hit `MAX_BUFFERED`
        /// (no consumer draining). Surfaced via a throttled warn so the loss
        /// isn't silent.
        dropped: Arc<AtomicU64>,
        thread: Option<JoinHandle<()>>,
    }

    impl CGEventTapCapture {
        pub fn new(capture_chars: bool) -> Self {
            Self {
                capture_chars,
                running: Arc::new(AtomicBool::new(false)),
                buffer: Arc::new(Mutex::new(Vec::new())),
                dropped: Arc::new(AtomicU64::new(0)),
                thread: None,
            }
        }
    }

    impl InputCapture for CGEventTapCapture {
        fn start(&mut self) -> Result<(), InputError> {
            if self.running.load(Ordering::SeqCst) {
                return Ok(());
            }
            // Join any handle left over from a prior failed start so a denied
            // permission followed by a retry can't leak threads.
            if let Some(h) = self.thread.take() {
                let _ = h.join();
            }
            self.running.store(true, Ordering::SeqCst);
            let running = Arc::clone(&self.running);
            let buffer = Arc::clone(&self.buffer);
            let dropped = Arc::clone(&self.dropped);
            let capture_chars = self.capture_chars;

            // The thread reports whether the tap actually came up, so `start()`
            // returns a real error (e.g. Input Monitoring not granted) instead
            // of silently pretending to capture.
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);

            // The CGEventTap and its CFRunLoop must be created and pumped on the
            // SAME thread, so we own a dedicated one for the capture lifetime.
            let handle = std::thread::Builder::new()
                .name("cel-input-capture".into())
                .spawn(move || {
                    let buf_for_cb = Arc::clone(&buffer);
                    let dropped_for_cb = Arc::clone(&dropped);
                    let tap = CGEventTap::new(
                        CGEventTapLocation::Session,
                        CGEventTapPlacement::HeadInsertEventTap,
                        CGEventTapOptions::ListenOnly,
                        vec![
                            CGEventType::KeyDown,
                            CGEventType::KeyUp,
                            CGEventType::MouseMoved,
                            CGEventType::LeftMouseDown,
                            CGEventType::LeftMouseUp,
                            CGEventType::RightMouseDown,
                            CGEventType::RightMouseUp,
                            CGEventType::OtherMouseDown,
                            CGEventType::OtherMouseUp,
                            CGEventType::LeftMouseDragged,
                            CGEventType::RightMouseDragged,
                            CGEventType::ScrollWheel,
                        ],
                        move |_proxy, event_type, event| {
                            if let Some(ci) = translate(event_type, event, capture_chars) {
                                if let Ok(mut b) = buf_for_cb.lock() {
                                    if b.len() < MAX_BUFFERED {
                                        b.push(ci);
                                    } else {
                                        // Tail-drop with a throttled warn: the
                                        // callback fires at input frequency, so
                                        // log at most once per MAX_BUFFERED drops.
                                        let n = dropped_for_cb.fetch_add(1, Ordering::Relaxed) + 1;
                                        if n.is_multiple_of(MAX_BUFFERED as u64) {
                                            tracing::warn!(
                                                dropped = n,
                                                "cel-input: capture buffer full; dropping events \
                                                 (no consumer draining?)"
                                            );
                                        }
                                    }
                                }
                            }
                            // ListenOnly — the return value is ignored, but the
                            // signature requires an Option<CGEvent>.
                            None
                        },
                    );

                    let tap = match tap {
                        Ok(t) => t,
                        Err(()) => {
                            running.store(false, Ordering::SeqCst);
                            let _ = ready_tx.send(Err(
                                "CGEventTap creation failed (Input Monitoring permission not \
                                 granted?)"
                                    .into(),
                            ));
                            return;
                        }
                    };

                    let source = match tap.mach_port.create_runloop_source(0) {
                        Ok(s) => s,
                        Err(()) => {
                            running.store(false, Ordering::SeqCst);
                            let _ = ready_tx.send(Err("failed to create runloop source".into()));
                            return;
                        }
                    };

                    let current = CFRunLoop::get_current();
                    unsafe {
                        current.add_source(&source, kCFRunLoopDefaultMode);
                    }
                    tap.enable();

                    // Tap is live — unblock start() before entering the pump loop.
                    let _ = ready_tx.send(Ok(()));

                    // Pump the runloop in short slices so the stop flag is
                    // observed promptly without blocking the thread forever.
                    while running.load(Ordering::SeqCst) {
                        unsafe {
                            CFRunLoop::run_in_mode(
                                kCFRunLoopDefaultMode,
                                std::time::Duration::from_millis(200),
                                false,
                            );
                        }
                    }
                })
                .map_err(|e| InputError::Failed(format!("capture thread spawn: {e}")))?;

            // Block until the tap comes up (or fails) so the return value and
            // `is_running()` reflect reality.
            match ready_rx.recv() {
                Ok(Ok(())) => {
                    self.thread = Some(handle);
                    Ok(())
                }
                Ok(Err(reason)) => {
                    let _ = handle.join();
                    self.running.store(false, Ordering::SeqCst);
                    Err(InputError::Failed(reason))
                }
                Err(_) => {
                    let _ = handle.join();
                    self.running.store(false, Ordering::SeqCst);
                    Err(InputError::Failed(
                        "capture thread exited before signaling readiness".into(),
                    ))
                }
            }
        }

        fn stop(&mut self) -> Result<(), InputError> {
            self.running.store(false, Ordering::SeqCst);
            if let Some(h) = self.thread.take() {
                let _ = h.join();
            }
            Ok(())
        }

        fn drain_events(&mut self) -> Vec<CapturedInput> {
            let mut b = self.buffer.lock().unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut *b)
        }

        fn is_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }
    }

    impl Drop for CGEventTapCapture {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    /// Map a CGEvent to our platform-neutral [`CapturedInput`]. Returns `None`
    /// for event types we don't model.
    fn translate(
        event_type: CGEventType,
        event: &core_graphics::event::CGEvent,
        capture_chars: bool,
    ) -> Option<CapturedInput> {
        use CGEventType as T;
        match event_type {
            T::KeyDown => {
                let keycode =
                    event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                let chars = if capture_chars {
                    event_chars(event)
                } else {
                    None
                };
                Some(CapturedInput::KeyDown { keycode, chars })
            }
            T::KeyUp => {
                let keycode =
                    event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                Some(CapturedInput::KeyUp { keycode })
            }
            T::MouseMoved | T::LeftMouseDragged | T::RightMouseDragged => {
                let p = event.location();
                Some(CapturedInput::MouseMoved { x: p.x, y: p.y })
            }
            T::LeftMouseDown | T::RightMouseDown | T::OtherMouseDown => {
                let p = event.location();
                Some(CapturedInput::MouseButton {
                    button: button_for(event),
                    pressed: true,
                    x: p.x,
                    y: p.y,
                })
            }
            T::LeftMouseUp | T::RightMouseUp | T::OtherMouseUp => {
                let p = event.location();
                Some(CapturedInput::MouseButton {
                    button: button_for(event),
                    pressed: false,
                    x: p.x,
                    y: p.y,
                })
            }
            T::ScrollWheel => {
                let dy = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
                let dx = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
                Some(CapturedInput::Scroll { dx, dy })
            }
            _ => None,
        }
    }

    fn button_for(event: &core_graphics::event::CGEvent) -> MouseButton {
        match event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER) {
            0 => MouseButton::Left,
            1 => MouseButton::Right,
            _ => MouseButton::Middle,
        }
    }

    /// Extract typed characters from a keyboard event. Only called when content
    /// capture is enabled. Returns `None` for control-only keys (arrows, etc.).
    fn event_chars(event: &core_graphics::event::CGEvent) -> Option<String> {
        use std::os::raw::{c_ulong, c_void};
        extern "C" {
            fn CGEventKeyboardGetUnicodeString(
                event: *const c_void,
                max_len: c_ulong,
                actual_len: *mut c_ulong,
                unicode_string: *mut u16,
            );
        }
        let mut buf = [0u16; 8];
        let mut actual: c_ulong = 0;
        unsafe {
            CGEventKeyboardGetUnicodeString(
                event.as_ptr() as *const c_void,
                buf.len() as c_ulong,
                &mut actual,
                buf.as_mut_ptr(),
            );
        }
        let n = actual as usize;
        if n == 0 || n > buf.len() {
            return None;
        }
        let s = String::from_utf16_lossy(&buf[..n]);
        if s.is_empty() || s.chars().all(|c| c.is_control()) {
            None
        } else {
            Some(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_lifecycle_and_drain() {
        let mut cap = StubInputCapture::new();
        assert!(!cap.is_running());
        cap.start().unwrap();
        assert!(cap.is_running());

        cap.push(CapturedInput::KeyDown {
            keycode: 4,
            chars: Some("h".into()),
        });
        cap.push(CapturedInput::MouseMoved { x: 10.0, y: 20.0 });

        let drained = cap.drain_events();
        assert_eq!(drained.len(), 2);
        // Drain is destructive — second drain is empty.
        assert!(cap.drain_events().is_empty());

        cap.stop().unwrap();
        assert!(!cap.is_running());
    }

    #[test]
    fn create_capture_returns_something_usable() {
        // On non-macOS this is the stub; on macOS it's the real tap (not
        // started here). Either way the trait object must be constructible.
        let cap = create_input_capture(false);
        assert!(!cap.is_running());
    }
}
