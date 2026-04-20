use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("Input injection not available on this platform")]
    Unavailable,
    #[error("Input injection failed: {0}")]
    Failed(String),
    #[error("Invalid key: {0}")]
    InvalidKey(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// A recorded input event for logging and replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove { x: i32, y: i32 },
    MouseClick { x: i32, y: i32, button: MouseButton },
    MouseDown { x: i32, y: i32, button: MouseButton },
    MouseUp { x: i32, y: i32, button: MouseButton },
    KeyPress { key: String },
    KeyDown { key: String },
    KeyUp { key: String },
    TypeText { text: String },
    Scroll { dx: i32, dy: i32 },
    /// Trackpad gesture observed during recording.
    /// Stored semantically — replay uses keyboard equivalents.
    Gesture { gesture: GestureEvent },
}

/// A trackpad gesture observed via CGEventTap (macOS) or libinput (Linux).
/// These are semantic — they describe WHAT the user did, not raw touch points.
/// The replay layer maps these to keyboard shortcuts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "gesture_type", rename_all = "snake_case")]
pub enum GestureEvent {
    /// Pinch to zoom. scale > 1.0 = zoom in, < 1.0 = zoom out.
    PinchZoom { scale: f64 },
    /// Two-finger swipe. direction: "left", "right", "up", "down".
    Swipe { direction: String, finger_count: u8 },
    /// Two-finger rotation. angle in degrees (positive = clockwise).
    Rotate { angle_degrees: f64 },
    /// Smart zoom (double-tap with two fingers).
    SmartZoom,
    /// Scroll with momentum. dx/dy are accumulated deltas.
    MomentumScroll { dx: f64, dy: f64 },
}

/// Platform-agnostic input controller trait.
pub trait InputController: Send + Sync {
    /// Move the mouse to absolute screen coordinates.
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), InputError>;

    /// Click at absolute screen coordinates.
    fn click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError>;

    /// Double-click at absolute screen coordinates.
    fn double_click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError>;

    /// Type a string of text (uses fast unicode input).
    fn type_text(&mut self, text: &str) -> Result<(), InputError>;

    /// Press and release a single key (e.g., "Enter", "Tab", "Escape").
    fn key_press(&mut self, key: &str) -> Result<(), InputError>;

    /// Press a key combination (e.g., ["Ctrl", "C"]).
    fn key_combo(&mut self, keys: &[&str]) -> Result<(), InputError>;

    /// Scroll at the current mouse position.
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;

    /// Get the current mouse cursor position as (x, y).
    fn mouse_position(&self) -> Result<(i32, i32), InputError>;

    /// Drag from one point to another (left mouse button).
    fn drag(&mut self, from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Result<(), InputError>;

    /// Get the main display size.
    /// Prefer cel-display's resolution() for multi-monitor and DPI-aware queries.
    fn display_size(&self) -> Result<(i32, i32), InputError>;

    /// Triple-click at absolute screen coordinates (selects full line/paragraph).
    fn triple_click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError>;

    /// Press a key down (without releasing). Pair with key_up() for independent modifier control.
    /// Use case: hold Shift, click something, release Shift.
    fn key_down(&mut self, key: &str) -> Result<(), InputError>;

    /// Release a key that was previously pressed with key_down().
    fn key_up(&mut self, key: &str) -> Result<(), InputError>;

    /// Paste from clipboard (Cmd+V on macOS, Ctrl+V on others).
    /// More reliable than type_text() for long strings, special characters, and CJK text.
    fn paste(&mut self) -> Result<(), InputError>;

    /// Select all text in the focused element (Cmd+A on macOS, Ctrl+A on others).
    fn select_all(&mut self) -> Result<(), InputError>;

    /// Move the mouse smoothly from current position to target with human-like interpolation.
    /// `duration_ms`: how long the movement should take (0 = instant, like mouse_move).
    fn mouse_move_smooth(&mut self, x: i32, y: i32, duration_ms: u32) -> Result<(), InputError>;
}
