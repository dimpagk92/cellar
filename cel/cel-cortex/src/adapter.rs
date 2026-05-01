//! Adapter system — Cortex drivers for app-specific I/O.
//!
//! Adapters are plugins that extend the Cortex's perception and execution
//! capabilities for specific applications. Each adapter:
//! 1. Declares its capabilities via a manifest (app patterns, element types, actions)
//! 2. Provides context elements when queried (get_context → ContextElement[])
//! 3. Executes actions when dispatched (execute → ActionResult)
//!
//! Three runtimes:
//! - Native (Rust): implements AdapterDriver trait directly, in-process, 0ms overhead
//! - Process: child process communicating via stdin/stdout JSON lines, any language
//! - WASM: wasmtime sandbox (future)

use async_trait::async_trait;
use cel_context::ContextElement;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

// ── Errors ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("Adapter not available: {0}")]
    Unavailable(String),
    #[error("Activation failed: {0}")]
    ActivationFailed(String),
    #[error("Context read failed: {0}")]
    ContextReadFailed(String),
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),
    #[error("Protocol error: {0}")]
    ProtocolError(String),
    #[error("Timeout after {0}ms")]
    Timeout(u64),
    #[error("Process crashed: {0}")]
    ProcessCrashed(String),
}

// ── Manifest ───────────────────────────────────────────────────────────────

/// Declares what an adapter can see and do.
/// Loaded from `adapter.json` in the adapter's directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterManifest {
    /// Unique adapter name (e.g., "excel", "sap-gui").
    pub name: String,
    /// Human-readable display name (e.g., "Microsoft Excel").
    pub display_name: String,
    /// Regex patterns matched against the frontmost app name.
    /// When any pattern matches, the Cortex activates this adapter.
    pub app_patterns: Vec<String>,
    /// Supported platforms.
    pub platform: Vec<String>,
    /// Runtime type: "native", "process", or "wasm".
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Entrypoint for process/wasm runtimes (e.g., "adapter.py").
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// Context capabilities.
    pub context: ContextDeclaration,
    /// Lifecycle semantics for activation/bootstrap.
    #[serde(default)]
    pub lifecycle: LifecycleDeclaration,
    /// Verification semantics for adapter truth.
    #[serde(default)]
    pub verification: VerificationDeclaration,
    /// Available actions.
    #[serde(default)]
    pub actions: HashMap<String, ActionDeclaration>,
}

fn default_runtime() -> String {
    "process".into()
}

/// Declares what context elements an adapter provides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextDeclaration {
    /// Element types this adapter produces (e.g., "cell", "sheet_tab").
    pub element_types: Vec<String>,
    /// How often to query the adapter, in milliseconds.
    /// Defaults to the Cortex tick interval (200ms).
    #[serde(default = "default_refresh_ms")]
    pub refresh_ms: u64,
    /// Base confidence for adapter-sourced elements (0.0-1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Which truth surface this adapter primarily contributes.
    /// Examples: "native_api", "document_model", "browser_dom", "ui".
    #[serde(default = "default_truth_surface")]
    pub truth_surface: String,
}

fn default_refresh_ms() -> u64 { 200 }
fn default_confidence() -> f64 { 0.95 }
fn default_truth_surface() -> String { "native_api".into() }

/// Declares activation/bootstrap expectations for an adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleDeclaration {
    /// Whether the app should normally be frontmost for this adapter to activate.
    #[serde(default = "default_requires_frontmost")]
    pub requires_frontmost: bool,
    /// Whether CEL should call `bootstrap()` right after a successful activation.
    #[serde(default)]
    pub bootstrap_on_activate: bool,
    /// Whether the adapter can keep contributing context while not frontmost.
    #[serde(default)]
    pub background_refresh: bool,
}

fn default_requires_frontmost() -> bool { true }

impl Default for LifecycleDeclaration {
    fn default() -> Self {
        Self {
            requires_frontmost: default_requires_frontmost(),
            bootstrap_on_activate: false,
            background_refresh: false,
        }
    }
}

/// Declares how CEL should verify adapter-backed truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationDeclaration {
    /// Which surface should be treated as authoritative for this adapter.
    #[serde(default = "default_verification_surface")]
    pub truth_surface: String,
    /// Optional action name that reads authoritative state back.
    #[serde(default)]
    pub readback_action: Option<String>,
    /// Optional action name that returns a compact state snapshot.
    #[serde(default)]
    pub snapshot_action: Option<String>,
}

impl Default for VerificationDeclaration {
    fn default() -> Self {
        Self {
            truth_surface: default_verification_surface(),
            readback_action: None,
            snapshot_action: None,
        }
    }
}

fn default_verification_surface() -> String { "ui".into() }

/// Declares an action the adapter can execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDeclaration {
    /// Parameter names and their types.
    #[serde(default)]
    pub params: HashMap<String, String>,
    /// Human-readable description for the planner prompt.
    #[serde(default)]
    pub description: String,
    /// Whether this action mutates application state.
    #[serde(default)]
    pub mutates_state: bool,
    /// Whether CEL should ask the adapter for a stronger verification/readback.
    #[serde(default)]
    pub requires_verification: bool,
    /// Whether the action returns structured data that can feed CEL context/evals.
    #[serde(default)]
    pub returns_data: bool,
}

// ── Action Result ──────────────────────────────────────────────────────────

/// Result of an adapter action execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

impl ActionResult {
    pub fn ok() -> Self {
        Self { success: true, error: None, data: None }
    }

    pub fn fail(reason: impl Into<String>) -> Self {
        Self { success: false, error: Some(reason.into()), data: None }
    }
}

// ── Driver Trait ───────────────────────────────────────────────────────────

/// The interface all adapters implement, regardless of language or runtime.
///
/// Native Rust adapters implement this directly.
/// Process-based adapters use ProcessDriver which implements this via stdio.
#[async_trait]
pub trait AdapterDriver: Send + Sync {
    /// Get the adapter's manifest.
    fn manifest(&self) -> &AdapterManifest;

    /// Connect to the target application's API.
    /// Called when the Cortex detects the app is frontmost.
    async fn activate(&mut self) -> Result<(), AdapterError>;

    /// Disconnect from the target application.
    /// Called when the app loses focus or the Cortex shuts down.
    async fn deactivate(&mut self) -> Result<(), AdapterError>;

    /// Optional post-activation hook for deterministic setup. Adapters can use
    /// this to create/open a scratch document or otherwise prepare app state.
    async fn bootstrap(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Read context elements from the application.
    /// Called on each Cortex tick (or at the adapter's declared refresh_ms).
    /// Returns elements in CEL's native ContextElement format.
    async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError>;

    /// Return a compact snapshot that should be treated as adapter-backed truth
    /// for CEL context and eval surfaces. Defaults to `get_context()`.
    async fn snapshot(&self) -> Result<Vec<ContextElement>, AdapterError> {
        self.get_context().await
    }

    /// Execute a named action on the application.
    async fn execute(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, AdapterError>;

    /// Optional verification hook. When an action declares
    /// `requires_verification`, CEL may call this to get a stronger verdict
    /// or a deterministic readback after the base `execute`.
    async fn verify_action(
        &self,
        _action: &str,
        _params: &serde_json::Value,
        _result: &ActionResult,
    ) -> Result<Option<ActionResult>, AdapterError> {
        Ok(None)
    }

    /// Check if the target application is running and reachable.
    async fn probe(&self) -> bool;
}

// ── Adapter State ──────────────────────────────────────────────────────────

/// Lifecycle state of a registered adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterState {
    /// Registered but not connected to the target app.
    Inactive,
    /// Currently connected and providing context.
    Active,
    /// Failed to activate or encountered an error.
    Error,
}

/// A registered adapter with its runtime state.
pub struct RegisteredAdapter {
    pub driver: Box<dyn AdapterDriver>,
    pub state: AdapterState,
    /// Compiled regex patterns for fast app matching.
    pub compiled_patterns: Vec<regex::Regex>,
    /// Tick counter for refresh rate limiting.
    pub ticks_since_last_read: u64,
}

impl RegisteredAdapter {
    pub fn new(driver: Box<dyn AdapterDriver>) -> Self {
        let patterns: Vec<regex::Regex> = driver
            .manifest()
            .app_patterns
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();

        Self {
            driver,
            state: AdapterState::Inactive,
            compiled_patterns: patterns,
            ticks_since_last_read: 0,
        }
    }

    /// Check if this adapter's app patterns match the given app name.
    pub fn matches_app(&self, app_name: &str) -> bool {
        self.compiled_patterns.iter().any(|re| re.is_match(app_name))
    }

    /// Check if enough ticks have passed to read context again.
    pub fn should_read(&self, tick_ms: u64) -> bool {
        let refresh_ms = self.driver.manifest().context.refresh_ms;
        let elapsed = self.ticks_since_last_read * tick_ms;
        elapsed >= refresh_ms
    }
}

// ── Manifest Loading ───────────────────────────────────────────────────────

/// Load an adapter manifest from a JSON file.
pub fn load_manifest(path: &std::path::Path) -> Result<AdapterManifest, AdapterError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        AdapterError::Unavailable(format!("Failed to read {}: {e}", path.display()))
    })?;
    serde_json::from_str(&content).map_err(|e| {
        AdapterError::ProtocolError(format!("Invalid manifest {}: {e}", path.display()))
    })
}

/// Discover all adapter manifests in a directory.
/// Scans `base_dir/*/adapter.json`.
pub fn discover_adapters(base_dir: &std::path::Path) -> Vec<(std::path::PathBuf, AdapterManifest)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(base_dir) else { return found };

    for entry in entries.flatten() {
        let manifest_path = entry.path().join("adapter.json");
        if manifest_path.exists() {
            if let Ok(manifest) = load_manifest(&manifest_path) {
                found.push((entry.path(), manifest));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_deserialize() {
        let json = r#"{
            "name": "excel",
            "display_name": "Microsoft Excel",
            "app_patterns": ["Microsoft Excel", "LibreOffice Calc"],
            "platform": ["macos", "windows"],
            "context": {
                "element_types": ["cell", "sheet_tab"],
                "confidence": 0.95
            },
            "actions": {
                "write_cell": {
                    "params": {"row": "number", "col": "number", "value": "string"},
                    "description": "Write a value to a cell"
                }
            }
        }"#;
        let manifest: AdapterManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "excel");
        assert_eq!(manifest.app_patterns.len(), 2);
        assert_eq!(manifest.context.confidence, 0.95);
        assert_eq!(manifest.context.truth_surface, "native_api");
        assert!(manifest.actions.contains_key("write_cell"));
        assert!(manifest.lifecycle.requires_frontmost);
        assert!(!manifest.lifecycle.bootstrap_on_activate);
        assert_eq!(manifest.verification.truth_surface, "ui");
        assert_eq!(manifest.runtime, "process"); // default
    }

    #[test]
    fn test_action_result() {
        let ok = ActionResult::ok();
        assert!(ok.success);
        assert!(ok.error.is_none());

        let fail = ActionResult::fail("bad target");
        assert!(!fail.success);
        assert_eq!(fail.error.as_deref(), Some("bad target"));
    }

    #[test]
    fn test_registered_adapter_matches_app() {
        // Create a minimal mock
        struct MockDriver {
            manifest: AdapterManifest,
        }

        #[async_trait]
        impl AdapterDriver for MockDriver {
            fn manifest(&self) -> &AdapterManifest { &self.manifest }
            async fn activate(&mut self) -> Result<(), AdapterError> { Ok(()) }
            async fn deactivate(&mut self) -> Result<(), AdapterError> { Ok(()) }
            async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> { Ok(vec![]) }
            async fn execute(&self, _: &str, _: serde_json::Value) -> Result<ActionResult, AdapterError> {
                Ok(ActionResult::ok())
            }
            async fn probe(&self) -> bool { true }
        }

        let manifest: AdapterManifest = serde_json::from_str(r#"{
            "name": "excel",
            "display_name": "Excel",
            "app_patterns": ["(?i)microsoft excel", "(?i)libreoffice calc"],
            "platform": ["macos"],
            "context": { "element_types": ["cell"] }
        }"#).unwrap();

        let registered = RegisteredAdapter::new(Box::new(MockDriver { manifest }));
        assert!(registered.matches_app("Microsoft Excel"));
        assert!(registered.matches_app("MICROSOFT EXCEL"));
        assert!(registered.matches_app("LibreOffice Calc"));
        assert!(!registered.matches_app("Google Chrome"));
    }
}
