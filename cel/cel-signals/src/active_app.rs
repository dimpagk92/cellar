//! Running apps signal source.
//!
//! macOS: reads running GUI applications via osascript.
//! Frontmost detection is handled by AXObserver (cel-accessibility), not here.

use serde::{Deserialize, Serialize};

/// A running GUI application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningApp {
    /// Application name.
    pub name: String,
    /// Whether this app is currently frontmost (queryable state).
    /// AXObserver provides real-time transition events; this provides current state.
    pub is_frontmost: bool,
}

/// List running GUI applications with frontmost state.
pub fn list_running_apps() -> Vec<RunningApp> {
    #[cfg(target_os = "macos")]
    {
        list_running_apps_macos()
    }
    #[cfg(target_os = "linux")]
    {
        list_running_apps_linux()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        vec![]
    }
}

/// Linux implementation using `xdotool` and `wmctrl`.
#[cfg(target_os = "linux")]
fn list_running_apps_linux() -> Vec<RunningApp> {
    // Determine the active window name via xdotool.
    let active_name = get_active_window_name().unwrap_or_default();

    // Build list from wmctrl if available, otherwise try xdotool.
    let mut apps = list_apps_wmctrl().unwrap_or_default();
    if apps.is_empty() {
        apps = list_apps_xdotool().unwrap_or_default();
    }

    // Mark the active app.
    if !active_name.is_empty() {
        for app in &mut apps {
            if app.name == active_name {
                app.is_frontmost = true;
            }
        }
    }

    // Deduplicate by name, preserving frontmost flag.
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(app.name.clone()));

    apps
}

/// Get the active window's app name via `xdotool getactivewindow`.
#[cfg(target_os = "linux")]
fn get_active_window_name() -> Option<String> {
    let wid_output = std::process::Command::new("xdotool")
        .args(["getactivewindow"])
        .output()
        .ok()?;
    if !wid_output.status.success() {
        return None;
    }
    let wid = String::from_utf8_lossy(&wid_output.stdout).trim().to_string();
    if wid.is_empty() {
        return None;
    }

    // Try to get PID and resolve via /proc/PID/comm.
    let pid_output = std::process::Command::new("xdotool")
        .args(["getwindowpid", &wid])
        .output()
        .ok();
    if let Some(o) = pid_output {
        if o.status.success() {
            let pid_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Ok(pid) = pid_str.parse::<u32>() {
                if pid > 0 {
                    if let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", pid)) {
                        let name = comm.trim().to_string();
                        if !name.is_empty() {
                            return Some(name);
                        }
                    }
                }
            }
        }
    }

    // Fallback: use the window title.
    let name_output = std::process::Command::new("xdotool")
        .args(["getwindowname", &wid])
        .output()
        .ok()?;
    if name_output.status.success() {
        let name = String::from_utf8_lossy(&name_output.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }

    None
}

/// Build app list from `wmctrl -lp`, resolving PIDs to app names via `/proc/PID/comm`.
#[cfg(target_os = "linux")]
fn list_apps_wmctrl() -> Option<Vec<RunningApp>> {
    let output = std::process::Command::new("wmctrl")
        .args(["-lp"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut apps = Vec::new();

    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let pid: u32 = fields[2].parse().unwrap_or(0);

        let name = if pid > 0 {
            std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        } else {
            // Use window title as fallback.
            if fields.len() > 4 {
                fields[4..].join(" ")
            } else {
                String::new()
            }
        };

        if name.is_empty() {
            continue;
        }

        apps.push(RunningApp {
            name,
            is_frontmost: false,
        });
    }

    Some(apps)
}

/// Fallback app list from xdotool.
#[cfg(target_os = "linux")]
fn list_apps_xdotool() -> Option<Vec<RunningApp>> {
    let id_output = std::process::Command::new("xdotool")
        .args(["search", "--name", ""])
        .output()
        .ok()?;
    if !id_output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&id_output.stdout);
    let mut apps = Vec::new();

    for wid_str in text.lines() {
        let wid_str = wid_str.trim();
        if wid_str.is_empty() {
            continue;
        }

        let pid_output = std::process::Command::new("xdotool")
            .args(["getwindowpid", wid_str])
            .output()
            .ok();
        let pid: u32 = match pid_output {
            Some(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse()
                    .unwrap_or(0)
            }
            _ => 0,
        };

        let name = if pid > 0 {
            std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        } else {
            // Fallback to window name.
            let name_output = std::process::Command::new("xdotool")
                .args(["getwindowname", wid_str])
                .output()
                .ok();
            match name_output {
                Some(o) if o.status.success() => {
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                }
                _ => String::new(),
            }
        };

        if name.is_empty() {
            continue;
        }

        apps.push(RunningApp {
            name,
            is_frontmost: false,
        });
    }

    Some(apps)
}

#[cfg(target_os = "macos")]
fn list_running_apps_macos() -> Vec<RunningApp> {
    // Get app names and frontmost status in a single osascript call
    let output = std::process::Command::new("osascript")
        .args(["-e", r#"
            tell application "System Events"
                set frontName to name of first process whose frontmost is true
                set appNames to name of every process whose background only is false
                set output to ""
                repeat with n in appNames
                    set output to output & n & "||" & (n as text is frontName) & "\n"
                end repeat
                return output
            end tell
        "#])
        .output()
        .ok();

    let text = match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return vec![],
    };

    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split("||").collect();
            if parts.len() >= 2 {
                let name = parts[0].trim().to_string();
                if name.is_empty() { return None; }
                Some(RunningApp {
                    name,
                    is_frontmost: parts[1].trim() == "true",
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_running_apps_does_not_panic() {
        let apps = list_running_apps();
        for app in &apps {
            let _ = serde_json::to_string(app).unwrap();
        }
    }

    #[test]
    fn test_running_app_serialization() {
        let app = RunningApp {
            name: "Safari".into(),
            is_frontmost: true,
        };
        let json = serde_json::to_string(&app).unwrap();
        let back: RunningApp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Safari");
        assert!(back.is_frontmost);
    }
}
