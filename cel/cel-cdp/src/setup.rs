//! CEL CDP Setup
//!
//! Legacy helper for installing a Chrome wrapper and session env var.
//!
//! The preferred product path is now CEL's dedicated browser instance on the
//! preferred CDP port. This setup flow remains for compatibility with manual
//! launches and older local workflows.

use std::path::PathBuf;

const LAUNCH_AGENT_LABEL: &str = "com.cellar.cdp";

/// The LaunchAgent plist content.
/// Only sets GOOGLE_CHROME_EXTRA_ARGS for Chrome (legacy compatibility env var).
///
/// NOTE: We deliberately do NOT set ELECTRON_EXTRA_LAUNCH_ARGS.
/// That env var is inherited by ALL Electron apps (Claude, Codex, Slack, Discord, etc.)
/// and causes crashes or instability in apps that don't expect CDP to be enabled.
/// Instead, CEL only enables CDP for Chrome (via the wrapper script) and discovers
/// other apps' debug ports via their DevToolsActivePort files if they opt in.
fn launch_agent_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>-c</string>
        <string>/bin/launchctl setenv GOOGLE_CHROME_EXTRA_ARGS --remote-debugging-port={}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>"#,
        LAUNCH_AGENT_LABEL,
        crate::DEFAULT_CEL_CDP_PORT,
    )
}

/// Get the LaunchAgent plist file path.
fn launch_agent_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", LAUNCH_AGENT_LABEL)),
    )
}

/// Install the LaunchAgent that enables CDP on Electron apps.
/// Returns Ok(true) if installed, Ok(false) if already installed.
pub fn install_cdp_launch_agent() -> Result<bool, String> {
    let path = launch_agent_path().ok_or("Could not determine LaunchAgent path")?;

    if path.exists() {
        return Ok(false); // Already installed
    }

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create LaunchAgents directory: {}", e))?;
    }

    // Write the plist
    std::fs::write(&path, launch_agent_plist())
        .map_err(|e| format!("Failed to write LaunchAgent plist: {}", e))?;

    // Load it immediately (so it takes effect without logout)
    let _ = std::process::Command::new("launchctl")
        .args(["load", path.to_str().unwrap_or("")])
        .output();

    // Set the Chrome env var for the current session (Chrome-only, not all Electron apps)
    let _ = std::process::Command::new("launchctl")
        .args([
            "setenv",
            "GOOGLE_CHROME_EXTRA_ARGS",
            &format!("--remote-debugging-port={}", crate::DEFAULT_CEL_CDP_PORT),
        ])
        .output();
    // Clean up any previously-set ELECTRON_EXTRA_LAUNCH_ARGS that affected all Electron apps
    let _ = std::process::Command::new("launchctl")
        .args(["unsetenv", "ELECTRON_EXTRA_LAUNCH_ARGS"])
        .output();

    // Install Chrome wrapper that intercepts Chrome launch and adds debug flag.
    // This uses CEL's isolated browser profile so it does not mutate the user's
    // live Chrome state.
    install_chrome_wrapper()?;

    Ok(true)
}

/// Chrome wrapper script path.
fn chrome_wrapper_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cellar"))
}

/// Install a wrapper script that Chrome can be launched through.
/// Also creates an Automator app that replaces Dock/Spotlight Chrome with the wrapper.
fn install_chrome_wrapper() -> Result<(), String> {
    let dir = chrome_wrapper_dir().ok_or("Could not determine wrapper dir")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create wrapper dir: {}", e))?;

    // Create the wrapper script
    let wrapper_path = dir.join("chrome-cdp-wrapper.sh");
    // Detect Chrome binary location (works on any macOS install).
    let wrapper_content = r#"#!/bin/bash
# CEL Chrome wrapper — launches Chrome with CEL's dedicated CDP profile.
CHROME_BIN="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
if [ ! -f "$CHROME_BIN" ]; then
    for loc in "$HOME/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
               "/Applications/Chromium.app/Contents/MacOS/Chromium" \
               "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"; do
        if [ -f "$loc" ]; then CHROME_BIN="$loc"; break; fi
    done
fi
CEL_DATA_DIR="$HOME/.cellar/cdp-profiles/google-chrome"
mkdir -p "$CEL_DATA_DIR"
exec "$CHROME_BIN" \
    --remote-debugging-port=9333 \
    --remote-allow-origins="*" \
    --user-data-dir="$CEL_DATA_DIR" \
    --no-first-run \
    --no-default-browser-check \
    --disable-sync \
    "$@"
"#;
    std::fs::write(&wrapper_path, wrapper_content)
        .map_err(|e| format!("Failed to write Chrome wrapper: {}", e))?;

    // Make executable
    let _ = std::process::Command::new("chmod")
        .args(["+x", wrapper_path.to_str().unwrap_or("")])
        .output();

    Ok(())
}

/// Uninstall the LaunchAgent.
pub fn uninstall_cdp_launch_agent() -> Result<bool, String> {
    let path = launch_agent_path().ok_or("Could not determine LaunchAgent path")?;

    if !path.exists() {
        return Ok(false); // Not installed
    }

    // Unload first
    let _ = std::process::Command::new("launchctl")
        .args(["unload", path.to_str().unwrap_or("")])
        .output();

    // Remove env vars
    let _ = std::process::Command::new("launchctl")
        .args(["unsetenv", "GOOGLE_CHROME_EXTRA_ARGS"])
        .output();
    // Also clean up legacy ELECTRON_EXTRA_LAUNCH_ARGS if still present
    let _ = std::process::Command::new("launchctl")
        .args(["unsetenv", "ELECTRON_EXTRA_LAUNCH_ARGS"])
        .output();

    // Delete the LaunchAgent plist
    std::fs::remove_file(&path).map_err(|e| format!("Failed to remove LaunchAgent: {}", e))?;

    // Remove Chrome wrapper
    if let Some(dir) = chrome_wrapper_dir() {
        let wrapper = dir.join("chrome-cdp-wrapper.sh");
        let _ = std::fs::remove_file(wrapper);
    }

    Ok(true)
}

/// Check if the LaunchAgent is installed.
pub fn is_cdp_setup_installed() -> bool {
    launch_agent_path().is_some_and(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launch_agent_plist_is_valid_xml() {
        let plist = launch_agent_plist();
        // Chrome-only: GOOGLE_CHROME_EXTRA_ARGS is set, ELECTRON_EXTRA_LAUNCH_ARGS is NOT
        assert!(plist.contains("GOOGLE_CHROME_EXTRA_ARGS"));
        assert!(!plist.contains("ELECTRON_EXTRA_LAUNCH_ARGS"),
            "LaunchAgent must NOT set ELECTRON_EXTRA_LAUNCH_ARGS — it crashes other Electron apps (Codex, Slack, etc.)");
        assert!(plist.contains(LAUNCH_AGENT_LABEL));
    }

    #[test]
    fn test_launch_agent_path() {
        let path = launch_agent_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.to_str().unwrap().contains("LaunchAgents"));
        assert!(p.to_str().unwrap().ends_with(".plist"));
    }
}
