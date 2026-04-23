//! CDP Target Discovery
//!
//! Finds Chromium debug ports across all running apps without user configuration.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// A discovered CDP debug target.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CdpTarget {
    pub app_name: String,
    pub pid: u32,
    pub port: u16,
    pub ws_url: String,
}

/// Detailed HTTP target metadata from Chrome's /json endpoints.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CdpHttpTarget {
    pub id: String,
    pub app_name: String,
    pub port: u16,
    pub title: String,
    pub url: String,
    pub ws_url: String,
}

/// Apps whose DevToolsActivePort should NOT be read during passive discovery.
/// These are Electron apps where connecting to their CDP port would interfere
/// with the host process (e.g., Claude Code, Codex — CEL runs inside these).
///
/// Process-args scanning still finds them if they explicitly expose a port,
/// but the file-based scan is the problematic one: it reads stale port files
/// and connects to apps that didn't ask to be controlled.
const CDP_PASSIVE_EXCLUDED_APPS: &[&str] = &[
    "claude",      // Claude Code / Claude Desktop / Codex
    "codex",       // Codex CLI Electron wrapper
    "slack",       // Rarely useful for automation
    "discord",     // Rarely useful for automation
    "notion",      // Has its own API
    "obsidian",    // Has its own API
];

/// Check if an app name matches the passive exclusion list (case-insensitive).
fn is_passive_excluded(app_name: &str) -> bool {
    let lower = app_name.to_lowercase();
    CDP_PASSIVE_EXCLUDED_APPS.iter().any(|excl| lower.contains(excl))
}

/// Discover all available CDP targets on this machine.
/// Combines multiple discovery strategies:
/// 1. Scan process args for --remote-debugging-port
/// 2. Scan DevToolsActivePort files in known browser data directories
/// 3. Query discovered ports for WebSocket URLs
///
/// By default, only discovers **browser** targets (Chrome, Edge, Arc, Brave).
/// Electron apps like Claude/Codex/Slack/Discord are excluded from passive
/// file-based discovery to prevent interfering with apps CEL runs inside.
///
/// Set `include_all` to true to discover ALL CDP-capable apps (for explicit
/// user requests like "connect to Codex's CDP port").
pub fn discover_cdp_targets() -> Vec<CdpTarget> {
    discover_cdp_targets_filtered(false)
}

/// Discover CDP targets with optional inclusion of all apps.
pub fn discover_cdp_targets_filtered(include_all: bool) -> Vec<CdpTarget> {
    let mut seen_ports = HashSet::new();
    let mut targets = Vec::new();

    // Strategy 0: probe CEL's dedicated browser port directly. This keeps the
    // native CDP path aligned with the dedicated browser manager even when the
    // process table or DevToolsActivePort file doesn't expose the target.
    if let Some(target) = probe_preferred_cdp_target() {
        if seen_ports.insert(target.port) {
            targets.push(target);
        }
    }

    // Strategy 1: Scan process args.
    // In normal mode we still exclude known host apps like Codex/Claude so CEL
    // never accidentally drives or destabilizes the shell it is running inside.
    // `include_all=true` remains the explicit escape hatch for debugging.
    for target in scan_process_args() {
        if !include_all && is_passive_excluded(&target.app_name) {
            continue;
        }
        if seen_ports.insert(target.port) {
            targets.push(target);
        }
    }

    // Strategy 2: Scan DevToolsActivePort files (filtered unless include_all)
    for target in scan_devtools_port_files() {
        if !include_all && is_passive_excluded(&target.app_name) {
            continue;
        }
        if seen_ports.insert(target.port) {
            targets.push(target);
        }
    }

    // Strategy 3: For each discovered port, query /json/list to get WebSocket URLs
    let mut enriched = Vec::new();
    for mut target in targets {
        if target.ws_url.is_empty() {
            if let Some(ws) = query_json_list(target.port) {
                target.ws_url = ws;
            }
        }
        if !target.ws_url.is_empty() {
            enriched.push(target);
        }
    }

    enriched
}

fn preferred_cel_cdp_port() -> u16 {
    std::env::var("CEL_CDP_PORT")
        .ok()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(crate::DEFAULT_CEL_CDP_PORT)
}

fn probe_preferred_cdp_target() -> Option<CdpTarget> {
    let port = preferred_cel_cdp_port();
    let ws_url = query_json_list(port)?;
    Some(CdpTarget {
        app_name: query_browser_name(port).unwrap_or_else(|| "Browser".to_string()),
        pid: 0,
        port,
        ws_url,
    })
}

/// Scan running processes for --remote-debugging-port=N in their args.
fn scan_process_args() -> Vec<CdpTarget> {
    let mut targets = Vec::new();

    let output = match std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return targets,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let pid = parts
            .next()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let command = parts.next().map(str::trim).unwrap_or("");
        if let Some(port_str) = extract_debug_port(command) {
            if let Ok(port) = port_str.parse::<u16>() {
                if port == 0 {
                    continue; // Port 0 means "assigned at runtime" — check DevToolsActivePort instead
                }
                let app_name = extract_app_name(command);
                targets.push(CdpTarget {
                    app_name,
                    pid,
                    port,
                    ws_url: String::new(), // Will be enriched later
                });
            }
        }
    }

    targets
}

/// Extract --remote-debugging-port=N value from a process command line.
fn extract_debug_port(line: &str) -> Option<&str> {
    let marker = "--remote-debugging-port=";
    let start = line.find(marker)? + marker.len();
    let rest = &line[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end > 0 {
        Some(&rest[..end])
    } else {
        None
    }
}

/// List page targets exposed by Chrome's DevTools HTTP endpoint.
pub fn list_http_targets(port: u16) -> Vec<CdpHttpTarget> {
    let body = match http_request_local(port, "GET", "/json/list") {
        Some((status, body)) if (200..300).contains(&status) => body,
        _ => return Vec::new(),
    };

    let version = query_browser_name(port).unwrap_or_else(|| "Browser".to_string());
    let entries: Vec<serde_json::Value> = match serde_json::from_str(&body) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut targets = Vec::new();
    for entry in entries {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else { continue };
        let Some(ws_url) = entry.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) else { continue };
        let entry_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or_default();
        if entry_type != "page" {
            continue;
        }
        let url = entry
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if url.starts_with("devtools://") {
            continue;
        }
        targets.push(CdpHttpTarget {
            id: id.to_string(),
            app_name: version.clone(),
            port,
            title: entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            url,
            ws_url: ws_url.to_string(),
        });
    }

    targets
}

fn percent_encode_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for byte in url.bytes() {
        let is_allowed = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'~' | b':' | b'/' | b'?' | b'&' | b'=' | b'%' | b'#'
            );
        if is_allowed {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn open_new_target(port: u16, url: &str) -> Option<CdpHttpTarget> {
    let path = format!("/json/new?{}", percent_encode_url(url));
    let body = match http_request_local(port, "PUT", &path) {
        Some((status, body)) if (200..300).contains(&status) => body,
        _ => return None,
    };

    let entry: serde_json::Value = serde_json::from_str(&body).ok()?;
    let id = entry.get("id")?.as_str()?.to_string();
    let ws_url = entry.get("webSocketDebuggerUrl")?.as_str()?.to_string();
    let title = entry
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let url = entry
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(CdpHttpTarget {
        id,
        app_name: query_browser_name(port).unwrap_or_else(|| "Browser".into()),
        port,
        title,
        url,
        ws_url,
    })
}

fn activate_target(port: u16, target_id: &str) -> bool {
    let path = format!("/json/activate/{}", target_id);
    matches!(http_request_local(port, "GET", &path), Some((status, _)) if (200..300).contains(&status))
}

fn close_target(port: u16, target_id: &str) -> bool {
    let path = format!("/json/close/{}", target_id);
    matches!(http_request_local(port, "GET", &path), Some((status, _)) if (200..300).contains(&status))
}

/// Reset CEL's dedicated browser to a fresh single page target at `url`.
///
/// This avoids stale-tab drift by opening a new target on the dedicated CDP
/// browser, activating it, and then closing older page targets on the same port.
pub fn reset_preferred_target(url: &str) -> Result<(), String> {
    let port = preferred_cel_cdp_port();
    let existing = list_http_targets(port);
    let new_target = open_new_target(port, url)
        .ok_or_else(|| format!("Failed to open a new CDP target for {url} on port {port}"))?;

    if !activate_target(port, &new_target.id) {
        tracing::debug!(
            "CDP HTTP activate failed for new target {} on port {}",
            new_target.id,
            port
        );
    }

    for target in existing {
        if target.id != new_target.id {
            let _ = close_target(port, &target.id);
        }
    }

    Ok(())
}

/// Extract app name from a ps aux line.
fn extract_app_name(line: &str) -> String {
    // Look for .app in the full command string (not split by whitespace)
    if let Some(app_start) = line.find("/Applications/") {
        let rest = &line[app_start..];
        if let Some(app_end) = rest.find(".app") {
            let app_path = &rest["/Applications/".len()..app_end];
            // Handle nested paths like "Google Chrome.app/Contents/..."
            return app_path.split('/').next().unwrap_or("unknown").to_string();
        }
    }
    // Fallback: extract from command field
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() > 10 {
        fields[10].split('/').last().unwrap_or("unknown").to_string()
    } else {
        "unknown".to_string()
    }
}

/// Scan known app data directories for DevToolsActivePort files.
/// Chromium writes the debug port to this file when --remote-debugging-port=0 is used.
fn scan_devtools_port_files() -> Vec<CdpTarget> {
    let mut targets = Vec::new();
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return targets,
    };

    // Known app data directories on macOS
    let candidates = vec![
        ("Google Chrome", format!("{}/Library/Application Support/Google/Chrome", home)),
        ("Google Chrome Canary", format!("{}/Library/Application Support/Google/Chrome Canary", home)),
        ("Microsoft Edge", format!("{}/Library/Application Support/Microsoft Edge", home)),
        ("Brave Browser", format!("{}/Library/Application Support/BraveSoftware/Brave-Browser", home)),
        ("Arc", format!("{}/Library/Application Support/Arc", home)),
        ("Opera", format!("{}/Library/Application Support/com.operasoftware.Opera", home)),
        // Claude/Codex — excluded from passive discovery via CDP_PASSIVE_EXCLUDED_APPS.
        // Available when discover_cdp_targets_filtered(true) is called explicitly.
        ("Claude", format!("{}/Library/Application Support/Claude", home)),
        ("Visual Studio Code", format!("{}/Library/Application Support/Code", home)),
        ("Slack", format!("{}/Library/Application Support/Slack", home)),
        ("Discord", format!("{}/Library/Application Support/discord", home)),
        ("Notion", format!("{}/Library/Application Support/Notion", home)),
        ("Obsidian", format!("{}/Library/Application Support/obsidian", home)),
    ];

    for (app_name, dir) in candidates {
        let port_file = PathBuf::from(&dir).join("DevToolsActivePort");
        if let Ok(contents) = std::fs::read_to_string(&port_file) {
            let lines: Vec<&str> = contents.lines().collect();
            if let Some(port_str) = lines.first() {
                if let Ok(port) = port_str.trim().parse::<u16>() {
                    let ws_path = lines.get(1).unwrap_or(&"");
                    let ws_url = if !ws_path.is_empty() {
                        format!("ws://127.0.0.1:{}{}", port, ws_path)
                    } else {
                        String::new()
                    };
                    targets.push(CdpTarget {
                        app_name: app_name.to_string(),
                        pid: 0, // DevToolsActivePort doesn't include PID
                        port,
                        ws_url,
                    });
                }
            }
        }
    }

    targets
}

/// Query a CDP port's /json/list endpoint to get the WebSocket debug URL.
fn query_json_list(port: u16) -> Option<String> {
    let body = http_get_local_json(port, "/json/list")?;
    // Parse JSON array, prefer real page targets over blank tabs.
    let entries: Vec<serde_json::Value> = serde_json::from_str(&body).ok()?;
    let mut fallback_page: Option<String> = None;
    for entry in &entries {
        let entry_type = entry.get("type")?.as_str()?;
        if entry_type != "page" {
            continue;
        }

        let ws_url = entry
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if fallback_page.is_none() {
            fallback_page = ws_url.clone();
        }

        let page_url = entry
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let is_blank = page_url.is_empty()
            || page_url == "about:blank"
            || page_url.starts_with("chrome://new-tab-page")
            || page_url.starts_with("chrome-search://")
            || page_url.starts_with("devtools://");
        if !is_blank {
            return ws_url;
        }
    }
    fallback_page.or_else(|| {
        entries
            .first()?
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

fn query_browser_name(port: u16) -> Option<String> {
    let body = http_get_local_json(port, "/json/version")?;
    let payload: serde_json::Value = serde_json::from_str(&body).ok()?;
    payload
        .get("Browser")
        .and_then(|value| value.as_str())
        .map(|browser| browser.split('/').next().unwrap_or("Browser").to_string())
}

fn http_get_local_json(port: u16, path: &str) -> Option<String> {
    http_request_local(port, "GET", path).map(|(_, body)| body)
}

fn http_request_local(port: u16, method: &str, path: &str) -> Option<(u16, String)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));

    // Chrome's DevTools HTTP endpoint silently closes HTTP/1.0 requests, so we
    // must speak HTTP/1.1. `Connection: close` still lets the server end the
    // response with EOF without keeping the socket open for another request.
    let request = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        method, path, port
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut response = Vec::new();
    let mut header_end = None;

    while header_end.is_none() {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4);
    }

    let header_end = header_end?;
    let header_text = String::from_utf8_lossy(&response[..header_end]);
    let status_code = header_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(0);
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        });

    if let Some(content_length) = content_length {
        while response.len() < header_end + content_length {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            response.extend_from_slice(&chunk[..read]);
        }
        if response.len() < header_end + content_length {
            return None;
        }
        let body = String::from_utf8(response[header_end..header_end + content_length].to_vec()).ok()?;
        return Some((status_code, body));
    }

    loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
    }

    let body = String::from_utf8(response[header_end..].to_vec()).ok()?;
    Some((status_code, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_debug_port() {
        assert_eq!(
            extract_debug_port("chrome --remote-debugging-port=9222 --other"),
            Some("9222")
        );
        assert_eq!(
            extract_debug_port("electron --remote-debugging-port=0"),
            Some("0")
        );
        assert_eq!(extract_debug_port("normal process"), None);
    }

    #[test]
    fn test_extract_app_name() {
        assert_eq!(
            extract_app_name("user  1234  0.0  0.0  0  0  ??  S  0:00.00  0  /Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            "Google Chrome"
        );
    }

    #[test]
    fn test_passive_excluded_apps() {
        assert!(is_passive_excluded("Claude"));
        assert!(is_passive_excluded("claude"));
        assert!(is_passive_excluded("Codex"));
        assert!(is_passive_excluded("codex"));
        assert!(is_passive_excluded("Slack"));
        assert!(is_passive_excluded("Discord"));
        assert!(!is_passive_excluded("Google Chrome"));
        assert!(!is_passive_excluded("Arc"));
        assert!(!is_passive_excluded("Visual Studio Code"));
    }

    #[test]
    fn test_discover_runs_without_panic() {
        // Just verify it doesn't crash — may or may not find targets
        let targets = discover_cdp_targets();
        // targets may be empty if no CDP apps are running
        let _ = targets;
    }
}
