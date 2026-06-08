//! Background (non-focus-stealing) input — WS1 of the Peekaboo-parity plan.
//!
//! Posts CGEvents directly to a target process via `CGEventPostToPid`
//! instead of the session-wide tap used by [`crate::EnigoInput`]. The
//! target app receives mouse and keyboard events **without being brought
//! to the foreground**, so the user's active window keeps focus.
//!
//! Caveats — not every macOS app honors background-delivered events. Some
//! only process input while they hold key-window focus (notably certain
//! games and Electron apps that gate on `NSApp.isActive`). Callers (the
//! cortex dispatcher) treat any failure here as a signal to fall back to
//! the foreground activate-then-type path. This module never activates an
//! app; that policy lives one layer up so the fallback stays observable in
//! the receipt.

use crate::inject::{InputError, MouseButton};

#[cfg(target_os = "macos")]
mod imp {
    use super::{InputError, MouseButton};
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, CGMouseButton, EventField};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;
    use foreign_types::ForeignType;
    use std::os::raw::c_void;

    // core-graphics 0.24 exposes `CGEvent::post` (session/HID taps) but not
    // `CGEventPostToPid`. Bind the symbol directly — it's stable public
    // CoreGraphics, and the framework is already linked by the crate.
    extern "C" {
        fn CGEventPostToPid(pid: libc::pid_t, event: *const c_void);
    }

    /// Deliver `event` to `pid`'s event queue without activating the app.
    #[inline]
    fn post(event: &CGEvent, pid: i32) {
        // SAFETY: `event` is a live CGEvent for the duration of the call;
        // CGEventPostToPid copies it into the target process's queue and
        // does not take ownership of the pointer.
        unsafe { CGEventPostToPid(pid as libc::pid_t, event.as_ptr() as *const c_void) };
    }

    fn source() -> Result<CGEventSource, InputError> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| InputError::Failed("failed to create CGEventSource".into()))
    }

    fn cg_button(button: MouseButton) -> CGMouseButton {
        match button {
            MouseButton::Left => CGMouseButton::Left,
            MouseButton::Right => CGMouseButton::Right,
            MouseButton::Middle => CGMouseButton::Center,
        }
    }

    fn down_up_types(button: MouseButton) -> (CGEventType, CGEventType) {
        match button {
            MouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
            MouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
            MouseButton::Middle => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
        }
    }

    /// Click `clicks` times at window-global `(x, y)`, delivered to `pid`.
    /// `clicks` >= 2 encodes a double/triple click via the click-state field.
    pub fn click(
        pid: i32,
        x: i32,
        y: i32,
        button: MouseButton,
        clicks: u32,
    ) -> Result<(), InputError> {
        let point = CGPoint::new(x as f64, y as f64);
        let (down, up) = down_up_types(button);
        let cg_btn = cg_button(button);
        for n in 1..=clicks.max(1) {
            let ev_down = CGEvent::new_mouse_event(source()?, down, point, cg_btn)
                .map_err(|_| InputError::Failed("mouse-down event".into()))?;
            ev_down.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, n as i64);
            post(&ev_down, pid);

            let ev_up = CGEvent::new_mouse_event(source()?, up, point, cg_btn)
                .map_err(|_| InputError::Failed("mouse-up event".into()))?;
            ev_up.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, n as i64);
            post(&ev_up, pid);
        }
        Ok(())
    }

    /// Type a unicode string into `pid`. keycode 0 + an attached string is
    /// the standard "type arbitrary text" recipe — the keycode is ignored
    /// when a string is set, so this handles CJK/emoji without a keymap.
    pub fn type_text(pid: i32, text: &str) -> Result<(), InputError> {
        let down = CGEvent::new_keyboard_event(source()?, 0, true)
            .map_err(|_| InputError::Failed("keyboard event".into()))?;
        down.set_string(text);
        post(&down, pid);

        let up = CGEvent::new_keyboard_event(source()?, 0, false)
            .map_err(|_| InputError::Failed("keyboard event".into()))?;
        up.set_string(text);
        post(&up, pid);
        Ok(())
    }

    /// Press and release a single named key (e.g. "Enter", "Tab", "a").
    pub fn key_press(pid: i32, key: &str) -> Result<(), InputError> {
        let keycode = keycode_for(key)?;
        press_keycode(pid, keycode, CGEventFlags::empty())
    }

    /// Press a key combination (e.g. ["cmd", "v"]). All but the last
    /// non-modifier token become modifier flags on the main keystroke.
    pub fn key_combo(pid: i32, keys: &[&str]) -> Result<(), InputError> {
        let mut flags = CGEventFlags::empty();
        let mut main: Option<u16> = None;
        for k in keys {
            if let Some(f) = modifier_flag(k) {
                flags |= f;
            } else {
                main = Some(keycode_for(k)?);
            }
        }
        let keycode = main
            .ok_or_else(|| InputError::InvalidKey("key combo without a non-modifier key".into()))?;
        press_keycode(pid, keycode, flags)
    }

    fn press_keycode(pid: i32, keycode: u16, flags: CGEventFlags) -> Result<(), InputError> {
        let down = CGEvent::new_keyboard_event(source()?, keycode, true)
            .map_err(|_| InputError::Failed("keyboard event".into()))?;
        if !flags.is_empty() {
            down.set_flags(flags);
        }
        post(&down, pid);

        let up = CGEvent::new_keyboard_event(source()?, keycode, false)
            .map_err(|_| InputError::Failed("keyboard event".into()))?;
        if !flags.is_empty() {
            up.set_flags(flags);
        }
        post(&up, pid);
        Ok(())
    }

    fn modifier_flag(key: &str) -> Option<CGEventFlags> {
        match key.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" | "win" => Some(CGEventFlags::CGEventFlagCommand),
            "shift" => Some(CGEventFlags::CGEventFlagShift),
            "alt" | "option" => Some(CGEventFlags::CGEventFlagAlternate),
            "ctrl" | "control" => Some(CGEventFlags::CGEventFlagControl),
            _ => None,
        }
    }

    /// macOS ANSI virtual keycodes for the keys CEL commonly dispatches.
    fn keycode_for(key: &str) -> Result<u16, InputError> {
        let k = key.to_ascii_lowercase();
        let code = match k.as_str() {
            "return" | "enter" => 36,
            "tab" => 48,
            "space" => 49,
            "delete" | "backspace" => 51,
            "escape" | "esc" => 53,
            "forwarddelete" => 117,
            "home" => 115,
            "end" => 119,
            "pageup" => 116,
            "pagedown" => 121,
            "left" | "leftarrow" => 123,
            "right" | "rightarrow" => 124,
            "down" | "downarrow" => 125,
            "up" | "uparrow" => 126,
            s if s.chars().count() == 1 => ansi_char_keycode(s.chars().next().unwrap())
                .ok_or_else(|| InputError::InvalidKey(key.to_string()))?,
            other => return Err(InputError::InvalidKey(other.to_string())),
        };
        Ok(code)
    }

    /// ANSI keyboard layout virtual keycodes (Carbon `kVK_ANSI_*`).
    fn ansi_char_keycode(c: char) -> Option<u16> {
        Some(match c.to_ascii_lowercase() {
            'a' => 0,
            's' => 1,
            'd' => 2,
            'f' => 3,
            'h' => 4,
            'g' => 5,
            'z' => 6,
            'x' => 7,
            'c' => 8,
            'v' => 9,
            'b' => 11,
            'q' => 12,
            'w' => 13,
            'e' => 14,
            'r' => 15,
            'y' => 16,
            't' => 17,
            '1' => 18,
            '2' => 19,
            '3' => 20,
            '4' => 21,
            '6' => 22,
            '5' => 23,
            '=' => 24,
            '9' => 25,
            '7' => 26,
            '-' => 27,
            '8' => 28,
            '0' => 29,
            ']' => 30,
            'o' => 31,
            'u' => 32,
            '[' => 33,
            'i' => 34,
            'p' => 35,
            'l' => 37,
            'j' => 38,
            '\'' => 39,
            'k' => 40,
            ';' => 41,
            '\\' => 42,
            ',' => 43,
            '/' => 44,
            'n' => 45,
            'm' => 46,
            '.' => 47,
            '`' => 50,
            _ => return None,
        })
    }

    /// Cheap proxy for "can we post events at all": a usable HID event
    /// source. The authoritative grant (AXIsProcessTrusted) is probed by
    /// `cellar doctor`; this just guards against a hard FFI failure.
    pub fn available() -> bool {
        source().is_ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn known_special_keys_map() {
            assert_eq!(keycode_for("Enter").unwrap(), 36);
            assert_eq!(keycode_for("escape").unwrap(), 53);
            assert_eq!(keycode_for("Tab").unwrap(), 48);
            assert_eq!(keycode_for("a").unwrap(), 0);
            assert_eq!(keycode_for("V").unwrap(), 9);
        }

        #[test]
        fn unknown_key_is_invalid() {
            assert!(matches!(keycode_for("f13"), Err(InputError::InvalidKey(_))));
        }

        #[test]
        fn modifiers_parse_case_insensitively() {
            assert_eq!(modifier_flag("Cmd"), Some(CGEventFlags::CGEventFlagCommand));
            assert_eq!(
                modifier_flag("CONTROL"),
                Some(CGEventFlags::CGEventFlagControl)
            );
            assert_eq!(modifier_flag("a"), None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{InputError, MouseButton};

    pub fn click(_: i32, _: i32, _: i32, _: MouseButton, _: u32) -> Result<(), InputError> {
        Err(InputError::Unavailable)
    }
    pub fn type_text(_: i32, _: &str) -> Result<(), InputError> {
        Err(InputError::Unavailable)
    }
    pub fn key_press(_: i32, _: &str) -> Result<(), InputError> {
        Err(InputError::Unavailable)
    }
    pub fn key_combo(_: i32, _: &[&str]) -> Result<(), InputError> {
        Err(InputError::Unavailable)
    }
    pub fn available() -> bool {
        false
    }
}

pub use imp::{available, click, key_combo, key_press, type_text};
