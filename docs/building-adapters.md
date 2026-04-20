# Building Adapters

Adapters extend CEL to work with specific applications through their native APIs. Instead of relying on accessibility trees or vision to understand Excel, an Excel adapter uses COM automation to read cell values, formulas, and sheet structure directly — with near-perfect accuracy.

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
Agent → cel-context (fusion) → Adapter → Native API → Application
                ↑                  ↑
         accessibility tree    deterministic data
```

When an adapter is registered, its data is fused into the unified context alongside accessibility and vision data. Adapter elements get `source: "native_api"` and typically have the highest confidence scores (0.95+).

## Adapter Interface

Every adapter implements the `AdapterTrait` from `adapters/adapter-common/`:

```rust
pub trait AdapterTrait: Send + Sync {
    /// Check if this adapter's application is available on the system.
    fn is_available(&self) -> bool;

    /// Connect to the application.
    fn connect(&mut self) -> Result<(), AdapterError>;

    /// Disconnect from the application.
    fn disconnect(&mut self) -> Result<(), AdapterError>;

    /// Read context elements from the application.
    fn get_elements(&self) -> Result<Vec<ContextElement>, AdapterError>;

    /// Execute an adapter-specific action.
    fn execute_action(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError>;
}
```

## Example: Building a Simple Adapter

Here's a skeleton for a note-taking app adapter:

```rust
// adapters/my-notes/src/lib.rs

use adapter_common::{AdapterTrait, AdapterError};
use cel_context::{ContextElement, ContextSource, Bounds, ElementState};

pub struct MyNotesAdapter {
    connected: bool,
}

impl MyNotesAdapter {
    pub fn new() -> Self {
        Self { connected: false }
    }
}

impl AdapterTrait for MyNotesAdapter {
    fn is_available(&self) -> bool {
        // Check if the app is installed/running
        std::process::Command::new("my-notes")
            .arg("--version")
            .output()
            .is_ok()
    }

    fn connect(&mut self) -> Result<(), AdapterError> {
        // Initialize connection to the app's API
        self.connected = true;
        Ok(())
    }

    fn disconnect(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        Ok(())
    }

    fn get_elements(&self) -> Result<Vec<ContextElement>, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
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

    fn execute_action(
        &mut self,
        action: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError> {
        match action {
            "set_title" => {
                let title = params["title"].as_str()
                    .ok_or(AdapterError::InvalidParams("title required".into()))?;
                // Call the app's API to set the title
                Ok(serde_json::json!({ "success": true }))
            }
            _ => Err(AdapterError::UnknownAction(action.into())),
        }
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

Via MCP, use `cel_action`:

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

- **Confidence scores**: Native API elements should use 0.95-0.98 confidence. Leave room for the fusion engine to boost to 0.95 when vision confirms.
- **Bounds**: Always provide bounds when possible. Without bounds, the agent can't click the element.
- **Actions**: Declare all available actions in the `actions` field. This tells the agent what it can do.
- **Error handling**: Return `AdapterError::NotConnected` if the app isn't available, not a panic.
- **License**: Community adapters are MIT licensed by convention.
