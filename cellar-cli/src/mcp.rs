//! WS19 — CEL as an MCP *client*.
//!
//! `cellar mcp` registers and calls *other* MCP servers (the complement of the
//! cellar MCP *server*). A small registry persists known servers; a minimal
//! stdio JSON-RPC client connects, runs the `initialize` handshake, and lists
//! or calls tools.
//!
//! The transport is newline-delimited JSON-RPC over the child's stdin/stdout
//! (the MCP stdio transport). Blocking std I/O is used deliberately — `cellar
//! mcp` is one-shot: spawn, handshake, one request, exit.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Per-request response deadline for the stdio MCP client.
const MCP_TIMEOUT: Duration = Duration::from_secs(30);

/// A registered MCP server: the command + args to spawn it over stdio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// The on-disk registry of MCP servers.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct McpRegistry {
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

impl McpRegistry {
    fn path() -> PathBuf {
        if let Ok(p) = std::env::var("CELLAR_MCP_SERVERS") {
            return PathBuf::from(p);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".cellar").join("mcp-servers.json")
    }

    fn load() -> Result<Self> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s)
                .with_context(|| format!("parse MCP registry {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read MCP registry {}", path.display())),
        }
    }

    fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }
}

/// A minimal stdio JSON-RPC MCP client. Spawns the server, runs `initialize`,
/// and exposes `tools/list` + `tools/call`. The child is killed on drop.
struct McpClient {
    child: Child,
    stdin: ChildStdin,
    /// Lines from the server's stdout, fed by a background reader thread so
    /// `request` can apply a deadline (a hung server can't block forever).
    rx: Receiver<std::io::Result<String>>,
    next_id: i64,
    timeout: Duration,
}

impl McpClient {
    fn connect(cfg: &McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn MCP server `{}`", cfg.command))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout pipe"))?;

        // Drain stdout on a thread; `request` reads via `rx.recv_timeout`.
        let (tx, rx) = mpsc::channel::<std::io::Result<String>>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if tx.send(line).is_err() {
                    break; // receiver dropped — client is gone
                }
            }
        });

        let mut client = McpClient {
            child,
            stdin,
            rx,
            next_id: 0,
            timeout: MCP_TIMEOUT,
        };
        client.initialize()?;
        Ok(client)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{req}").context("write MCP request")?;
        self.stdin.flush().ok();

        // Wait for the response with our id — bounded by a total deadline so
        // intervening notifications can't extend the wait indefinitely.
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match self.rx.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => return Err(anyhow!("read MCP response: {e}")),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    bail!("MCP `{method}` timed out after {}s", self.timeout.as_secs())
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("MCP server closed the connection before responding to `{method}`")
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // ignore non-JSON log noise on stdout
            };
            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    bail!("MCP `{method}` error: {err}");
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn notify(&mut self, method: &str) -> Result<()> {
        let req = json!({ "jsonrpc": "2.0", "method": method });
        writeln!(self.stdin, "{req}").context("write MCP notification")?;
        self.stdin.flush().ok();
        Ok(())
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "cellar", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        self.notify("notifications/initialized")?;
        Ok(())
    }

    fn list_tools(&mut self) -> Result<Value> {
        self.request("tools/list", json!({}))
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lookup(name: &str) -> Result<McpServerConfig> {
    let reg = McpRegistry::load()?;
    reg.servers
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("no MCP server named `{name}` (see `cellar mcp list`)"))
}

// ── command handlers ────────────────────────────────────────────────────

/// `cellar mcp add <name> -- <command> [args...]`
pub fn add(name: String, command: String, args: Vec<String>) -> Result<()> {
    let mut reg = McpRegistry::load()?;
    reg.servers
        .insert(name.clone(), McpServerConfig { command, args });
    reg.save()?;
    println!("added MCP server `{name}`");
    Ok(())
}

/// `cellar mcp remove <name>`
pub fn remove(name: String) -> Result<()> {
    let mut reg = McpRegistry::load()?;
    if reg.servers.remove(&name).is_none() {
        bail!("no MCP server named `{name}`");
    }
    reg.save()?;
    println!("removed MCP server `{name}`");
    Ok(())
}

/// `cellar mcp list`
pub fn list(json_out: bool) -> Result<()> {
    let reg = McpRegistry::load()?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&reg.servers)?);
        return Ok(());
    }
    if reg.servers.is_empty() {
        println!("(no MCP servers registered — add one with `cellar mcp add`)");
        return Ok(());
    }
    for (name, cfg) in &reg.servers {
        println!("{name}\t{} {}", cfg.command, cfg.args.join(" "));
    }
    Ok(())
}

/// `cellar mcp inspect <name>` — connect + list the server's tools.
pub fn inspect(name: String, json_out: bool) -> Result<()> {
    let cfg = lookup(&name)?;
    let mut client = McpClient::connect(&cfg)?;
    let result = client.list_tools()?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if tools.is_empty() {
        println!("`{name}` exposes no tools");
        return Ok(());
    }
    println!("`{name}` tools ({}):", tools.len());
    for tool in &tools {
        let tname = tool.get("name").and_then(Value::as_str).unwrap_or("?");
        let desc = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("");
        println!("  {tname}\t{desc}");
    }
    Ok(())
}

/// `cellar mcp call <name> <tool> [--args <json>]` — call a tool.
pub fn call(name: String, tool: String, args: String, json_out: bool) -> Result<()> {
    let arguments: Value =
        serde_json::from_str(&args).with_context(|| format!("--args is not valid JSON: {args}"))?;
    let cfg = lookup(&name)?;
    let mut client = McpClient::connect(&cfg)?;
    let result = client.call_tool(&tool, arguments)?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    // Pretty-print the standard MCP content array when present.
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => {
                    println!("{}", item.get("text").and_then(Value::as_str).unwrap_or(""))
                }
                Some(other) => println!("[{other} content]"),
                None => println!("{item}"),
            }
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trips() {
        let mut reg = McpRegistry::default();
        reg.servers.insert(
            "fs".into(),
            McpServerConfig {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                ],
            },
        );
        let json = serde_json::to_string(&reg).unwrap();
        let back: McpRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.servers.len(), 1);
        assert_eq!(back.servers["fs"].command, "npx");
        assert_eq!(back.servers["fs"].args.len(), 2);
    }

    #[test]
    fn missing_args_default_empty() {
        let reg: McpRegistry =
            serde_json::from_str(r#"{ "servers": { "x": { "command": "foo" } } }"#).unwrap();
        assert!(reg.servers["x"].args.is_empty());
    }
}
