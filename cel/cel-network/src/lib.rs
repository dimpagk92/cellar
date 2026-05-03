//! CEL Network Layer
//!
//! Honest network monitoring — reports what it actually observes.
//!
//! Two types of network data:
//! - **ConnectionEvent** — raw TCP/UDP connections from OS APIs (lsof, /proc/net/tcp)
//! - **HttpEvent** — real HTTP request/response data from CDP or proxy
//!
//! ConnectionEvent never fabricates HTTP fields. If we don't know the HTTP method,
//! we say so (None), rather than guessing from port numbers.

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
mod macos;

/// Map well-known destination ports to service names.
/// These are protocol-level names, NOT HTTP methods.
pub fn service_for_port(port: u16) -> Option<&'static str> {
    match port {
        80 | 8080 => Some("http"),
        443 | 8443 => Some("https"),
        22 => Some("ssh"),
        21 => Some("ftp"),
        25 | 587 => Some("smtp"),
        465 => Some("smtps"),
        53 => Some("dns"),
        110 => Some("pop3"),
        143 => Some("imap"),
        993 => Some("imaps"),
        995 => Some("pop3s"),
        1433 => Some("mssql"),
        3306 => Some("mysql"),
        3389 => Some("rdp"),
        5432 => Some("postgres"),
        5672 => Some("amqp"),
        6379 => Some("redis"),
        8888 => Some("jupyter"),
        9200 => Some("elasticsearch"),
        27017 => Some("mongodb"),
        _ => None,
    }
}

fn default_protocol() -> String {
    "tcp".to_string()
}

/// A raw TCP/UDP connection observed at the OS level.
///
/// This is honest data — no fabricated HTTP fields. We report exactly what
/// lsof or /proc/net/tcp tells us: IP addresses, ports, state, and process info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionEvent {
    #[serde(default)]
    pub timestamp_ms: u64,
    /// Transport protocol: "tcp", "tcp6", "udp".
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub local_addr: String,
    #[serde(default)]
    pub local_port: u16,
    #[serde(default)]
    pub remote_addr: String,
    #[serde(default)]
    pub remote_port: u16,
    /// TCP state: "ESTABLISHED", "LISTEN", "CLOSE_WAIT", etc.
    #[serde(default)]
    pub state: String,
    /// Well-known service name derived from port (e.g., "https", "ssh", "postgres").
    /// This is a port mapping, not a guess about actual protocol.
    pub service: Option<String>,
    /// Process name that owns this connection (from lsof -c or /proc/[pid]/comm).
    pub process_name: Option<String>,
    /// Process ID.
    pub pid: Option<u32>,
}

/// A real HTTP request/response observed via CDP or proxy.
///
/// Only created from sources that have actual HTTP-level data.
/// Never fabricated from port numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpEvent {
    pub timestamp_ms: u64,
    #[serde(default)]
    pub method: String,
    pub url: String,
    #[serde(alias = "status")]
    pub status_code: Option<u16>,
    pub content_type: Option<String>,
    pub duration_ms: Option<f64>,
    pub size_bytes: Option<u64>,
    /// Where this data came from: "cdp", "proxy", "performance_api".
    #[serde(default)]
    pub source: String,
}

/// Backwards-compatible alias. Consumers that only need connection-level data
/// can use this. Prefer `ConnectionEvent` for new code.
pub type NetworkEvent = ConnectionEvent;

/// Network monitor trait — returns honest connection-level data.
pub trait NetworkMonitor: Send + Sync {
    /// Start monitoring network traffic.
    fn start(&mut self) -> Result<(), NetworkError>;

    /// Stop monitoring.
    fn stop(&mut self) -> Result<(), NetworkError>;

    /// Get new connections since last call (drains the buffer).
    fn drain_events(&mut self) -> Vec<ConnectionEvent>;

    /// Whether the monitor is currently active.
    fn is_running(&self) -> bool;

    /// Whether network is idle (no recent new connections).
    fn is_idle(&self) -> bool {
        true
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("Network monitoring not available: {0}")]
    Unavailable(String),
    #[error("Monitor failed: {0}")]
    Failed(String),
}

/// Stub network monitor — no-op fallback.
pub struct StubNetworkMonitor;

impl NetworkMonitor for StubNetworkMonitor {
    fn start(&mut self) -> Result<(), NetworkError> {
        tracing::warn!("Stub network monitor: no real monitoring");
        Ok(())
    }
    fn stop(&mut self) -> Result<(), NetworkError> {
        Ok(())
    }
    fn drain_events(&mut self) -> Vec<ConnectionEvent> {
        vec![]
    }
    fn is_running(&self) -> bool {
        false
    }
}

/// Linux /proc/net/tcp monitor — polls connection state to detect new connections.
#[cfg(target_os = "linux")]
pub struct ProcNetMonitor {
    running: bool,
    events: Arc<Mutex<Vec<ConnectionEvent>>>,
    known_connections: std::collections::HashSet<String>,
    last_event_time: Option<std::time::Instant>,
}

#[cfg(target_os = "linux")]
impl Default for ProcNetMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl ProcNetMonitor {
    pub fn new() -> Self {
        Self {
            running: false,
            events: Arc::new(Mutex::new(Vec::new())),
            known_connections: std::collections::HashSet::new(),
            last_event_time: None,
        }
    }

    pub fn poll(&mut self) {
        if !self.running {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        for (path, proto) in &[("/proc/net/tcp", "tcp"), ("/proc/net/tcp6", "tcp6")] {
            if let Ok(contents) = std::fs::read_to_string(path) {
                for line in contents.lines().skip(1) {
                    if let Some(event) = self.parse_proc_line(line, now, proto) {
                        let key = format!(
                            "{}:{}->{}:{}",
                            event.local_addr,
                            event.local_port,
                            event.remote_addr,
                            event.remote_port
                        );
                        if self.known_connections.insert(key) {
                            if let Ok(mut events) = self.events.lock() {
                                events.push(event);
                            }
                        }
                    }
                }
            }
        }
    }

    fn parse_proc_line(&self, line: &str, timestamp: u64, proto: &str) -> Option<ConnectionEvent> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            return None;
        }

        let local = fields[1];
        let remote = fields[2];
        let state_hex = fields[3];

        let local_port = parse_hex_port(local)?;
        let remote_port = parse_hex_port(remote)?;
        let local_ip = parse_hex_ip(local)?;
        let remote_ip = parse_hex_ip(remote)?;
        let state = tcp_state_name(state_hex);

        // Only report established connections to non-local destinations
        if state != "ESTABLISHED" || remote_ip == "127.0.0.1" || remote_ip == "0.0.0.0" {
            return None;
        }

        // Try to get process name from /proc/[pid]/comm via inode lookup
        let pid = if fields.len() > 9 {
            // Field 9 is the inode, but mapping inode→PID requires /proc scan
            // For now, PID is not available from /proc/net/tcp directly
            None
        } else {
            None
        };

        let process_name = pid.and_then(|p| {
            std::fs::read_to_string(format!("/proc/{}/comm", p))
                .ok()
                .map(|s| s.trim().to_string())
        });

        Some(ConnectionEvent {
            timestamp_ms: timestamp,
            protocol: proto.to_string(),
            local_addr: local_ip,
            local_port,
            remote_addr: remote_ip,
            remote_port,
            state: state.to_string(),
            service: service_for_port(remote_port).map(|s| s.to_string()),
            process_name,
            pid,
        })
    }
}

#[cfg(target_os = "linux")]
impl NetworkMonitor for ProcNetMonitor {
    fn start(&mut self) -> Result<(), NetworkError> {
        self.running = true;
        self.poll(); // Baseline — drain to ignore pre-existing
        if let Ok(mut events) = self.events.lock() {
            events.clear();
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), NetworkError> {
        self.running = false;
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<ConnectionEvent> {
        self.poll();
        let drained = if let Ok(mut events) = self.events.lock() {
            std::mem::take(&mut *events)
        } else {
            vec![]
        };
        if !drained.is_empty() {
            self.last_event_time = Some(std::time::Instant::now());
        }
        drained
    }

    fn is_running(&self) -> bool {
        self.running
    }

    fn is_idle(&self) -> bool {
        match self.last_event_time {
            None => true,
            Some(t) => t.elapsed() > std::time::Duration::from_secs(3),
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_hex_port(addr: &str) -> Option<u16> {
    let parts: Vec<&str> = addr.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    u16::from_str_radix(parts[1], 16).ok()
}

#[cfg(target_os = "linux")]
fn parse_hex_ip(addr: &str) -> Option<String> {
    let parts: Vec<&str> = addr.split(':').collect();
    if parts.is_empty() {
        return None;
    }
    let hex = parts[0];
    if hex.len() == 8 {
        // IPv4 in hex (little-endian on x86)
        let bytes: Vec<u8> = (0..8)
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect();
        if bytes.len() == 4 {
            Some(format!(
                "{}.{}.{}.{}",
                bytes[3], bytes[2], bytes[1], bytes[0]
            ))
        } else {
            None
        }
    } else {
        // IPv6 — just return hex representation
        Some(hex.to_string())
    }
}

#[cfg(target_os = "linux")]
fn tcp_state_name(hex: &str) -> &str {
    match hex {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        _ => "UNKNOWN",
    }
}

/// Create a platform-appropriate network monitor.
pub fn create_monitor() -> Box<dyn NetworkMonitor> {
    #[cfg(target_os = "linux")]
    {
        Box::new(ProcNetMonitor::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::LsofNetMonitor::new())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Box::new(StubNetworkMonitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stub_monitor_start_stop() {
        let mut monitor = StubNetworkMonitor;
        assert!(monitor.start().is_ok());
        assert!(!monitor.is_running());
        assert!(monitor.stop().is_ok());
    }

    #[test]
    fn test_stub_monitor_drain_empty() {
        let mut monitor = StubNetworkMonitor;
        monitor.start().unwrap();
        let events = monitor.drain_events();
        assert!(events.is_empty());
    }

    #[test]
    fn test_connection_event_serialization() {
        let event = ConnectionEvent {
            timestamp_ms: 1700000000000,
            protocol: "tcp".into(),
            local_addr: "192.168.1.100".into(),
            local_port: 54321,
            remote_addr: "93.184.216.34".into(),
            remote_port: 443,
            state: "ESTABLISHED".into(),
            service: Some("https".into()),
            process_name: Some("chrome".into()),
            pid: Some(1234),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ConnectionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.remote_addr, "93.184.216.34");
        assert_eq!(back.remote_port, 443);
        assert_eq!(back.service.as_deref(), Some("https"));
        assert_eq!(back.process_name.as_deref(), Some("chrome"));
    }

    #[test]
    fn test_http_event_serialization() {
        let event = HttpEvent {
            timestamp_ms: 1700000000000,
            method: "GET".into(),
            url: "https://api.example.com/data".into(),
            status_code: Some(200),
            content_type: Some("application/json".into()),
            duration_ms: Some(150.5),
            size_bytes: Some(4096),
            source: "cdp".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: HttpEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "GET");
        assert_eq!(back.url, "https://api.example.com/data");
        assert_eq!(back.status_code, Some(200));
        assert_eq!(back.source, "cdp");
    }

    #[test]
    fn test_connection_event_minimal() {
        let event = ConnectionEvent {
            timestamp_ms: 0,
            protocol: "tcp".into(),
            local_addr: "0.0.0.0".into(),
            local_port: 0,
            remote_addr: "1.2.3.4".into(),
            remote_port: 9999,
            state: "ESTABLISHED".into(),
            service: None,
            process_name: None,
            pid: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ConnectionEvent = serde_json::from_str(&json).unwrap();
        assert!(back.service.is_none());
        assert!(back.process_name.is_none());
    }

    #[test]
    fn test_network_error_display() {
        assert_eq!(
            NetworkError::Unavailable("no pcap".into()).to_string(),
            "Network monitoring not available: no pcap"
        );
        assert_eq!(
            NetworkError::Failed("connection reset".into()).to_string(),
            "Monitor failed: connection reset"
        );
    }

    #[test]
    fn test_service_for_port() {
        assert_eq!(service_for_port(80), Some("http"));
        assert_eq!(service_for_port(8080), Some("http"));
        assert_eq!(service_for_port(443), Some("https"));
        assert_eq!(service_for_port(8443), Some("https"));
        assert_eq!(service_for_port(22), Some("ssh"));
        assert_eq!(service_for_port(21), Some("ftp"));
        assert_eq!(service_for_port(25), Some("smtp"));
        assert_eq!(service_for_port(587), Some("smtp"));
        assert_eq!(service_for_port(465), Some("smtps"));
        assert_eq!(service_for_port(53), Some("dns"));
        assert_eq!(service_for_port(110), Some("pop3"));
        assert_eq!(service_for_port(143), Some("imap"));
        assert_eq!(service_for_port(993), Some("imaps"));
        assert_eq!(service_for_port(995), Some("pop3s"));
        assert_eq!(service_for_port(1433), Some("mssql"));
        assert_eq!(service_for_port(3306), Some("mysql"));
        assert_eq!(service_for_port(3389), Some("rdp"));
        assert_eq!(service_for_port(5432), Some("postgres"));
        assert_eq!(service_for_port(5672), Some("amqp"));
        assert_eq!(service_for_port(6379), Some("redis"));
        assert_eq!(service_for_port(8888), Some("jupyter"));
        assert_eq!(service_for_port(9200), Some("elasticsearch"));
        assert_eq!(service_for_port(27017), Some("mongodb"));
        assert_eq!(service_for_port(9999), None);
    }

    #[test]
    fn test_create_monitor() {
        let mut monitor = create_monitor();
        assert!(monitor.start().is_ok());
        let _ = monitor.drain_events();
        assert!(monitor.stop().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_proc_net_monitor_lifecycle() {
        let mut monitor = ProcNetMonitor::new();
        assert!(!monitor.is_running());
        monitor.start().unwrap();
        assert!(monitor.is_running());
        let _events = monitor.drain_events();
        monitor.stop().unwrap();
        assert!(!monitor.is_running());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_hex_port() {
        assert_eq!(parse_hex_port("0100007F:0050"), Some(80));
        assert_eq!(parse_hex_port("0100007F:01BB"), Some(443));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_hex_ip() {
        assert_eq!(parse_hex_ip("0100007F:0050"), Some("127.0.0.1".to_string()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_tcp_state_name() {
        assert_eq!(tcp_state_name("01"), "ESTABLISHED");
        assert_eq!(tcp_state_name("0A"), "LISTEN");
        assert_eq!(tcp_state_name("FF"), "UNKNOWN");
    }
}
