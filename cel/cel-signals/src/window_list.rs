//! Window list signal source.
//!
//! macOS: CGWindowListCopyWindowInfo — all visible windows with bounds, app, and layer.
//! Linux: wmctrl or /proc-based window enumeration.

use serde::{Deserialize, Serialize};

/// A visible window on screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    /// Owning application name.
    pub app_name: String,
    /// Window title.
    pub title: String,
    /// Screen-space bounds.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Window layer (0 = normal, >0 = floating/overlay).
    pub layer: i32,
    /// Whether the window is currently on screen (not minimized).
    pub is_on_screen: bool,
    /// Owning process ID.
    pub pid: u32,
}

/// List all visible windows. Returns empty vec if unavailable.
pub fn list_windows() -> Vec<WindowState> {
    #[cfg(target_os = "macos")]
    {
        list_windows_macos()
    }
    #[cfg(target_os = "linux")]
    {
        list_windows_linux()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        vec![]
    }
}

/// Linux implementation: tries `wmctrl -lp` first, then `xdotool` as fallback.
#[cfg(target_os = "linux")]
fn list_windows_linux() -> Vec<WindowState> {
    if let Some(windows) = list_windows_wmctrl() {
        return windows;
    }
    if let Some(windows) = list_windows_xdotool() {
        return windows;
    }
    vec![]
}

/// Parse `wmctrl -lp` output.
///
/// Each line has the format:
/// `0x0400000e  0 12345  hostname Window Title`
#[cfg(target_os = "linux")]
fn list_windows_wmctrl() -> Option<Vec<WindowState>> {
    let output = std::process::Command::new("wmctrl")
        .args(["-lp"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut windows = Vec::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(5, char::is_whitespace).collect();
        if parts.len() < 5 {
            continue;
        }
        // parts: [wid, desktop, pid, hostname, title...]
        // Because splitn with whitespace may produce empty parts, re-parse more carefully.
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let pid: u32 = fields[2].parse().unwrap_or(0);
        // Title is everything after the 4th whitespace-separated field.
        let title = if fields.len() > 4 {
            fields[4..].join(" ")
        } else {
            String::new()
        };

        // Try to resolve app name from /proc/PID/comm
        let app_name = if pid > 0 {
            std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        // Skip windows with no title and no app name.
        if app_name.is_empty() && title.is_empty() {
            continue;
        }

        windows.push(WindowState {
            app_name,
            title,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            layer: 0,
            is_on_screen: true,
            pid,
        });
    }

    Some(windows)
}

/// Fallback using `xdotool search --name "" getwindowname %@`.
#[cfg(target_os = "linux")]
fn list_windows_xdotool() -> Option<Vec<WindowState>> {
    // Get all window IDs.
    let id_output = std::process::Command::new("xdotool")
        .args(["search", "--name", ""])
        .output()
        .ok()?;
    if !id_output.status.success() {
        return None;
    }
    let id_text = String::from_utf8_lossy(&id_output.stdout);
    let mut windows = Vec::new();

    for wid_str in id_text.lines() {
        let wid_str = wid_str.trim();
        if wid_str.is_empty() {
            continue;
        }

        // Get the window name for this window ID.
        let name_output = std::process::Command::new("xdotool")
            .args(["getwindowname", wid_str])
            .output()
            .ok();
        let title = match name_output {
            Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => String::new(),
        };

        // Get the PID for this window ID.
        let pid_output = std::process::Command::new("xdotool")
            .args(["getwindowpid", wid_str])
            .output()
            .ok();
        let pid: u32 = match pid_output {
            Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0),
            _ => 0,
        };

        let app_name = if pid > 0 {
            std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        if app_name.is_empty() && title.is_empty() {
            continue;
        }

        windows.push(WindowState {
            app_name,
            title,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            layer: 0,
            is_on_screen: true,
            pid,
        });
    }

    Some(windows)
}

#[cfg(target_os = "macos")]
fn list_windows_macos() -> Vec<WindowState> {
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_graphics::display::{
        kCGNullWindowID, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
        CGWindowListCopyWindowInfo,
    };

    let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
    let window_list = unsafe { CGWindowListCopyWindowInfo(options, kCGNullWindowID) };
    if window_list.is_null() {
        return vec![];
    }

    let array: CFArray = unsafe { CFArray::wrap_under_create_rule(window_list as _) };
    let count = unsafe { core_foundation::array::CFArrayGetCount(array.as_concrete_TypeRef()) };
    let mut windows = Vec::new();

    for i in 0..count {
        let ptr = unsafe {
            core_foundation::array::CFArrayGetValueAtIndex(array.as_concrete_TypeRef(), i)
        };
        if ptr.is_null() {
            continue;
        }
        let dict: CFDictionary = unsafe { CFDictionary::wrap_under_get_rule(ptr as *const _) };

        let app_name = get_dict_string(&dict, "kCGWindowOwnerName").unwrap_or_default();
        let title = get_dict_string(&dict, "kCGWindowName").unwrap_or_default();
        let layer = get_dict_i32(&dict, "kCGWindowLayer").unwrap_or(0);
        let pid = get_dict_i32(&dict, "kCGWindowOwnerPID").unwrap_or(0) as u32;
        let on_screen = get_dict_i32(&dict, "kCGWindowIsOnscreen").unwrap_or(0) != 0;

        // Get bounds from kCGWindowBounds dictionary
        let (x, y, width, height) =
            get_dict_bounds(&dict, "kCGWindowBounds").unwrap_or((0, 0, 0, 0));

        // Skip windows with no name and no meaningful bounds (menu bar items, system UI)
        if app_name.is_empty() && title.is_empty() {
            continue;
        }
        if width == 0 || height == 0 {
            continue;
        }

        windows.push(WindowState {
            app_name,
            title,
            x,
            y,
            width,
            height,
            layer,
            is_on_screen: on_screen,
            pid,
        });
    }

    windows
}

#[cfg(target_os = "macos")]
fn get_dict_string(dict: &core_foundation::dictionary::CFDictionary, key: &str) -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;

    let key_cf = CFString::new(key);
    let value = dict.find(key_cf.as_CFTypeRef())?;
    let cf_ref = *value;
    if cf_ref.is_null() {
        return None;
    }
    if unsafe { core_foundation::string::CFStringGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        let s: CFString = unsafe {
            CFString::wrap_under_get_rule(cf_ref as core_foundation::string::CFStringRef)
        };
        let result = s.to_string();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_dict_i32(dict: &core_foundation::dictionary::CFDictionary, key: &str) -> Option<i32> {
    use core_foundation::base::TCFType;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    let key_cf = CFString::new(key);
    let value = dict.find(key_cf.as_CFTypeRef())?;
    let cf_ref = *value;
    if cf_ref.is_null() {
        return None;
    }
    if unsafe { core_foundation::number::CFNumberGetTypeID() }
        == unsafe { core_foundation::base::CFGetTypeID(cf_ref) }
    {
        let n: CFNumber = unsafe {
            CFNumber::wrap_under_get_rule(cf_ref as core_foundation::number::CFNumberRef)
        };
        n.to_i32()
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_dict_bounds(
    dict: &core_foundation::dictionary::CFDictionary,
    key: &str,
) -> Option<(i32, i32, u32, u32)> {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    let key_cf = CFString::new(key);
    let value = dict.find(key_cf.as_CFTypeRef())?;
    let cf_ref = *value;
    if cf_ref.is_null() {
        return None;
    }

    let bounds_dict: CFDictionary =
        unsafe { CFDictionary::wrap_under_get_rule(cf_ref as *const _) };

    let x = get_dict_i32(&bounds_dict, "X").unwrap_or(0);
    let y = get_dict_i32(&bounds_dict, "Y").unwrap_or(0);
    let w = get_dict_i32(&bounds_dict, "Width").unwrap_or(0).max(0) as u32;
    let h = get_dict_i32(&bounds_dict, "Height").unwrap_or(0).max(0) as u32;

    Some((x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_windows_does_not_panic() {
        let windows = list_windows();
        // On macOS, should return at least the current app's window
        for w in &windows {
            let _ = serde_json::to_string(w).unwrap();
        }
    }

    #[test]
    fn test_window_state_serialization() {
        let w = WindowState {
            app_name: "Safari".into(),
            title: "Apple".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            layer: 0,
            is_on_screen: true,
            pid: 1234,
        };
        let json = serde_json::to_string(&w).unwrap();
        let back: WindowState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.app_name, "Safari");
        assert_eq!(back.pid, 1234);
    }
}
