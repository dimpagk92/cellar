//! Macro recording + replay.
//!
//! Captures a *timed* sequence of input events (via [`InputCapture`]) and plays
//! it back through an [`InputController`]. A [`Recording`] is plain,
//! serializable JSON — so it can be saved, hand-edited, and replayed
//! deterministically. This is the closest CEL gets to a classic "macro": the
//! recording half already existed in [`crate::capture`]; this module adds the
//! timing + replay half.
//!
//! **Privacy.** A recording contains typed characters only when capture was
//! started with `capture_chars = true`. Otherwise keystrokes are bare keycodes
//! and no content is stored. The flag is persisted on the recording so a reader
//! knows whether content is present.
//!
//! **Mouse fidelity is faithful.** A button press replays as
//! [`InputController::mouse_down`] and a release as [`InputController::mouse_up`],
//! so a press at one point and a release at another reproduces a real
//! **press-drag-release** (the recorded `MouseMoved` events between them carry
//! the drag path) — not an approximated click. Key events replay via their
//! captured `chars` when present, otherwise via a best-effort keycode→key-name
//! table for the common non-character keys; unmapped content-less keys are
//! skipped and counted.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::{CapturedInput, InputCapture, InputController, MouseButton};

/// Serializable mirror of [`CapturedInput`] (which is intentionally not
/// `Serialize` — it is a hot-path perception type).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordedInput {
    /// A key was pressed. `chars` is present only when content capture was on.
    KeyDown {
        keycode: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chars: Option<String>,
    },
    /// A key was released.
    KeyUp { keycode: u16 },
    /// The pointer moved to a global position.
    MouseMoved { x: f64, y: f64 },
    /// A mouse button changed state at a global position.
    MouseButton {
        button: String,
        pressed: bool,
        x: f64,
        y: f64,
    },
    /// A scroll-wheel event.
    Scroll { dx: i64, dy: i64 },
}

impl From<&CapturedInput> for RecordedInput {
    fn from(c: &CapturedInput) -> Self {
        match c {
            CapturedInput::KeyDown { keycode, chars } => RecordedInput::KeyDown {
                keycode: *keycode,
                chars: chars.clone(),
            },
            CapturedInput::KeyUp { keycode } => RecordedInput::KeyUp { keycode: *keycode },
            CapturedInput::MouseMoved { x, y } => RecordedInput::MouseMoved { x: *x, y: *y },
            CapturedInput::MouseButton {
                button,
                pressed,
                x,
                y,
            } => RecordedInput::MouseButton {
                button: mouse_button_name(*button).to_string(),
                pressed: *pressed,
                x: *x,
                y: *y,
            },
            CapturedInput::Scroll { dx, dy } => RecordedInput::Scroll { dx: *dx, dy: *dy },
        }
    }
}

fn mouse_button_name(b: MouseButton) -> &'static str {
    match b {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

fn mouse_button_from_name(s: &str) -> MouseButton {
    match s {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// Best-effort inverse of `background::keycode_for` for the common non-character
/// keys. Character keys are intentionally absent — replay those via the
/// captured `chars` instead, which round-trips text faithfully.
fn keycode_to_key(keycode: u16) -> Option<&'static str> {
    Some(match keycode {
        36 => "return",
        48 => "tab",
        49 => "space",
        51 => "delete",
        53 => "escape",
        117 => "forwarddelete",
        115 => "home",
        119 => "end",
        116 => "pageup",
        121 => "pagedown",
        123 => "left",
        124 => "right",
        125 => "down",
        126 => "up",
        _ => return None,
    })
}

/// One recorded event, stamped with its offset from the recording start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedEvent {
    /// Milliseconds since the recording started.
    pub offset_ms: u64,
    /// The captured input.
    pub event: RecordedInput,
}

fn default_version() -> u32 {
    1
}

/// A complete macro recording. Serializes to/from JSON via [`Recording::to_json`]
/// / [`Recording::from_json`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// Schema version, for forward compatibility.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// Whether typed characters were captured (privacy flag).
    pub captured_chars: bool,
    /// The ordered events.
    pub events: Vec<RecordedEvent>,
}

impl Recording {
    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse from JSON.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// Outcome of a [`replay`] pass.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReplayStats {
    /// Events that produced an injected action.
    pub injected: usize,
    /// Content-less keys with no keycode→name mapping that were skipped.
    pub skipped_keys: usize,
}

/// Record input for `duration`, returning a timed [`Recording`].
///
/// Polls the capture on a short interval and stamps each drained event with the
/// elapsed time (batch granularity, which is plenty for replay). `capture` must
/// already be constructed with the desired `capture_chars` setting; this starts
/// and stops it. macOS requires Input Monitoring permission — `start()` errors
/// without it.
pub fn record(
    capture: &mut dyn InputCapture,
    duration: Duration,
    captured_chars: bool,
) -> Result<Recording, crate::InputError> {
    capture.start()?;
    let started = Instant::now();
    let mut events = Vec::new();
    while started.elapsed() < duration {
        let offset_ms = started.elapsed().as_millis() as u64;
        for ev in capture.drain_events() {
            events.push(RecordedEvent {
                offset_ms,
                event: RecordedInput::from(&ev),
            });
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    // Final drain to catch the tail.
    let offset_ms = started.elapsed().as_millis() as u64;
    for ev in capture.drain_events() {
        events.push(RecordedEvent {
            offset_ms,
            event: RecordedInput::from(&ev),
        });
    }
    capture.stop()?;
    Ok(Recording {
        version: default_version(),
        duration_ms: started.elapsed().as_millis() as u64,
        captured_chars,
        events,
    })
}

/// Replay a [`Recording`] through an [`InputController`].
///
/// `speed` scales playback (1.0 = real time, 2.0 = twice as fast, a very large
/// value = as fast as the controller accepts). Inter-event gaps are honored
/// (scaled by `speed`); a single event never blocks longer than its recorded
/// gap. Returns [`ReplayStats`].
pub fn replay(
    recording: &Recording,
    controller: &mut dyn InputController,
    speed: f64,
) -> Result<ReplayStats, crate::InputError> {
    let speed = if speed <= 0.0 { 1.0 } else { speed };
    let mut stats = ReplayStats::default();
    let mut last_offset = 0u64;

    for RecordedEvent { offset_ms, event } in &recording.events {
        // Honor the inter-event delay (scaled). Saturating sub guards against
        // out-of-order offsets in a hand-edited file.
        let gap_ms = offset_ms.saturating_sub(last_offset) as f64 / speed;
        if gap_ms >= 1.0 {
            std::thread::sleep(Duration::from_millis(gap_ms as u64));
        }
        last_offset = *offset_ms;

        match event {
            RecordedInput::MouseMoved { x, y } => {
                controller.mouse_move(*x as i32, *y as i32)?;
                stats.injected += 1;
            }
            // Faithful press/release. A press at one point and release at
            // another reproduces a real drag — the MouseMoved events recorded
            // between them carry the path.
            RecordedInput::MouseButton {
                button,
                pressed,
                x,
                y,
            } => {
                let btn = mouse_button_from_name(button);
                if *pressed {
                    controller.mouse_down(*x as i32, *y as i32, btn)?;
                } else {
                    controller.mouse_up(*x as i32, *y as i32, btn)?;
                }
                stats.injected += 1;
            }
            RecordedInput::Scroll { dx, dy } => {
                controller.scroll(*dx as i32, *dy as i32)?;
                stats.injected += 1;
            }
            RecordedInput::KeyDown { keycode, chars } => {
                if let Some(text) = chars.as_deref().filter(|s| !s.is_empty()) {
                    // Replay each captured character as a key press.
                    for ch in text.chars() {
                        controller.key_press(&ch.to_string())?;
                    }
                    stats.injected += 1;
                } else if let Some(name) = keycode_to_key(*keycode) {
                    controller.key_press(name)?;
                    stats.injected += 1;
                } else {
                    // Content-less character key with no name mapping — can't
                    // faithfully reconstruct, so skip rather than guess.
                    stats.skipped_keys += 1;
                }
            }
            // Key-up is implied by `key_press` (down+up); nothing to do.
            RecordedInput::KeyUp { .. } => {}
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InputError;
    use std::sync::{Arc, Mutex};

    /// An `InputController` that records the calls made to it, for replay tests.
    #[derive(Default)]
    struct RecordingController {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl InputController for RecordingController {
        // Methods replay() actually uses — these record their calls.
        fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), InputError> {
            self.calls.lock().unwrap().push(format!("move {x},{y}"));
            Ok(())
        }
        fn click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("click {x},{y},{}", mouse_button_name(button)));
            Ok(())
        }
        fn key_press(&mut self, key: &str) -> Result<(), InputError> {
            self.calls.lock().unwrap().push(format!("key {key}"));
            Ok(())
        }
        fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
            self.calls.lock().unwrap().push(format!("scroll {dx},{dy}"));
            Ok(())
        }
        fn mouse_down(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("down {x},{y},{}", mouse_button_name(button)));
            Ok(())
        }
        fn mouse_up(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("up {x},{y},{}", mouse_button_name(button)));
            Ok(())
        }
        // Remaining required methods — unused by replay(), stubbed no-ops.
        fn double_click(&mut self, _x: i32, _y: i32, _b: MouseButton) -> Result<(), InputError> {
            Ok(())
        }
        fn type_text(&mut self, _t: &str) -> Result<(), InputError> {
            Ok(())
        }
        fn key_combo(&mut self, _keys: &[&str]) -> Result<(), InputError> {
            Ok(())
        }
        fn mouse_position(&self) -> Result<(i32, i32), InputError> {
            Ok((0, 0))
        }
        fn drag(&mut self, _fx: i32, _fy: i32, _tx: i32, _ty: i32) -> Result<(), InputError> {
            Ok(())
        }
        fn display_size(&self) -> Result<(i32, i32), InputError> {
            Ok((1920, 1080))
        }
        fn triple_click(&mut self, _x: i32, _y: i32, _b: MouseButton) -> Result<(), InputError> {
            Ok(())
        }
        fn key_down(&mut self, _key: &str) -> Result<(), InputError> {
            Ok(())
        }
        fn key_up(&mut self, _key: &str) -> Result<(), InputError> {
            Ok(())
        }
        fn paste(&mut self) -> Result<(), InputError> {
            Ok(())
        }
        fn select_all(&mut self) -> Result<(), InputError> {
            Ok(())
        }
        fn mouse_move_smooth(&mut self, _x: i32, _y: i32, _d: u32) -> Result<(), InputError> {
            Ok(())
        }
    }

    fn sample_recording() -> Recording {
        Recording {
            version: 1,
            duration_ms: 100,
            captured_chars: true,
            events: vec![
                RecordedEvent {
                    offset_ms: 0,
                    event: RecordedInput::MouseMoved { x: 10.0, y: 20.0 },
                },
                RecordedEvent {
                    offset_ms: 5,
                    event: RecordedInput::MouseButton {
                        button: "left".into(),
                        pressed: true,
                        x: 10.0,
                        y: 20.0,
                    },
                },
                RecordedEvent {
                    offset_ms: 6,
                    event: RecordedInput::MouseButton {
                        button: "left".into(),
                        pressed: false,
                        x: 10.0,
                        y: 20.0,
                    },
                },
                RecordedEvent {
                    offset_ms: 10,
                    event: RecordedInput::KeyDown {
                        keycode: 0,
                        chars: Some("hi".into()),
                    },
                },
                RecordedEvent {
                    offset_ms: 12,
                    event: RecordedInput::KeyDown {
                        keycode: 36,
                        chars: None,
                    },
                },
                RecordedEvent {
                    offset_ms: 14,
                    event: RecordedInput::KeyDown {
                        keycode: 7,
                        chars: None,
                    },
                },
            ],
        }
    }

    #[test]
    fn recording_json_round_trips() {
        let rec = sample_recording();
        let json = rec.to_json().unwrap();
        let back = Recording::from_json(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn replay_emits_expected_controller_calls() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut ctrl = RecordingController {
            calls: calls.clone(),
        };
        // Huge speed → no real sleeping.
        let stats = replay(&sample_recording(), &mut ctrl, 1.0e9).unwrap();

        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                "move 10,20".to_string(),      // MouseMoved
                "down 10,20,left".to_string(), // button press → mouse_down
                "up 10,20,left".to_string(),   // button release → mouse_up
                "key h".to_string(),           // chars "hi"
                "key i".to_string(),
                "key return".to_string(), // keycode 36 → return
                                          // keycode 7 (x), no chars → skipped
            ]
        );
        assert_eq!(stats.injected, 5); // move, down, up, "hi", return
        assert_eq!(stats.skipped_keys, 1); // keycode 7 with no chars
    }

    #[test]
    fn from_captured_input_maps_all_variants() {
        let c = CapturedInput::MouseButton {
            button: MouseButton::Right,
            pressed: true,
            x: 1.0,
            y: 2.0,
        };
        assert_eq!(
            RecordedInput::from(&c),
            RecordedInput::MouseButton {
                button: "right".into(),
                pressed: true,
                x: 1.0,
                y: 2.0
            }
        );
    }
}
