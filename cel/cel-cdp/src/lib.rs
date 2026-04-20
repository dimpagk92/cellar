//! CEL CDP Layer
//!
//! Chrome DevTools Protocol client for extracting page content from
//! Chromium-based applications (browsers, Electron, CEF).
//!
//! CEL transparently enables CDP on Chromium apps via environment variables
//! and discovers active debug ports automatically.

mod discovery;
mod client;
mod content;
pub mod setup;

pub use discovery::{discover_cdp_targets, discover_cdp_targets_filtered, CdpTarget};
pub use client::{CdpClient, CdpError};
pub use content::{extract_page_content, PageContent, TextBlock, DomElement, ConsoleMessage, ResourceEntry, ElementBounds, ViewportInfo};
pub use setup::{install_cdp_launch_agent, uninstall_cdp_launch_agent, is_cdp_setup_installed};

pub const DEFAULT_CEL_CDP_PORT: u16 = 9333;

/// Check if CDP is available for the currently focused app.
/// Returns a connected client if a CDP target is found, None otherwise.
///
/// Prefers CEL's dedicated CDP instance (port 9333, overridable via CEL_CDP_PORT)
/// so autonomous runs stay pinned to the isolated automation browser instead of
/// drifting to whatever Chrome/Electron app happens to be frontmost.
///
/// Falls back to the focused app's PID only when the dedicated instance is not
/// available, then finally tries any remaining discovered target.
pub async fn connect_to_focused_app() -> Option<CdpClient> {
    let mut targets = discover_cdp_targets();
    if targets.is_empty() {
        return None;
    }

    sort_targets_by_preference(&mut targets);

    for target in &targets {
        match CdpClient::connect(&target.ws_url).await {
            Ok(client) => {
                let preferred_port = preferred_cel_cdp_port();
                let focused_pid = get_frontmost_pid().unwrap_or(0) as u32;
                tracing::debug!(
                    "CDP connected to {} (pid={}, port={}, focused_pid={}, preferred_port={})",
                    target.app_name,
                    target.pid,
                    target.port,
                    focused_pid,
                    preferred_port
                );
                return Some(client);
            }
            Err(e) => {
                tracing::debug!(
                    "CDP connect failed for {} (pid={}, port={}): {}",
                    target.app_name,
                    target.pid,
                    target.port,
                    e
                );
            }
        }
    }
    None
}

pub fn preferred_cel_cdp_port() -> u16 {
    preferred_cel_cdp_port_from(std::env::var("CEL_CDP_PORT").ok().as_deref())
}

pub fn discover_preferred_cdp_target() -> Option<CdpTarget> {
    let mut targets = discover_cdp_targets();
    if targets.is_empty() {
        return None;
    }
    sort_targets_by_preference(&mut targets);
    targets.into_iter().next()
}

/// Best-effort activation of CEL's preferred CDP browser instance.
///
/// Returns true if a preferred browser target with a concrete PID was found and
/// the AppleScript activation call succeeded. Returns false when there is no
/// preferred target or when activation fails.
pub fn activate_preferred_browser_target() -> bool {
    let Some(target) = discover_preferred_cdp_target() else {
        return false;
    };
    if target.pid == 0 {
        return false;
    }

    let script = format!(
        "tell application \"System Events\" to set frontmost of (first process whose unix id is {}) to true",
        target.pid
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .status()
        .map_or(false, |status| status.success())
}

fn preferred_cel_cdp_port_from(value: Option<&str>) -> u16 {
    value
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(DEFAULT_CEL_CDP_PORT)
}

fn sort_targets_by_preference(targets: &mut [CdpTarget]) {
    let preferred_port = preferred_cel_cdp_port();
    let focused_pid = get_frontmost_pid().unwrap_or(0) as u32;
    targets.sort_by_key(|target| {
        if target.port == preferred_port {
            0
        } else if focused_pid > 0 && target.pid > 0 && target.pid == focused_pid {
            1
        } else {
            2
        }
    });
}

/// Get the PID of the current frontmost application via System Events.
fn get_frontmost_pid() -> Option<i32> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to unix id of first process whose frontmost is true",
        ])
        .output()
        .ok()?;
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_port_defaults_to_cel_port() {
        assert_eq!(preferred_cel_cdp_port_from(None), 9333);
    }

    #[test]
    fn preferred_port_respects_env_override() {
        assert_eq!(preferred_cel_cdp_port_from(Some("9444")), 9444);
    }
}
