use crate::inject::{InputController, InputError, MouseButton};
use enigo::{
    Axis, Button, Coordinate, Direction, Enigo, Keyboard as EnigoKeyboard, Mouse as EnigoMouse,
    Settings,
};
use std::thread;
use std::time::Duration;

/// Cross-platform input controller using enigo.
pub struct EnigoInput {
    enigo: Enigo,
}

// SAFETY: EnigoInput is always accessed behind a Mutex (see cel-napi OnceLock<Mutex<>>).
// Enigo's CGEventSource on macOS isn't Sync, but we guarantee single-threaded access via Mutex.
unsafe impl Sync for EnigoInput {}

impl EnigoInput {
    pub fn new() -> Result<Self, InputError> {
        let settings = Settings::default();
        let enigo = Enigo::new(&settings).map_err(|e| InputError::Failed(e.to_string()))?;
        Ok(Self { enigo })
    }
}

fn map_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}

/// Map a key name string to an enigo Key.
fn parse_key(key: &str) -> Result<enigo::Key, InputError> {
    match key.to_lowercase().as_str() {
        "enter" | "return" => Ok(enigo::Key::Return),
        "tab" => Ok(enigo::Key::Tab),
        "escape" | "esc" => Ok(enigo::Key::Escape),
        "backspace" => Ok(enigo::Key::Backspace),
        "delete" => Ok(enigo::Key::Delete),
        "space" => Ok(enigo::Key::Space),
        "up" | "uparrow" | "arrowup" | "up arrow" | "up_arrow" | "arrow up" => {
            Ok(enigo::Key::UpArrow)
        }
        "down" | "downarrow" | "arrowdown" | "down arrow" | "down_arrow" | "arrow down" => {
            Ok(enigo::Key::DownArrow)
        }
        "left" | "leftarrow" | "arrowleft" | "left arrow" | "left_arrow" | "arrow left" => {
            Ok(enigo::Key::LeftArrow)
        }
        "right" | "rightarrow" | "arrowright" | "right arrow" | "right_arrow" | "arrow right" => {
            Ok(enigo::Key::RightArrow)
        }
        "home" => Ok(enigo::Key::Home),
        "end" => Ok(enigo::Key::End),
        "pageup" => Ok(enigo::Key::PageUp),
        "pagedown" => Ok(enigo::Key::PageDown),
        "f1" => Ok(enigo::Key::F1),
        "f2" => Ok(enigo::Key::F2),
        "f3" => Ok(enigo::Key::F3),
        "f4" => Ok(enigo::Key::F4),
        "f5" => Ok(enigo::Key::F5),
        "f6" => Ok(enigo::Key::F6),
        "f7" => Ok(enigo::Key::F7),
        "f8" => Ok(enigo::Key::F8),
        "f9" => Ok(enigo::Key::F9),
        "f10" => Ok(enigo::Key::F10),
        "f11" => Ok(enigo::Key::F11),
        "f12" => Ok(enigo::Key::F12),
        "ctrl" | "control" => Ok(enigo::Key::Control),
        "alt" => Ok(enigo::Key::Alt),
        "shift" => Ok(enigo::Key::Shift),
        "meta" | "super" | "win" | "command" | "cmd" => Ok(enigo::Key::Meta),
        s if s.len() == 1 => Ok(enigo::Key::Unicode(s.chars().next().unwrap())),
        other => Err(InputError::InvalidKey(other.to_string())),
    }
}

impl InputController for EnigoInput {
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        self.enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError> {
        self.mouse_move(x, y)?;
        // Allow macOS event system to register cursor position before clicking.
        // Without this delay, transient UI (Spotlight, menus) may not receive the click.
        // The drag() method uses 50ms for the same reason; 10ms suffices for click.
        thread::sleep(Duration::from_millis(10));
        self.enigo
            .button(map_button(button), Direction::Click)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn double_click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError> {
        self.mouse_move(x, y)?;
        thread::sleep(Duration::from_millis(10));
        let btn = map_button(button);
        self.enigo
            .button(btn, Direction::Click)
            .map_err(|e| InputError::Failed(e.to_string()))?;
        self.enigo
            .button(btn, Direction::Click)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn type_text(&mut self, text: &str) -> Result<(), InputError> {
        self.enigo
            .text(text)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn key_press(&mut self, key: &str) -> Result<(), InputError> {
        let k = parse_key(key)?;
        self.enigo
            .key(k, Direction::Click)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn key_combo(&mut self, keys: &[&str]) -> Result<(), InputError> {
        let parsed: Vec<enigo::Key> = keys
            .iter()
            .map(|k| parse_key(k))
            .collect::<Result<_, _>>()?;

        // Track which keys were pressed so we can release them all on error
        let mut pressed = Vec::with_capacity(parsed.len());
        let mut press_err = None;

        for k in &parsed {
            match self.enigo.key(*k, Direction::Press) {
                Ok(()) => pressed.push(*k),
                Err(e) => {
                    press_err = Some(InputError::Failed(e.to_string()));
                    break;
                }
            }
        }

        // Always release all pressed keys, even if a press or release fails
        for k in pressed.iter().rev() {
            let _ = self.enigo.key(*k, Direction::Release);
        }

        match press_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        if dy != 0 {
            self.enigo
                .scroll(dy, Axis::Vertical)
                .map_err(|e| InputError::Failed(e.to_string()))?;
        }
        if dx != 0 {
            self.enigo
                .scroll(dx, Axis::Horizontal)
                .map_err(|e| InputError::Failed(e.to_string()))?;
        }
        Ok(())
    }

    fn mouse_position(&self) -> Result<(i32, i32), InputError> {
        enigo::Mouse::location(&self.enigo).map_err(|e| InputError::Failed(e.to_string()))
    }

    fn drag(&mut self, from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Result<(), InputError> {
        self.mouse_move(from_x, from_y)?;

        self.enigo
            .button(Button::Left, Direction::Press)
            .map_err(|e| InputError::Failed(e.to_string()))?;

        thread::sleep(Duration::from_millis(50));

        let move_result = self.mouse_move(to_x, to_y);

        // Always release the button, even if the move failed
        let release_result = self
            .enigo
            .button(Button::Left, Direction::Release)
            .map_err(|e| InputError::Failed(e.to_string()));

        // Return the first error encountered
        move_result?;
        release_result
    }

    fn display_size(&self) -> Result<(i32, i32), InputError> {
        let (w, h) = enigo::Mouse::main_display(&self.enigo)
            .map_err(|e| InputError::Failed(e.to_string()))?;
        Ok((w, h))
    }

    fn triple_click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), InputError> {
        self.mouse_move(x, y)?;
        thread::sleep(Duration::from_millis(10));
        let btn = map_button(button);
        for _ in 0..3 {
            self.enigo
                .button(btn, Direction::Click)
                .map_err(|e| InputError::Failed(e.to_string()))?;
        }
        Ok(())
    }

    fn key_down(&mut self, key: &str) -> Result<(), InputError> {
        let k = parse_key(key)?;
        self.enigo
            .key(k, Direction::Press)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn key_up(&mut self, key: &str) -> Result<(), InputError> {
        let k = parse_key(key)?;
        self.enigo
            .key(k, Direction::Release)
            .map_err(|e| InputError::Failed(e.to_string()))
    }

    fn paste(&mut self) -> Result<(), InputError> {
        #[cfg(target_os = "macos")]
        {
            self.key_combo(&["cmd", "v"])
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.key_combo(&["ctrl", "v"])
        }
    }

    fn select_all(&mut self) -> Result<(), InputError> {
        #[cfg(target_os = "macos")]
        {
            self.key_combo(&["cmd", "a"])
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.key_combo(&["ctrl", "a"])
        }
    }

    fn mouse_move_smooth(&mut self, x: i32, y: i32, duration_ms: u32) -> Result<(), InputError> {
        if duration_ms == 0 {
            return self.mouse_move(x, y);
        }

        let (start_x, start_y) = self.mouse_position()?;
        let steps = (duration_ms / 10).clamp(5, 100) as i32; // 10ms per step, 5-100 steps
        let sleep_per_step = Duration::from_millis((duration_ms as u64) / (steps as u64));

        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            // Ease-in-out cubic: smoother start and stop
            let ease = if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0_f64).powi(3) / 2.0
            };
            let cx = start_x + ((x - start_x) as f64 * ease) as i32;
            let cy = start_y + ((y - start_y) as f64 * ease) as i32;
            self.mouse_move(cx, cy)?;
            thread::sleep(sleep_per_step);
        }

        // Ensure we land exactly on target
        self.mouse_move(x, y)
    }
}
