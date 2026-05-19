//! Process Driver — runs community adapters as child processes.
//!
//! Communicates via stdin/stdout JSON lines. Any language that can
//! read/write JSON to stdio can be a CEL adapter.
//!
//! Protocol:
//! ```text
//! ← {"method":"activate"}
//! → {"ok":true}
//!
//! ← {"method":"get_context"}
//! → {"elements":[...]}
//!
//! ← {"method":"snapshot"}
//! → {"elements":[...]}
//!
//! ← {"method":"execute","action":"write_cell","params":{...}}
//! → {"success":true}
//!
//! ← {"method":"verify_action","action":"write_cell","params":{...},"result":{...}}
//! → {"success":true,"data":{...}}
//!
//! ← {"method":"bootstrap"}
//! → {"ok":true}
//!
//! ← {"method":"deactivate"}
//! → {"ok":true}
//! ```

use async_trait::async_trait;
use cel_context::ContextElement;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::adapter::{ActionResult, AdapterDriver, AdapterError, AdapterManifest};

/// Default timeout for adapter responses in milliseconds. Adapters can
/// raise this via `LifecycleDeclaration::response_timeout_ms` for slow
/// native APIs (e.g., AppleScript-driven Reminders.app, where a single
/// `list` over an iCloud-synced account can take 5–10s).
const DEFAULT_RESPONSE_TIMEOUT_MS: u64 = 30_000;

/// Max restarts before giving up.
const MAX_RESTARTS: u32 = 3;

// ── Protocol Types ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Request {
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ContextResponse {
    #[serde(default)]
    elements: Vec<ContextElement>,
}

#[derive(Deserialize)]
struct ExecuteResponse {
    #[serde(default = "default_true")]
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AckResponse {
    #[serde(default = "default_true")]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

fn default_true() -> bool {
    true
}

// ── Process Driver ─────────────────────────────────────────────────────────

/// Runs an adapter as a child process communicating via stdin/stdout JSON lines.
pub struct ProcessDriver {
    manifest: AdapterManifest,
    adapter_dir: PathBuf,
    process: Mutex<Option<ProcessHandle>>,
    restart_count: Mutex<u32>,
}

struct ProcessHandle {
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
}

impl ProcessDriver {
    /// Create a new ProcessDriver from a manifest and adapter directory.
    pub fn new(manifest: AdapterManifest, adapter_dir: PathBuf) -> Self {
        Self {
            manifest,
            adapter_dir,
            process: Mutex::new(None),
            restart_count: Mutex::new(0),
        }
    }

    /// Spawn the adapter child process.
    async fn spawn(&self) -> Result<ProcessHandle, AdapterError> {
        let entrypoint = self
            .manifest
            .entrypoint
            .as_deref()
            .ok_or_else(|| AdapterError::Unavailable("No entrypoint in manifest".into()))?;
        let entrypoint_path = self.adapter_dir.join(entrypoint);
        let current_dir = self
            .adapter_dir
            .canonicalize()
            .unwrap_or_else(|_| self.adapter_dir.clone());
        let entrypoint_arg = entrypoint_path
            .canonicalize()
            .unwrap_or(entrypoint_path)
            .to_string_lossy()
            .into_owned();

        // Determine how to run the entrypoint based on extension
        let (cmd, args): (&str, Vec<String>) = if entrypoint.ends_with(".py") {
            ("python3", vec![entrypoint_arg.clone()])
        } else if entrypoint.ends_with(".ts") || entrypoint.ends_with(".js") {
            ("node", vec![entrypoint_arg.clone()])
        } else {
            // Assume it's a binary
            (entrypoint_arg.as_str(), vec![])
        };

        debug!(adapter = %self.manifest.name, cmd = cmd, "Spawning adapter process");

        let mut child = Command::new(cmd)
            .args(&args)
            .current_dir(current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // adapter stderr goes to CEL's stderr for debugging
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AdapterError::Unavailable(format!("Failed to spawn {cmd}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AdapterError::ProtocolError("Failed to capture stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AdapterError::ProtocolError("Failed to capture stdout".into()))?;

        Ok(ProcessHandle {
            child,
            stdin,
            reader: BufReader::new(stdout),
        })
    }

    /// Per-adapter response timeout, defaulting to `DEFAULT_RESPONSE_TIMEOUT_MS`
    /// when the manifest doesn't override it. Looks at the lifecycle
    /// declaration's `response_timeout_ms` field.
    fn response_timeout_ms(&self) -> u64 {
        self.manifest
            .lifecycle
            .response_timeout_ms
            .unwrap_or(DEFAULT_RESPONSE_TIMEOUT_MS)
    }

    /// Send a request and read the response line.
    async fn call_raw(&self, request: &Request) -> Result<String, AdapterError> {
        let mut proc = self.process.lock().await;
        let handle = proc
            .as_mut()
            .ok_or_else(|| AdapterError::Unavailable("Adapter process not running".into()))?;

        // Serialize request
        let mut line = serde_json::to_string(request)
            .map_err(|e| AdapterError::ProtocolError(format!("Serialize failed: {e}")))?;
        line.push('\n');

        // Write to stdin
        handle
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AdapterError::ProcessCrashed(format!("Write failed: {e}")))?;
        handle
            .stdin
            .flush()
            .await
            .map_err(|e| AdapterError::ProcessCrashed(format!("Flush failed: {e}")))?;

        // Read response with timeout
        let timeout_ms = self.response_timeout_ms();
        let mut response = String::new();
        let read_result = tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout_ms),
            handle.reader.read_line(&mut response),
        )
        .await;

        match read_result {
            Ok(Ok(0)) => Err(AdapterError::ProcessCrashed("EOF — adapter exited".into())),
            Ok(Ok(_)) => Ok(response),
            Ok(Err(e)) => Err(AdapterError::ProcessCrashed(format!("Read error: {e}"))),
            Err(_) => {
                // The adapter may still write its (now-stale) response after
                // we give up reading — leaving it in the pipe would desync
                // every subsequent call_raw. Kill the child so the next
                // request triggers a fresh `try_restart`, which spawns a new
                // process with a clean pipe. Without this, one slow call
                // poisons the whole session.
                if let Some(mut handle) = proc.take() {
                    let _ = handle.child.start_kill();
                }
                Err(AdapterError::Timeout(timeout_ms))
            }
        }
    }

    /// Attempt to restart the adapter process after a crash.
    async fn try_restart(&self) -> Result<(), AdapterError> {
        let mut count = self.restart_count.lock().await;
        if *count >= MAX_RESTARTS {
            return Err(AdapterError::ProcessCrashed(format!(
                "Max restarts ({MAX_RESTARTS}) exceeded"
            )));
        }
        *count += 1;
        warn!(adapter = %self.manifest.name, restart = *count, "Restarting crashed adapter");

        let handle = self.spawn().await?;
        *self.process.lock().await = Some(handle);

        // Re-activate
        let resp = self
            .call_raw(&Request {
                method: "activate".into(),
                action: None,
                params: None,
                result: None,
            })
            .await?;
        let ack: AckResponse = serde_json::from_str(&resp)
            .map_err(|e| AdapterError::ProtocolError(format!("Invalid activate response: {e}")))?;
        if !ack.ok {
            return Err(AdapterError::ActivationFailed(
                ack.error.unwrap_or_else(|| "Unknown error".into()),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl AdapterDriver for ProcessDriver {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    async fn activate(&mut self) -> Result<(), AdapterError> {
        let handle = self.spawn().await?;
        *self.process.lock().await = Some(handle);
        *self.restart_count.lock().await = 0;

        let resp = self
            .call_raw(&Request {
                method: "activate".into(),
                action: None,
                params: None,
                result: None,
            })
            .await?;

        let ack: AckResponse = serde_json::from_str(&resp)
            .map_err(|e| AdapterError::ProtocolError(format!("Invalid activate response: {e}")))?;
        if !ack.ok {
            return Err(AdapterError::ActivationFailed(
                ack.error.unwrap_or_else(|| "Unknown error".into()),
            ));
        }
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), AdapterError> {
        // Best-effort: send deactivate, then kill
        let _ = self
            .call_raw(&Request {
                method: "deactivate".into(),
                action: None,
                params: None,
                result: None,
            })
            .await;

        let mut proc = self.process.lock().await;
        if let Some(mut handle) = proc.take() {
            let _ = handle.child.kill().await;
        }
        Ok(())
    }

    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
        let result = self
            .call_raw(&Request {
                method: "get_context".into(),
                action: None,
                params: None,
                result: None,
            })
            .await;

        let resp = match result {
            Ok(r) => r,
            Err(AdapterError::ProcessCrashed(_) | AdapterError::Timeout(_)) => {
                self.try_restart().await?;
                self.call_raw(&Request {
                    method: "get_context".into(),
                    action: None,
                    params: None,
                    result: None,
                })
                .await?
            }
            Err(e) => return Err(e),
        };

        let parsed: ContextResponse = serde_json::from_str(&resp).map_err(|e| {
            AdapterError::ProtocolError(format!("Invalid get_context response: {e}"))
        })?;
        Ok(parsed.elements)
    }

    async fn execute(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError> {
        let resp = self
            .call_raw(&Request {
                method: "execute".into(),
                action: Some(action.into()),
                params: Some(params),
                result: None,
            })
            .await?;

        let parsed: ExecuteResponse = serde_json::from_str(&resp)
            .map_err(|e| AdapterError::ProtocolError(format!("Invalid execute response: {e}")))?;
        Ok(ActionResult {
            success: parsed.success,
            error: parsed.error,
            data: parsed.data,
        })
    }

    async fn probe(&self) -> bool {
        // If the process is up, we're available. Otherwise fall through
        // to the lifecycle declaration: adapters that opt into
        // `background_refresh` (e.g. AppleScript-backed mail/calendar
        // adapters that work regardless of which app is frontmost) want
        // to be spawned proactively even before the matched app comes
        // foreground — without this branch, the cortex's tick loop
        // computes `should_be_active = frontmost_match || probe() = false`
        // and leaves them Inactive forever, defeating the whole point of
        // `background_refresh`. Frontmost-gated adapters (default
        // lifecycle) preserve their old semantics: probe stays false
        // until activate() spawns the process, and activate() is gated
        // on the matching app being frontmost.
        if self.process.lock().await.is_some() {
            return true;
        }
        self.manifest.lifecycle.background_refresh
    }

    async fn bootstrap(&mut self) -> Result<(), AdapterError> {
        let resp = match self
            .call_raw(&Request {
                method: "bootstrap".into(),
                action: None,
                params: None,
                result: None,
            })
            .await
        {
            Ok(resp) => resp,
            Err(AdapterError::Timeout(ms)) => {
                return Err(AdapterError::Timeout(ms));
            }
            Err(AdapterError::ProcessCrashed(_)) => {
                self.try_restart().await?;
                self.call_raw(&Request {
                    method: "bootstrap".into(),
                    action: None,
                    params: None,
                    result: None,
                })
                .await?
            }
            Err(_) => {
                return Ok(());
            }
        };

        match serde_json::from_str::<AckResponse>(&resp) {
            Ok(ack) if ack.ok => Ok(()),
            Ok(_) => Ok(()),
            Err(_) => Ok(()),
        }
    }

    async fn snapshot(&self) -> Result<Vec<ContextElement>, AdapterError> {
        let resp = match self
            .call_raw(&Request {
                method: "snapshot".into(),
                action: None,
                params: None,
                result: None,
            })
            .await
        {
            Ok(resp) => resp,
            Err(AdapterError::ProcessCrashed(_) | AdapterError::Timeout(_)) => {
                return self.get_context().await;
            }
            Err(_) => {
                return self.get_context().await;
            }
        };

        match serde_json::from_str::<ContextResponse>(&resp) {
            Ok(parsed) => Ok(parsed.elements),
            Err(_) => self.get_context().await,
        }
    }

    async fn verify_action(
        &self,
        action: &str,
        params: &serde_json::Value,
        result: &ActionResult,
    ) -> Result<Option<ActionResult>, AdapterError> {
        let resp = match self
            .call_raw(&Request {
                method: "verify_action".into(),
                action: Some(action.into()),
                params: Some(params.clone()),
                result: Some(serde_json::to_value(result).map_err(|e| {
                    AdapterError::ProtocolError(format!("verify_action serialize failed: {e}"))
                })?),
            })
            .await
        {
            Ok(resp) => resp,
            Err(_) => return Ok(None),
        };

        match serde_json::from_str::<ExecuteResponse>(&resp) {
            Ok(parsed) => Ok(Some(ActionResult {
                success: parsed.success,
                error: parsed.error,
                data: parsed.data,
            })),
            Err(_) => Ok(None),
        }
    }
}
