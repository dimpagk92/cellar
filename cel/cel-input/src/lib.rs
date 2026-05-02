//! CEL Input Layer
//!
//! Input injection and interception for mouse and keyboard events.
//! Uses enigo for cross-platform input simulation (Windows, macOS, Linux).

#[cfg(target_os = "macos")]
pub mod applescript;
mod enigo_input;
mod inject;

#[cfg(target_os = "macos")]
pub use applescript::{read_numbers_cells, write_numbers_cells, CellWrite};
pub use enigo_input::EnigoInput;
pub use inject::{GestureEvent, InputController, InputError, InputEvent, MouseButton};

/// Create a platform-appropriate input controller.
pub fn create_controller() -> Result<Box<dyn InputController>, InputError> {
    Ok(Box::new(EnigoInput::new()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mouse_button_serialization() {
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            let json = serde_json::to_string(&button).unwrap();
            let back: MouseButton = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{:?}", button), format!("{:?}", back));
        }
    }

    #[test]
    fn test_input_event_mouse_move() {
        let event = InputEvent::MouseMove { x: 100, y: 200 };
        let json = serde_json::to_string(&event).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        match back {
            InputEvent::MouseMove { x, y } => {
                assert_eq!(x, 100);
                assert_eq!(y, 200);
            }
            _ => panic!("Expected MouseMove"),
        }
    }

    #[test]
    fn test_input_event_click() {
        let event = InputEvent::MouseClick {
            x: 50,
            y: 75,
            button: MouseButton::Right,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        match back {
            InputEvent::MouseClick { x, y, button } => {
                assert_eq!(x, 50);
                assert_eq!(y, 75);
                assert!(matches!(button, MouseButton::Right));
            }
            _ => panic!("Expected MouseClick"),
        }
    }

    #[test]
    fn test_input_event_key_press() {
        let event = InputEvent::KeyPress {
            key: "Enter".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Enter"));
    }

    #[test]
    fn test_input_event_type_text() {
        let event = InputEvent::TypeText {
            text: "Hello, World!".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Hello, World!"));
    }

    #[test]
    fn test_input_event_scroll() {
        let event = InputEvent::Scroll { dx: -3, dy: 5 };
        let json = serde_json::to_string(&event).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        match back {
            InputEvent::Scroll { dx, dy } => {
                assert_eq!(dx, -3);
                assert_eq!(dy, 5);
            }
            _ => panic!("Expected Scroll"),
        }
    }

    #[test]
    fn test_all_input_event_variants_serializable() {
        let events = vec![
            InputEvent::MouseMove { x: 0, y: 0 },
            InputEvent::MouseClick {
                x: 0,
                y: 0,
                button: MouseButton::Left,
            },
            InputEvent::MouseDown {
                x: 0,
                y: 0,
                button: MouseButton::Left,
            },
            InputEvent::MouseUp {
                x: 0,
                y: 0,
                button: MouseButton::Left,
            },
            InputEvent::KeyPress { key: "a".into() },
            InputEvent::KeyDown {
                key: "Shift".into(),
            },
            InputEvent::KeyUp {
                key: "Shift".into(),
            },
            InputEvent::TypeText {
                text: "test".into(),
            },
            InputEvent::Scroll { dx: 0, dy: 1 },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let _back: InputEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn test_input_error_display() {
        assert_eq!(
            InputError::Unavailable.to_string(),
            "Input injection not available on this platform"
        );
        assert_eq!(
            InputError::InvalidKey("badkey".into()).to_string(),
            "Invalid key: badkey"
        );
        assert_eq!(
            InputError::Failed("some error".into()).to_string(),
            "Input injection failed: some error"
        );
    }
}
