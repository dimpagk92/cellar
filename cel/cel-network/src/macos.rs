//! macOS Network Monitor
//!
//! Uses `lsof -i -n -P -F pcn` to detect active network connections.
//! Returns honest ConnectionEvent data — no fabricated HTTP fields.

use crate::{service_for_port, ConnectionEvent, NetworkError, NetworkMonitor};
use std::collections::HashSet;
use std::process::Command;

/// A raw connection with its process info from lsof.
struct LsofConnection {
    conn_str: String,
    pid: Option<u32>,
    process_name: Option<String>,
}

/// macOS network monitor using lsof.
pub struct LsofNetMonitor {
    running: bool,
    known_connections: HashSet<String>,
    events: Vec<ConnectionEvent>,
    last_new_connection_ms: u64,
}

impl LsofNetMonitor {
    pub fn new() -> Self {
        Self {
            running: false,
            known_connections: HashSet::new(),
            events: Vec::new(),
            last_new_connection_ms: 0,
        }
    }

    fn poll(&mut self) {
        if !self.running {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let current = self.get_connections();
        for lsof_conn in &current {
            if self.known_connections.insert(lsof_conn.conn_str.clone()) {
                self.last_new_connection_ms = now;
                if let Some(event) = parse_lsof_connection(
                    &lsof_conn.conn_str,
                    now,
                    lsof_conn.pid,
                    lsof_conn.process_name.clone(),
                ) {
                    self.events.push(event);
                }
            }
        }
    }

    /// Get current network connections from lsof with process names.
    fn get_connections(&self) -> Vec<LsofConnection> {
        let mut connections = Vec::new();

        // -F pcn: output PID (p), command name (c), and file name (n)
        let output = Command::new("lsof")
            .args(["-i", "-n", "-P", "-F", "pcn"])
            .output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return connections,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);

        // lsof -F pcn outputs:
        // p1234        (PID)
        // cchrome      (command name)
        // n*:8080      (listening — skip)
        // n10.0.0.1:443->192.168.1.1:54321  (connection)
        let mut current_pid: Option<u32> = None;
        let mut current_cmd: Option<String> = None;

        for line in stdout.lines() {
            if let Some(pid_str) = line.strip_prefix('p') {
                current_pid = pid_str.parse().ok();
                current_cmd = None; // Reset command for new process
            } else if let Some(cmd) = line.strip_prefix('c') {
                current_cmd = Some(cmd.to_string());
            } else if let Some(rest) = line.strip_prefix('n') {
                if rest.contains("->") {
                    connections.push(LsofConnection {
                        conn_str: rest.to_string(),
                        pid: current_pid,
                        process_name: current_cmd.clone(),
                    });
                }
            }
        }

        connections
    }
}

impl NetworkMonitor for LsofNetMonitor {
    fn start(&mut self) -> Result<(), NetworkError> {
        self.running = true;
        // Baseline — capture existing connections so we only report new ones
        let existing: HashSet<String> = self
            .get_connections()
            .into_iter()
            .map(|c| c.conn_str)
            .collect();
        self.known_connections = existing;
        self.events.clear();
        self.last_new_connection_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), NetworkError> {
        self.running = false;
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<ConnectionEvent> {
        self.poll();
        std::mem::take(&mut self.events)
    }

    fn is_running(&self) -> bool {
        self.running
    }

    fn is_idle(&self) -> bool {
        if self.last_new_connection_ms == 0 {
            return true;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        now.saturating_sub(self.last_new_connection_ms) > 3000
    }
}

/// Parse a lsof connection string like "10.0.0.1:443->192.168.1.100:54321"
/// into an honest ConnectionEvent.
fn parse_lsof_connection(
    conn: &str,
    timestamp: u64,
    pid: Option<u32>,
    process_name: Option<String>,
) -> Option<ConnectionEvent> {
    let parts: Vec<&str> = conn.split("->").collect();
    if parts.len() != 2 {
        return None;
    }

    let local = parts[0];
    let remote = parts[1];

    let local_port = parse_port(local)?;
    let remote_port = parse_port(remote)?;
    let local_ip = parse_ip(local)?;
    let remote_ip = parse_ip(remote)?;

    // Skip localhost connections
    if remote_ip == "127.0.0.1" || remote_ip == "::1" || remote_ip.starts_with("[::1]") {
        return None;
    }

    Some(ConnectionEvent {
        timestamp_ms: timestamp,
        protocol: "tcp".to_string(),
        local_addr: local_ip,
        local_port,
        remote_addr: remote_ip,
        remote_port,
        state: "ESTABLISHED".to_string(),
        service: service_for_port(remote_port).map(|s| s.to_string()),
        process_name,
        pid,
    })
}

/// Extract port from "ip:port" or "[ipv6]:port".
fn parse_port(addr: &str) -> Option<u16> {
    if let Some(bracket_end) = addr.rfind(']') {
        let port_str = &addr[bracket_end + 2..];
        return port_str.parse().ok();
    }
    addr.rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
}

/// Extract IP from "ip:port" or "[ipv6]:port".
fn parse_ip(addr: &str) -> Option<String> {
    if let Some(bracket_end) = addr.rfind(']') {
        Some(addr[1..bracket_end].to_string())
    } else {
        addr.rsplit_once(':').map(|(ip, _)| ip.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_lsof_connection_ipv4() {
        let event = parse_lsof_connection(
            "192.168.1.100:54321->93.184.216.34:443",
            1000,
            Some(42),
            Some("chrome".into()),
        );
        assert!(event.is_some());
        let e = event.unwrap();
        assert_eq!(e.remote_addr, "93.184.216.34");
        assert_eq!(e.remote_port, 443);
        assert_eq!(e.local_port, 54321);
        assert_eq!(e.service.as_deref(), Some("https"));
        assert_eq!(e.process_name.as_deref(), Some("chrome"));
        assert_eq!(e.pid, Some(42));
        assert_eq!(e.protocol, "tcp");
        assert_eq!(e.state, "ESTABLISHED");
    }

    #[test]
    fn test_parse_lsof_connection_localhost_filtered() {
        let event =
            parse_lsof_connection("127.0.0.1:3000->127.0.0.1:54321", 1000, None, None);
        assert!(event.is_none());
    }

    #[test]
    fn test_parse_lsof_connection_no_fake_http() {
        let event = parse_lsof_connection(
            "192.168.1.100:54321->93.184.216.34:80",
            1000,
            None,
            None,
        );
        assert!(event.is_some());
        let e = event.unwrap();
        // Service is "http" (from port), but no fabricated HTTP method/status
        assert_eq!(e.service.as_deref(), Some("http"));
        // These fields don't exist on ConnectionEvent — that's the point
    }

    #[test]
    fn test_parse_port() {
        assert_eq!(parse_port("192.168.1.1:443"), Some(443));
        assert_eq!(parse_port("[::1]:8080"), Some(8080));
    }

    #[test]
    fn test_parse_ip() {
        assert_eq!(parse_ip("192.168.1.1:443"), Some("192.168.1.1".to_string()));
        assert_eq!(parse_ip("[::1]:8080"), Some("::1".to_string()));
    }

    #[test]
    fn test_lsof_monitor_lifecycle() {
        let mut monitor = LsofNetMonitor::new();
        assert!(!monitor.is_running());
        monitor.start().unwrap();
        assert!(monitor.is_running());
        let _events = monitor.drain_events();
        monitor.stop().unwrap();
        assert!(!monitor.is_running());
    }

    #[test]
    fn test_lsof_monitor_idle_initially() {
        let monitor = LsofNetMonitor::new();
        assert!(monitor.is_idle());
    }

    #[test]
    fn test_lsof_monitor_not_idle_after_start() {
        let mut monitor = LsofNetMonitor::new();
        monitor.start().unwrap();
        assert!(!monitor.is_idle());
    }
}
