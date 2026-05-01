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

use cel_cortex::adapter::{AdapterDriver, AdapterError, ActionResult, AdapterManifest};
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
adapter-common = { path = "../adapter-common" }
cel-context = { path = "../../cel/cel-context" }
serde_json = "1"
```

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
