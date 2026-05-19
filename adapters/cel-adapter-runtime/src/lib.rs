//! Shared stdio loop for ProcessDriver adapters.
//!
//! Each adapter binary's `main.rs` is a 4-line wrapper:
//!
//! ```ignore
//! use cel_adapter_runtime::run_stdio_loop;
//! use adapter_mail::MailAdapter;
//!
//! fn main() {
//!     run_stdio_loop(MailAdapter::new());
//! }
//! ```
//!
//! This crate handles the JSON-line protocol (matching
//! `cel/cel-cortex/src/process_driver.rs` § Protocol) so individual
//! adapter crates stay focused on the AdapterDriver impl. The protocol
//! is read-line / write-line over stdin/stdout, JSON encoded.
//!
//! ## Why a shared runtime instead of per-adapter boilerplate
//!
//! Every ProcessDriver adapter speaks the same protocol — `activate`,
//! `deactivate`, `get_context`, `snapshot`, `execute(action, params)`,
//! `verify_action(action, params, result)`, `bootstrap`. Without this
//! crate, each adapter binary would carry ~120 lines of identical
//! dispatch + serialization code. With it, the binary collapses to a
//! single call and the AdapterDriver impl in the adapter's `lib.rs`
//! remains the only thing the adapter author writes.

use cel_cortex::{ActionResult, AdapterDriver, AdapterError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

#[derive(Deserialize)]
struct Request {
    method: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
}

/// Drive an AdapterDriver impl as a stdio-JSON ProcessDriver adapter.
///
/// Reads JSON-line requests from stdin, dispatches them to the supplied
/// adapter, and writes JSON-line responses to stdout. Exits when stdin
/// closes (parent process kills the child during deactivate).
pub fn run_stdio_loop<D>(mut adapter: D)
where
    D: AdapterDriver + Send + 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime for adapter stdio loop");

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    rt.block_on(async {
        let reader = stdin.lock();
        let mut out = stdout.lock();
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Request>(trimmed) {
                Ok(req) => handle(&mut adapter, req).await,
                Err(e) => json!({
                    "success": false,
                    "error": format!("invalid request JSON: {e}"),
                }),
            };
            // Best-effort write; if stdout is closed (parent killed us)
            // the next loop iteration will error on stdin read and exit.
            if writeln!(out, "{resp}").is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
        }
    });
}

async fn handle<D: AdapterDriver>(adapter: &mut D, req: Request) -> Value {
    match req.method.as_str() {
        "activate" => match adapter.activate().await {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": err_to_string(&e) }),
        },
        "deactivate" => match adapter.deactivate().await {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": err_to_string(&e) }),
        },
        "bootstrap" => match adapter.bootstrap().await {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": err_to_string(&e) }),
        },
        "get_context" => match adapter.get_context().await {
            Ok(elements) => json!({ "elements": elements }),
            Err(e) => json!({ "elements": Vec::<Value>::new(), "error": err_to_string(&e) }),
        },
        "snapshot" => match adapter.snapshot().await {
            Ok(elements) => json!({ "elements": elements }),
            Err(e) => json!({ "elements": Vec::<Value>::new(), "error": err_to_string(&e) }),
        },
        "execute" => {
            let action = req.action.unwrap_or_default();
            let params = req.params.unwrap_or(Value::Null);
            match adapter.execute(&action, params).await {
                Ok(result) => action_result_to_json(&result),
                Err(e) => json!({ "success": false, "error": err_to_string(&e) }),
            }
        }
        "verify_action" => {
            let action = req.action.unwrap_or_default();
            let params = req.params.unwrap_or(Value::Null);
            let original_result_value = req.result.unwrap_or(Value::Null);
            // Best-effort: synthesize an ActionResult from the parent's
            // posted `result` for the adapter's verify hook to look at.
            // If parsing fails we still call verify with a minimal
            // success=true sentinel so the adapter can decide.
            let original_result = serde_json::from_value::<ActionResult>(original_result_value)
                .unwrap_or_else(|_| ActionResult::ok());
            match adapter.verify_action(&action, &params, &original_result).await {
                Ok(Some(verified)) => action_result_to_json(&verified),
                // None => signal "no verification opinion"; the cortex
                // ProcessDriver maps any non-ExecuteResponse-shaped
                // response back to None, so an `ok: true` here is fine
                // and tells parent verification ran but returned no diff.
                Ok(None) => json!({ "success": true }),
                Err(e) => json!({ "success": false, "error": err_to_string(&e) }),
            }
        }
        other => json!({
            "success": false,
            "error": format!("unknown method: {other}"),
        }),
    }
}

fn action_result_to_json(result: &ActionResult) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("success".into(), Value::Bool(result.success));
    if let Some(err) = &result.error {
        obj.insert("error".into(), Value::String(err.clone()));
    }
    if let Some(data) = &result.data {
        obj.insert("data".into(), data.clone());
    }
    Value::Object(obj)
}

fn err_to_string(e: &AdapterError) -> String {
    e.to_string()
}
