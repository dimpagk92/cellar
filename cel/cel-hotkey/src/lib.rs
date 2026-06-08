//! CEL global hotkeys — register system-wide keyboard shortcuts that trigger
//! CEL actions.
//!
//! Thin wrapper over the [`global-hotkey`] crate: it parses human combo strings
//! (`"cmd+shift+k"`), registers them, and runs a blocking loop that invokes a
//! callback whenever a registered hotkey is pressed. The caller decides what a
//! press does — fire a `cel_act`, kick off a `run_goal`, run a shell command.
//!
//! ## Positioning
//!
//! Global hotkeys are a *human-convenience* surface — a person pressing a key to
//! trigger an automation — rather than the agent-first path that is CEL's
//! centre of gravity. It ships because it rounds out desktop-automation parity,
//! but the governed agent loop remains the primary interface.
//!
//! ## Platform note
//!
//! macOS delivers hotkey events on the thread's run loop, so [`HotkeyRegistry::run`]
//! pumps it. The registry should be created and run on the same thread. On a
//! strict main-thread host (a GUI app) prefer driving registration from the app
//! event loop; for a CLI the dedicated-thread loop here is sufficient for
//! interactive use.

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use std::collections::HashMap;
use std::str::FromStr;

/// Errors from hotkey parsing / registration.
#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    /// The combo string could not be parsed into modifiers + a key.
    #[error("invalid hotkey combo: {0}")]
    InvalidCombo(String),
    /// The OS hotkey manager could not register / initialize.
    #[error("hotkey manager error: {0}")]
    Manager(String),
}

/// Result alias for hotkey operations.
pub type Result<T> = std::result::Result<T, HotkeyError>;

/// Parse a combo string like `"cmd+shift+k"` into a [`HotKey`].
///
/// Modifiers: `cmd`/`command`/`super`/`win`, `shift`, `alt`/`option`,
/// `ctrl`/`control`. The key is a single letter/digit, a function key
/// (`f1`..`f24`), or a named key (`space`, `enter`, `escape`, `up`, …). At least
/// one non-modifier key is required.
pub fn parse_combo(combo: &str) -> Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in combo.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" | "win" => mods |= Modifiers::SUPER,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" | "option" | "opt" => mods |= Modifiers::ALT,
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            other => {
                if code.is_some() {
                    return Err(HotkeyError::InvalidCombo(format!(
                        "{combo} (more than one non-modifier key)"
                    )));
                }
                code = Some(
                    key_to_code(other)
                        .ok_or_else(|| HotkeyError::InvalidCombo(combo.to_string()))?,
                );
            }
        }
    }
    let code = code.ok_or_else(|| HotkeyError::InvalidCombo(format!("{combo} (no key)")))?;
    let mods = if mods.is_empty() { None } else { Some(mods) };
    Ok(HotKey::new(mods, code))
}

/// Map a human key name to a [`Code`] via its `KeyboardEvent.code` spelling.
fn key_to_code(key: &str) -> Option<Code> {
    // Named keys → their Code spelling.
    let named = match key {
        "space" => "Space",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "escape" | "esc" => "Escape",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "minus" => "Minus",
        "equal" | "equals" => "Equal",
        "comma" => "Comma",
        "period" | "dot" => "Period",
        "slash" => "Slash",
        _ => "",
    };
    if !named.is_empty() {
        return Code::from_str(named).ok();
    }
    if key.len() == 1 {
        let c = key.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Code::from_str(&format!("Key{}", c.to_ascii_uppercase())).ok();
        }
        if c.is_ascii_digit() {
            return Code::from_str(&format!("Digit{c}")).ok();
        }
    }
    // Function keys f1..f24.
    if let Some(n) = key.strip_prefix('f') {
        if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            return Code::from_str(&format!("F{n}")).ok();
        }
    }
    None
}

/// A set of registered global hotkeys plus the OS manager that owns them.
pub struct HotkeyRegistry {
    manager: GlobalHotKeyManager,
    by_id: HashMap<u32, String>,
}

impl HotkeyRegistry {
    /// Create the OS hotkey manager. On macOS, call this on the thread whose
    /// run loop [`run`](Self::run) will pump.
    pub fn new() -> Result<Self> {
        Ok(Self {
            manager: GlobalHotKeyManager::new().map_err(|e| HotkeyError::Manager(e.to_string()))?,
            by_id: HashMap::new(),
        })
    }

    /// Register a combo string. Returns the hotkey id passed back to the
    /// [`run`](Self::run) callback.
    pub fn register(&mut self, combo: &str) -> Result<u32> {
        let hk = parse_combo(combo)?;
        self.manager
            .register(hk)
            .map_err(|e| HotkeyError::Manager(e.to_string()))?;
        self.by_id.insert(hk.id(), combo.to_string());
        Ok(hk.id())
    }

    /// The combo string a hotkey id maps to, if registered.
    pub fn combo_for(&self, id: u32) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }

    /// How many hotkeys are registered.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether no hotkeys are registered.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Block, pumping the platform event loop, invoking `on_press(id, combo)`
    /// each time a registered hotkey is pressed. Never returns (interrupt the
    /// process to stop).
    pub fn run<F: FnMut(u32, &str)>(&self, mut on_press: F) {
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            pump_event_loop();
            while let Ok(ev) = receiver.try_recv() {
                if ev.state == HotKeyState::Pressed {
                    if let Some(combo) = self.by_id.get(&ev.id) {
                        on_press(ev.id, combo);
                    }
                }
            }
        }
    }
}

/// Pump the platform event loop briefly so hotkey events are delivered.
#[cfg(target_os = "macos")]
fn pump_event_loop() {
    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
    // Returns after ~100ms or when an event is handled, whichever is first.
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn pump_event_loop() {
    std::thread::sleep(std::time::Duration::from_millis(50));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_letter() {
        // Just assert it parses without error and yields a stable id.
        let a = parse_combo("cmd+shift+k").unwrap();
        let b = parse_combo("  Command + Shift + K ").unwrap();
        assert_eq!(a.id(), b.id(), "case/space-insensitive, same hotkey");
    }

    #[test]
    fn parses_named_and_function_keys() {
        assert!(parse_combo("ctrl+space").is_ok());
        assert!(parse_combo("alt+f4").is_ok());
        assert!(parse_combo("cmd+up").is_ok());
        assert!(parse_combo("cmd+5").is_ok());
    }

    #[test]
    fn distinct_combos_have_distinct_ids() {
        let k = parse_combo("cmd+k").unwrap();
        let j = parse_combo("cmd+j").unwrap();
        assert_ne!(k.id(), j.id());
    }

    #[test]
    fn rejects_empty_and_modifier_only_and_double_key() {
        assert!(matches!(parse_combo(""), Err(HotkeyError::InvalidCombo(_))));
        assert!(matches!(
            parse_combo("cmd+shift"),
            Err(HotkeyError::InvalidCombo(_))
        ));
        assert!(matches!(
            parse_combo("cmd+k+j"),
            Err(HotkeyError::InvalidCombo(_))
        ));
        assert!(matches!(
            parse_combo("cmd+notakey"),
            Err(HotkeyError::InvalidCombo(_))
        ));
    }
}
