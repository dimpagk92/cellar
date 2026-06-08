# Building Adapters

Adapters extend CEL with app-specific structured truth and execution. Instead of relying on accessibility trees or vision to understand Excel, an Excel adapter can use COM automation to read cell values, formulas, and sheet structure directly — with near-perfect accuracy.

Architecture rule:
- `Adapters` add app/domain truth
- `CEL` fuses that truth with AX/CDP/vision and exposes stable tool surfaces
- `Agents` consume CEL through MCP, SDKs, or other bindings

## When to Build an Adapter

Build an adapter when:
- The application has a native API (COM, scripting, CLI) that's more reliable than accessibility
- You need precision that vision/accessibility can't provide (e.g., exact cell values in a spreadsheet)
- The application has custom UI patterns that accessibility trees don't represent well

Don't build an adapter when:
- The application is web-based (CEL's browser context handles this)
- Accessibility tree coverage is sufficient for your use case
- You just need to click buttons and type text

## Adapter Architecture

```
Agent Runtime → CEL tool / SDK surface → Adapter → Native API → Application
                     ↑                    ↑
            AX/CDP/vision fusion     deterministic app truth
```

When an adapter is registered, its data is fused into the unified context alongside accessibility and vision data. Adapter elements get `source: "native_api"` and typically have the highest confidence scores (0.95+).

Numbers is the canonical example:
- keep improving generic AX so app/window/dialog handoff is strong
- use adapter-style document-model operations for cell truth
- expose deterministic reads, writes, snapshots, and verification through the adapter contract

## Adapter Interface

Every adapter implements the `AdapterDriver` contract used by Cortex:

```rust
pub trait AdapterDriver: Send + Sync {
    fn manifest(&self) -> &AdapterManifest;

    /// Called when the target app becomes relevant / frontmost.
    async fn activate(&mut self) -> Result<(), AdapterError>;

    /// Called when the app is no longer active or the cortex shuts down.
    async fn deactivate(&mut self) -> Result<(), AdapterError>;

    /// Optional deterministic setup hook.
    async fn bootstrap(&mut self) -> Result<(), AdapterError>;

    /// Read context elements from the application.
    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError>;

    /// Compact adapter-backed truth snapshot. Defaults to get_context().
    async fn snapshot(&self) -> Result<Vec<ContextElement>, AdapterError>;

    /// Execute an adapter-specific action.
    async fn execute(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError>;

    /// Optional post-action verification / readback hook.
    async fn verify_action(
        &self,
        action: &str,
        params: &serde_json::Value,
        result: &ActionResult,
    ) -> Result<Option<ActionResult>, AdapterError>;

    async fn probe(&self) -> bool;
}
```

For process adapters, the same contract is surfaced over stdio JSON methods:

- `activate`
- `deactivate`
- `bootstrap` (optional)
- `get_context`
- `snapshot` (optional; falls back to `get_context`)
- `execute`
- `verify_action` (optional; returns a normal `ActionResult`)

## Manifest Semantics

The adapter manifest is also where you describe how CEL should reason about the adapter.

- `context.truth_surface`
  Declares the primary truth source, like `native_api`, `document_model`, `browser_dom`, or `ui`.
- `lifecycle.requires_frontmost`
  True for most desktop adapters.
- `lifecycle.bootstrap_on_activate`
  Ask CEL to run `bootstrap()` immediately after activation.
- `lifecycle.background_refresh`
  Only set true when the adapter can safely read context while not frontmost.
- `verification.truth_surface`
  The authoritative post-action surface that CEL/evals should trust.
- `verification.readback_action`
  The action name that deterministically reads state back.
- `verification.snapshot_action`
  The action name that returns a compact truth snapshot for CEL context/evals.
- `actions.<name>.mutates_state`
  Mark state-changing actions clearly.
- `actions.<name>.requires_verification`
  Mark actions that should be followed by deterministic verification.
- `actions.<name>.returns_data`
  Mark actions that produce structured data useful to CEL and evals.

This is how we keep the architecture clean:

- adapters declare app truth and deterministic operations
- CEL fuses, executes, verifies, and exposes that truth
- agents remain swappable

## Example: Building a Simple Adapter

Here's a skeleton for a note-taking app adapter:

```rust
// adapters/my-notes/src/lib.rs

use cel_adapter_sdk::{AdapterDriver, AdapterError, ActionResult, AdapterManifest};
use cel_context::{ContextElement, ContextSource, Bounds, ElementState};

pub struct MyNotesAdapter {
    connected: bool,
}

impl MyNotesAdapter {
    pub fn new() -> Self {
        Self { connected: false }
    }
}

impl AdapterDriver for MyNotesAdapter {
    fn manifest(&self) -> &AdapterManifest {
        unimplemented!()
    }

    async fn activate(&mut self) -> Result<(), AdapterError> {
        self.connected = true;
        Ok(())
    }

    async fn deactivate(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        Ok(())
    }

    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
        if !self.connected {
            return Err(AdapterError::ActivationFailed("not connected".into()));
        }

        // Read data from the app's native API
        // Return it as ContextElements
        Ok(vec![
            ContextElement {
                id: "notes:title".into(),
                label: Some("Note Title".into()),
                description: None,
                element_type: "input".into(),
                value: Some("My First Note".into()),
                bounds: Some(Bounds { x: 100, y: 50, width: 400, height: 30 }),
                state: ElementState {
                    focused: true,
                    enabled: true,
                    visible: true,
                    selected: false,
                    expanded: None,
                    checked: None,
                },
                parent_id: None,
                actions: vec!["click".into(), "activate".into()],
                confidence: 0.98,  // Native API = very high confidence
                source: ContextSource::NativeApi,
            },
        ])
    }

    async fn execute(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError> {
        match action {
            "set_title" => {
                let title = params["title"].as_str()
                    .ok_or(AdapterError::ExecutionFailed("title required".into()))?;
                // Call the app's API to set the title
                Ok(ActionResult::ok())
            }
            _ => Err(AdapterError::ExecutionFailed(format!("unknown action: {action}"))),
        }
    }

    async fn probe(&self) -> bool {
        true
    }
}
```

## Registering Your Adapter

Add your adapter crate to `adapters/` and register it in the adapter registry:

```toml
# adapters/my-notes/Cargo.toml
[package]
name = "adapter-my-notes"
version = "0.1.0"

[dependencies]
# The thin adapter SDK — the AdapterDriver trait + manifest/result types.
# This does NOT pull in the cel-cortex engine.
cel-adapter-sdk = { path = "../../cel/cel-adapter-sdk" }
cel-context = { path = "../../cel/cel-context" }
async-trait = "0.1"
serde_json = "1"
```

> **Which trait?** Implement `cel_adapter_sdk::AdapterDriver` — that is the
> live contract every first-party adapter uses. You may still see a second,
> simpler `adapter_common::Adapter` trait in the tree: it predates the SDK and
> survives only for four legacy Windows-finance adapters (excel, sap-gui,
> bloomberg, metatrader) plus the NAPI registry path. Do not target it for new
> work — it will be retired into `cel-adapter-sdk`.

## Using Adapters in Workflows

From TypeScript, adapter actions are invoked through the `custom` action type:

```typescript
const step: WorkflowStep = {
  id: "set-title",
  description: "Set the note title",
  action: {
    type: "custom",
    adapter: "my-notes",
    action: "set_title",
    params: { title: "Meeting Notes" },
  },
};
```

Via MCP, use `cel_act` with the adapter-backed action surface exposed by CEL.

```json
{
  "action": "custom",
  "adapter": "my-notes",
  "action_name": "set_title",
  "params": { "title": "Meeting Notes" }
}
```

## Process Adapters in Any Language (Python example)

You don't have to write Rust. A **process adapter** is any executable that
speaks CEL's line-delimited JSON protocol over stdin/stdout. Cortex spawns it
as a child process, writes one JSON request per line to its stdin, and reads
one JSON response per line from its stdout. This is how Python, Node, Go, or a
shell script can be a first-class adapter.

### The wire protocol

- **Transport:** newline-delimited JSON (one compact JSON object per line) over
  stdin (requests in) and stdout (responses out). Write a `\n` after each
  response and **flush**. Exit when stdin reaches EOF (the parent kills you on
  shutdown).
- **stderr** is yours for logging — Cortex ignores it. Never write log lines to
  stdout; that corrupts the protocol.

Every request is `{ "method": "<name>", ... }`. Responses are method-specific:

| Request | Extra request fields | Success response |
| --- | --- | --- |
| `{"method":"activate"}` | — | `{"ok":true}` |
| `{"method":"deactivate"}` | — | `{"ok":true}` |
| `{"method":"bootstrap"}` | — | `{"ok":true}` |
| `{"method":"get_context"}` | — | `{"elements":[ <ContextElement>, … ]}` |
| `{"method":"snapshot"}` | — | `{"elements":[ … ]}` (fall back to get_context) |
| `{"method":"execute"}` | `"action":"<name>"`, `"params":{…}` | `{"success":true,"data":{…}?}` |
| `{"method":"verify_action"}` | `"action"`, `"params"`, `"result"` | `{"success":true}` or an ActionResult |

On failure: the `ok`-shaped methods return `{"ok":false,"error":"…"}`; the
`elements`-shaped methods return `{"elements":[],"error":"…"}`; `execute`
returns `{"success":false,"error":"…"}`. An unknown method returns
`{"success":false,"error":"unknown method: …"}`.

> Note: there is **no `probe` method** in the process protocol. Cortex probes a
> process adapter by liveness (is the child still running?), not by calling you.
> `requires_frontmost` + your `app_patterns` decide when you're activated.

### A `ContextElement`

`get_context` / `snapshot` return elements in CEL's native shape (snake_case
JSON). Minimum useful element:

```json
{
  "id": "mynotes:title",
  "element_type": "input",
  "label": "Note Title",
  "value": "My First Note",
  "bounds": { "x": 100, "y": 50, "width": 400, "height": 30 },
  "actions": ["click", "set_value"],
  "confidence": 0.97,
  "source": "native_api"
}
```

Convention: `id` is `{adapter_name}:{stable_native_id}`. Always include `bounds`
when you can — without them, agents can't click the element by reference and
fall back to vision. Use `confidence` 0.95–0.98 for native-API truth.

### The Python adapter

```python
#!/usr/bin/env python3
# adapters/mynotes/adapter.py — a CEL process adapter in ~40 lines.
import sys, json

def handle(req: dict) -> dict:
    method = req.get("method")
    if method in ("activate", "deactivate", "bootstrap"):
        return {"ok": True}                       # connect/disconnect your API here
    if method in ("get_context", "snapshot"):
        return {"elements": [{
            "id": "mynotes:title",
            "element_type": "input",
            "label": "Note Title",
            "value": read_title_from_app(),        # your native read
            "bounds": {"x": 100, "y": 50, "width": 400, "height": 30},
            "actions": ["set_value"],
            "confidence": 0.97,
            "source": "native_api",
        }]}
    if method == "execute":
        if req.get("action") == "set_title":
            set_title_in_app(req.get("params", {}).get("title", ""))
            return {"success": True}
        return {"success": False, "error": f"unknown action: {req.get('action')}"}
    if method == "verify_action":
        return {"success": True}                   # no extra verification opinion
    return {"success": False, "error": f"unknown method: {method}"}

def main():
    for line in sys.stdin:                          # one request per line
        line = line.strip()
        if not line:
            continue
        try:
            resp = handle(json.loads(line))
        except Exception as e:                      # never crash the loop
            resp = {"success": False, "error": f"adapter error: {e}"}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()                          # MUST flush every line

if __name__ == "__main__":
    main()
```

Its manifest tells Cortex how to launch it:

```json
// adapters/mynotes/adapter.json
{
  "name": "mynotes",
  "display_name": "My Notes",
  "app_patterns": ["(?i)my notes"],
  "platform": ["macos"],
  "runtime": "process",
  "entrypoint": "adapter.py",
  "context": { "element_types": ["input"], "truth_surface": "native_api" },
  "actions": {
    "set_title": { "params": { "title": "string" }, "description": "Set the note title", "mutates_state": true }
  }
}
```

Drop the folder under `adapters/` (or `~/.cellar/adapters/`) and Cortex's
`discover_adapters` scan picks it up — no recompile, no code change in the
engine. Make `adapter.py` executable, or set `entrypoint` to how it should be
launched.

## Troubleshooting

**My adapter never activates / its elements don't appear.**
- Check `app_patterns`: they're regexes matched against the frontmost app
  name. Test yours (`(?i)` makes it case-insensitive). If `requires_frontmost`
  is true (the default), the target app must actually be frontmost.
- Confirm discovery found it: the manifest must be `adapter.json` at the
  folder root, `runtime` must be `"process"`, and the folder must be under a
  scanned dir (`adapters/` or `~/.cellar/adapters/`).
- `get_context` must return `{"elements":[…]}` — returning a bare array, or an
  object without the `elements` key, yields zero elements.

**The child process crashes or exits immediately.**
- You almost certainly wrote a log line to **stdout**. Logs go to **stderr**;
  stdout is protocol-only. One stray `print()` corrupts the stream.
- Forgot to flush: without `sys.stdout.flush()` after each response, the parent
  blocks waiting for output and the call times out.
- An unhandled exception killed your read loop. Wrap `handle()` in try/except
  and return `{"success": false, "error": …}` instead of throwing.

**The first action times out.**
- A slow native API (AppleScript on an iCloud-synced app, COM cold-start) can
  exceed the default response window. Raise `lifecycle.response_timeout_ms` in
  the manifest (e.g. Reminders uses several seconds).

**`cel_act` says the adapter or action is unknown.**
- The `adapter` arg must equal the manifest `name` exactly; the `action_name`
  must be a key under the manifest's `actions`. Declared-but-unimplemented
  actions return `{"success": false, "error": "unknown action: …"}` from your
  `execute`.

## Existing Adapters

| Adapter | Status | Application | API |
|---------|--------|-------------|-----|
| Excel | Stubs (COM interface designed) | Microsoft Excel | COM Automation |
| SAP GUI | Stubs | SAP GUI for Windows | SAP Scripting API |
| Bloomberg | Stubs | Bloomberg Terminal | Bloomberg API |
| MetaTrader | Stubs | MetaTrader 5 | MQL5 / Manager API |

These adapters have their interfaces defined but implementation is in progress. They're a good starting point if you want to contribute.

## Tips

- **Confidence scores**: Native API elements should use 0.95-0.98 confidence. Leave room for the fusion engine to merge them with AX/CDP/vision instead of replacing the rest of the context blindly.
- **Bounds**: Always provide bounds when possible. Without bounds, the agent can't click the element.
- **Actions**: Declare all available actions in the manifest. This tells CEL and downstream agents what the adapter can do.
- **Error handling**: Return structured `AdapterError`s, not panics.
- **License**: Community adapters are MIT licensed by convention.
