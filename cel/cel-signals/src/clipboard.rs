//! Clipboard signal source.
//!
//! macOS: reads NSPasteboard via osascript (avoids AppKit dependency).
//! Linux: reads from xclip/xsel or wl-paste.

use serde::{Deserialize, Serialize};

/// Current clipboard state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardState {
    /// Text content (truncated to 500 chars).
    pub text: Option<String>,
    /// Whether the clipboard contains an image.
    pub has_image: bool,
    /// Whether the clipboard contains file references.
    pub has_files: bool,
}

/// Read current clipboard contents. Returns None if unavailable.
pub fn read_clipboard() -> Option<ClipboardState> {
    #[cfg(target_os = "macos")]
    {
        read_clipboard_macos()
    }
    #[cfg(target_os = "linux")]
    {
        read_clipboard_linux()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn read_clipboard_macos() -> Option<ClipboardState> {
    // Read text via pbpaste (fast, no framework dependency)
    let text = std::process::Command::new("pbpaste")
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                if s.is_empty() {
                    None
                } else {
                    // Truncate to 500 chars
                    Some(if s.len() > 500 {
                        let mut end = 500;
                        while end > 0 && !s.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}...", &s[..end])
                    } else {
                        s
                    })
                }
            } else {
                None
            }
        });

    // Check for image/file types via osascript (lightweight check)
    let type_check = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to return (the clipboard info)",
        ])
        .output()
        .ok();

    let info_str = type_check
        .as_ref()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).to_lowercase())
            } else {
                None
            }
        })
        .unwrap_or_default();

    let has_image =
        info_str.contains("tiff") || info_str.contains("png") || info_str.contains("jpeg");
    let has_files = info_str.contains("file url");

    Some(ClipboardState {
        text,
        has_image,
        has_files,
    })
}

#[cfg(target_os = "linux")]
fn read_clipboard_linux() -> Option<ClipboardState> {
    // Try xclip first, then xsel, then wl-paste (Wayland)
    let text = ["xclip", "xsel", "wl-paste"].iter().find_map(|cmd| {
        let mut command = std::process::Command::new(cmd);
        if *cmd == "xclip" {
            command.args(["-selection", "clipboard", "-o"]);
        } else if *cmd == "xsel" {
            command.args(["--clipboard", "--output"]);
        }
        command.output().ok().and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).to_string();
                if s.is_empty() {
                    None
                } else {
                    Some(if s.len() > 500 {
                        let mut end = 500;
                        while end > 0 && !s.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}...", &s[..end])
                    } else {
                        s
                    })
                }
            } else {
                None
            }
        })
    });

    Some(ClipboardState {
        text,
        has_image: false, // Would need xclip -t TARGETS to check
        has_files: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_clipboard_does_not_panic() {
        let result = read_clipboard();
        // May be Some or None depending on platform
        if let Some(state) = result {
            let _ = serde_json::to_string(&state).unwrap();
        }
    }

    #[test]
    fn test_clipboard_state_serialization() {
        let state = ClipboardState {
            text: Some("hello world".into()),
            has_image: false,
            has_files: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: ClipboardState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.text.as_deref(), Some("hello world"));
        assert!(back.has_files);
    }
}
