//! CEL adapter SDK — the contract third-party adapters implement.
//!
//! This crate is the *thin* surface an adapter author depends on: the
//! [`AdapterDriver`] trait, the manifest types ([`AdapterManifest`] and its
//! declarations), [`ActionResult`]/[`AdapterError`], and the
//! discovery/registration helpers. It depends only on the low-level shared
//! crates (`cel-context`, `cel-contracts`, `cel-cdp`) and NOT on `cel-cortex`,
//! so adapters no longer pull in the whole perception engine just to get the
//! trait. `cel-cortex` depends on this crate and re-exports its items, so
//! existing `cel_cortex::AdapterDriver` paths keep working.
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
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
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
    /// Directory name of the peer manifest for the same conceptual adapter
    /// in a different runtime/language. Two manifests that declare each
    /// other as their alias (bidirectional) form a logical pair —
    /// [`group_paired_manifests`] surfaces them as one adapter with two
    /// implementations. Browser perception uses this today: the TS adapter
    /// at `adapters/browser/` and the Rust adapter at `adapters/browser-rs/`
    /// alias each other. See `docs/adapters-cel-agents.md` § "Browser
    /// perception" for the unification roadmap.
    #[serde(default)]
    pub manifest_alias: Option<String>,
    /// Relative path to a partial parent manifest whose fields are layered
    /// underneath this one. `load_manifest` resolves it (relative to this
    /// manifest's directory), JSON-merges parent + child, then deserializes
    /// the result. Used to give two adapter implementations one canonical
    /// source of truth for fields that must agree across runtimes
    /// (e.g. `truth_surface`, `confidence`) while each implementation keeps
    /// its own `adapter.json` for runtime-specific overrides (entrypoint,
    /// refresh_ms, runtime).
    ///
    /// Merge semantics: objects merge recursively; arrays and scalars in
    /// the child wholly replace the parent. Unknown to the parent means
    /// the field comes through unchanged from the child.
    #[serde(default)]
    pub manifest_extends: Option<String>,
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

fn default_refresh_ms() -> u64 {
    200
}
fn default_confidence() -> f64 {
    0.95
}
fn default_truth_surface() -> String {
    "native_api".into()
}

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
    /// Per-adapter override for the ProcessDriver response timeout, in
    /// milliseconds. `None` falls back to `DEFAULT_RESPONSE_TIMEOUT_MS` in
    /// `cel/cel-cortex/src/process_driver.rs`. Raise for adapters whose
    /// native APIs are intrinsically slow (e.g., AppleScript-driven
    /// Reminders.app where a single bulk-property list call can take
    /// 5–10s on iCloud-synced accounts).
    #[serde(default)]
    pub response_timeout_ms: Option<u64>,
}

fn default_requires_frontmost() -> bool {
    true
}

impl Default for LifecycleDeclaration {
    fn default() -> Self {
        Self {
            requires_frontmost: default_requires_frontmost(),
            bootstrap_on_activate: false,
            background_refresh: false,
            response_timeout_ms: None,
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

fn default_verification_surface() -> String {
    "ui".into()
}

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
        Self {
            success: true,
            error: None,
            data: None,
        }
    }

    pub fn fail(reason: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(reason.into()),
            data: None,
        }
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

    /// Closing-gap fill: surface app-specific structured facts the
    /// planner should see in its `PlanningView.adapter_facts` slot.
    /// Called once per planner turn (alongside `get_context`) by the
    /// Cortex aggregator. Default returns empty — adapters that don't
    /// have structured facts to surface keep pre-closure behaviour.
    ///
    /// Adapters decide what's relevant for `goal` + `context`. The
    /// planner sees the union from all active adapters; no reranking.
    /// Each fact contributes one `EvidenceRef` to `view.evidence` so
    /// the planner can trace it back to its adapter.
    async fn facts_for_planning_view(
        &self,
        _goal: &str,
        _context: &cel_context::ScreenContext,
    ) -> Vec<cel_contracts::AdapterFactRef> {
        Vec::new()
    }

    /// Optional downcast hook. Returns `Some(self)` to allow external code
    /// to access concrete-type APIs (e.g. for post-construction binding of
    /// resources that the trait surface doesn't expose generically).
    ///
    /// Default `None` keeps the trait object-safe and means most adapters
    /// don't need to think about Any.
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// Optional post-construction CDP client binding. Default no-op.
    ///
    /// Adapters that perceive via Chrome DevTools Protocol (the browser-rs
    /// adapter) override this to accept a CDP client from external code —
    /// typically `Cortex::bind_browser_cdp_url` after `cel.ensureBrowser`
    /// spawn (Phase 3 of ADR-unify-browser-ownership). This avoids relying
    /// on `cel_cdp::connect_to_focused_app` discovery, which can fail for
    /// headless browsers that aren't macOS-frontmost.
    ///
    /// The trait carries this method (rather than putting it behind `as_any`)
    /// because it needs to be async — `as_any` + downcast forces callers to
    /// hold a non-`Send` `&dyn Any` across await points, which doesn't
    /// compose with the async cortex tick loop.
    async fn set_cdp_client(&self, _client: std::sync::Arc<cel_cdp::CdpClient>) {
        // Default no-op. Most adapters don't perceive via CDP.
    }
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
    /// Last successful snapshot, already tagged with cortex source/confidence.
    /// Replayed into `new_context.elements` on skip ticks (ticks where
    /// `should_read` is false) so adapter elements don't flicker out of the
    /// model every time `refresh_ms > tick_ms`. Cleared on deactivation.
    pub last_elements: Vec<ContextElement>,
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
            last_elements: Vec::new(),
        }
    }

    /// Check if this adapter's app patterns match the given app name.
    pub fn matches_app(&self, app_name: &str) -> bool {
        self.compiled_patterns
            .iter()
            .any(|re| re.is_match(app_name))
    }

    /// Check if enough ticks have passed to read context again.
    ///
    /// `ticks_since_last_read` is set to `u64::MAX` right after activation so
    /// the adapter always reads context on its first active tick — this avoids
    /// the `refresh_ms` gap that would otherwise delay the initial DOM snapshot
    /// by one or two ticks (e.g. 300 ms refresh_ms with 200 ms tick_ms means
    /// the first read is skipped twice before the threshold is met).
    /// `saturating_mul` guards against the sentinel value overflowing u64.
    pub fn should_read(&self, tick_ms: u64) -> bool {
        let refresh_ms = self.driver.manifest().context.refresh_ms;
        let elapsed = self.ticks_since_last_read.saturating_mul(tick_ms);
        elapsed >= refresh_ms
    }
}

// ── Adapter Action Projection ───────────────────────────────────────────────

/// Project a list of active adapter manifests into the structured,
/// agent-facing action catalogue used by `PlanningView.adapter_actions`.
///
/// The output is stable: adapters sort by name, actions sort by name, and
/// params use `BTreeMap`. That keeps prompts and serialized views from
/// churning just because manifest `HashMap` iteration order changed.
pub fn adapter_actions_from_manifests(
    manifests: &[AdapterManifest],
) -> Vec<cel_contracts::AdapterActionRef> {
    let mut manifests = manifests
        .iter()
        .filter(|manifest| !manifest.actions.is_empty())
        .collect::<Vec<_>>();
    manifests.sort_by(|left, right| left.name.cmp(&right.name));

    let mut out = Vec::new();
    for manifest in manifests {
        let mut action_names: Vec<&String> = manifest.actions.keys().collect();
        action_names.sort();
        for action_name in action_names {
            let decl = &manifest.actions[action_name];
            let params_schema: BTreeMap<String, String> = decl
                .params
                .iter()
                .map(|(name, type_hint)| (name.clone(), type_hint.clone()))
                .collect();
            out.push(cel_contracts::AdapterActionRef {
                adapter: manifest.name.clone(),
                action: action_name.clone(),
                params_schema,
                description: decl.description.clone(),
                mutates_state: decl.mutates_state,
                requires_verification: decl.requires_verification,
                returns_data: decl.returns_data,
            });
        }
    }
    out
}

fn prompt_description(description: &str) -> String {
    const MAX_CHARS: usize = 240;
    let compact = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_CHARS {
        return compact;
    }
    let mut truncated = compact.chars().take(MAX_CHARS).collect::<String>();
    truncated.push('…');
    truncated
}

/// Transitional renderer for prompt-only clients. New callers should prefer
/// the structured `adapter_actions_from_manifests` / `PlanningView.adapter_actions`
/// contract and render at the planner boundary.
///
/// Returned string is empty when there are no manifests or no manifest carries
/// actions; LLM-backed planners append the fragment to their system prompt
/// under an "## App-Specific Actions" heading when non-empty.
///
/// The rendering deliberately mirrors the existing action-list format in
/// `cel-planner::llm_plan_producer::NEXT_MOVE_SYSTEM_PROMPT` (one JSON
/// example per line, trailing description). Keeps prompt style consistent
/// so the LLM doesn't have to context-switch between top-level vs.
/// adapter actions.
///
/// Example output for the mail adapter:
/// ```text
///   { "type": "custom", "adapter": "mail", "action": "compose",
///     "params": { "to": "string|string[]", "subject": "string",
///                 "body": "string", ... } }
///     — Create an outgoing message. DOES NOT SEND. Returns draft_id …
/// ```
pub fn format_adapter_actions_prompt(manifests: &[AdapterManifest]) -> String {
    format_adapter_action_refs_prompt(&adapter_actions_from_manifests(manifests))
}

fn format_adapter_action_refs_prompt(actions: &[cel_contracts::AdapterActionRef]) -> String {
    let mut out = String::new();
    for action in actions {
        let example = serde_json::json!({
            "type": "custom",
            "adapter": &action.adapter,
            "action": &action.action,
            "params": &action.params_schema,
        });
        out.push_str("  ");
        out.push_str(&serde_json::to_string(&example).unwrap_or_else(|_| "{}".into()));
        out.push('\n');
        if !action.description.is_empty() {
            out.push_str("    — ");
            out.push_str(&prompt_description(&action.description));
            out.push('\n');
        }
    }
    out
}

// ── Manifest Loading ───────────────────────────────────────────────────────

/// Recursively merge two manifest JSON values. Keys present in `overlay`
/// replace or augment those in `base`: objects merge key-by-key; arrays
/// and scalars from `overlay` replace the corresponding slot in `base`
/// wholesale. Missing keys come through unchanged from the side that has
/// them.
///
/// Used by [`load_manifest`] to layer a child adapter manifest over the
/// shared parent it declares via `manifest_extends`. Exposed publicly so
/// adapters that embed their manifests (e.g. the Rust browser adapter
/// embedding both files via `include_str!`) can do the same merge at
/// construction time without re-implementing it.
pub fn merge_manifest_layers(
    base: serde_json::Value,
    overlay: serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;
    match (base, overlay) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(existing) => merge_manifest_layers(existing, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Object(b)
        }
        // Arrays and scalars: overlay wholly replaces base. Additive array
        // merge would make app_patterns / actions semantics surprising —
        // the child adapter would silently inherit patterns it doesn't
        // actually support, and removing a pattern from shared would still
        // leave it active in children that "shouldn't" need to think about
        // it.
        (_, overlay) => overlay,
    }
}

/// Load an adapter manifest from a JSON file.
///
/// If the file declares `manifest_extends`, that field is resolved as a
/// path relative to the manifest's directory, the parent file is read and
/// JSON-merged underneath (via [`merge_manifest_layers`]), and the merged
/// value is then deserialized. The parent file is loaded as raw JSON, not
/// as `AdapterManifest`, so it may legitimately omit fields the struct
/// requires (`name`, `display_name`, …) — it's a fragment, not a full
/// manifest.
pub fn load_manifest(path: &std::path::Path) -> Result<AdapterManifest, AdapterError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        AdapterError::Unavailable(format!("Failed to read {}: {e}", path.display()))
    })?;
    let mut value: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        AdapterError::ProtocolError(format!("Invalid manifest {}: {e}", path.display()))
    })?;

    if let Some(parent_rel) = value
        .get("manifest_extends")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        let parent_dir = path.parent().ok_or_else(|| {
            AdapterError::ProtocolError(format!(
                "manifest path {} has no parent directory",
                path.display()
            ))
        })?;
        let parent_path = parent_dir.join(&parent_rel);
        let parent_content = std::fs::read_to_string(&parent_path).map_err(|e| {
            AdapterError::Unavailable(format!(
                "Failed to read parent manifest {}: {e}",
                parent_path.display()
            ))
        })?;
        let parent_value: serde_json::Value =
            serde_json::from_str(&parent_content).map_err(|e| {
                AdapterError::ProtocolError(format!(
                    "Invalid parent manifest {}: {e}",
                    parent_path.display()
                ))
            })?;
        value = merge_manifest_layers(parent_value, value);
    }

    serde_json::from_value(value).map_err(|e| {
        AdapterError::ProtocolError(format!(
            "Failed to deserialize manifest {}: {e}",
            path.display()
        ))
    })
}

/// Discover all adapter manifests in a directory.
/// Scans `base_dir/*/adapter.json`.
pub fn discover_adapters(base_dir: &std::path::Path) -> Vec<(std::path::PathBuf, AdapterManifest)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return found;
    };

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

/// Group discovered manifests by their [`AdapterManifest::manifest_alias`]
/// pairing. Two manifests that name each other's directory (bidirectional)
/// land in the same group; everyone else gets a singleton group.
///
/// Used by diagnostics, dashboards, and adapter-catalogue surfaces to render
/// e.g. "Browser (TS + Rust)" as one logical adapter with two implementations
/// instead of two unrelated rows. A one-way alias (only one side declares the
/// pairing) does **not** form a group — that case is almost always a typo or
/// a stale rename, and treating it as a pair would silently mask the bug.
///
/// Group order matches the input order of each group's first member, so the
/// output is stable for callers that snapshot or diff it.
pub fn group_paired_manifests(
    found: &[(std::path::PathBuf, AdapterManifest)],
) -> Vec<Vec<&(std::path::PathBuf, AdapterManifest)>> {
    let mut consumed = vec![false; found.len()];
    let mut groups: Vec<Vec<&(std::path::PathBuf, AdapterManifest)>> = Vec::new();

    for (i, entry) in found.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        consumed[i] = true;
        let mut group = vec![entry];

        let Some(alias) = entry.1.manifest_alias.as_deref() else {
            groups.push(group);
            continue;
        };

        // Find a peer whose directory name matches our alias AND whose own
        // alias points back at our directory name. Bidirectional only —
        // a one-way reference is treated as unpaired so a typo on one side
        // is loud at discovery time instead of hidden behind a pair badge.
        let our_dir = entry.0.file_name().and_then(|n| n.to_str());
        for (j, other) in found.iter().enumerate().skip(i + 1) {
            if consumed[j] {
                continue;
            }
            let other_dir = other.0.file_name().and_then(|n| n.to_str());
            let other_alias = other.1.manifest_alias.as_deref();
            if other_dir == Some(alias) && other_alias == our_dir {
                consumed[j] = true;
                group.push(other);
                break;
            }
        }

        groups.push(group);
    }

    groups
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
    fn adapter_actions_from_manifests_are_structured_and_stable() {
        let mut beta_actions = HashMap::new();
        beta_actions.insert(
            "send".into(),
            ActionDeclaration {
                params: HashMap::from([
                    ("to".into(), "string|string[]".into()),
                    ("body".into(), "string".into()),
                ]),
                description: "Send a message".into(),
                mutates_state: true,
                requires_verification: true,
                returns_data: false,
            },
        );
        beta_actions.insert(
            "draft".into(),
            ActionDeclaration {
                params: HashMap::from([("subject".into(), "string?".into())]),
                description: "Create a draft".into(),
                mutates_state: true,
                requires_verification: false,
                returns_data: true,
            },
        );

        let mut alpha_actions = HashMap::new();
        alpha_actions.insert(
            "read".into(),
            ActionDeclaration {
                params: HashMap::new(),
                description: "Read state".into(),
                mutates_state: false,
                requires_verification: false,
                returns_data: true,
            },
        );

        let manifests = vec![
            AdapterManifest {
                name: "beta".into(),
                display_name: "Beta".into(),
                app_patterns: vec![],
                platform: vec!["macos".into()],
                runtime: "process".into(),
                entrypoint: None,
                manifest_alias: None,
                manifest_extends: None,
                context: ContextDeclaration {
                    element_types: vec![],
                    refresh_ms: 200,
                    confidence: 0.95,
                    truth_surface: "native_api".into(),
                },
                lifecycle: LifecycleDeclaration::default(),
                verification: VerificationDeclaration::default(),
                actions: beta_actions,
            },
            AdapterManifest {
                name: "alpha".into(),
                display_name: "Alpha".into(),
                app_patterns: vec![],
                platform: vec!["macos".into()],
                runtime: "process".into(),
                entrypoint: None,
                manifest_alias: None,
                manifest_extends: None,
                context: ContextDeclaration {
                    element_types: vec![],
                    refresh_ms: 200,
                    confidence: 0.95,
                    truth_surface: "native_api".into(),
                },
                lifecycle: LifecycleDeclaration::default(),
                verification: VerificationDeclaration::default(),
                actions: alpha_actions,
            },
        ];

        let actions = adapter_actions_from_manifests(&manifests);
        let ids = actions
            .iter()
            .map(|a| format!("{}:{}", a.adapter, a.action))
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["alpha:read", "beta:draft", "beta:send"]);
        let param_keys = actions[2]
            .params_schema
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(param_keys, vec!["body", "to"]);
        assert!(actions[2].mutates_state);
        assert!(actions[2].requires_verification);
        assert!(!actions[2].returns_data);
    }

    #[test]
    fn adapter_actions_prompt_uses_json_examples_and_caps_descriptions() {
        let long_description = format!("{}\n{}", "word ".repeat(80), "tail");
        let manifest = AdapterManifest {
            name: "quote\"adapter".into(),
            display_name: "Quoted".into(),
            app_patterns: vec![],
            platform: vec!["macos".into()],
            runtime: "process".into(),
            entrypoint: None,
            manifest_alias: None,
            manifest_extends: None,
            context: ContextDeclaration {
                element_types: vec![],
                refresh_ms: 200,
                confidence: 0.95,
                truth_surface: "native_api".into(),
            },
            lifecycle: LifecycleDeclaration::default(),
            verification: VerificationDeclaration::default(),
            actions: HashMap::from([(
                "act".into(),
                ActionDeclaration {
                    params: HashMap::from([("weird\"param".into(), "string".into())]),
                    description: long_description,
                    mutates_state: false,
                    requires_verification: false,
                    returns_data: false,
                },
            )]),
        };

        let prompt = format_adapter_actions_prompt(&[manifest]);
        assert!(prompt.contains(r#""adapter":"quote\"adapter""#));
        assert!(prompt.contains(r#""weird\"param":"string""#));
        assert!(prompt.contains('…'), "long descriptions should be capped");
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
            fn manifest(&self) -> &AdapterManifest {
                &self.manifest
            }
            async fn activate(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn deactivate(&mut self) -> Result<(), AdapterError> {
                Ok(())
            }
            async fn get_context(&self) -> Result<Vec<ContextElement>, AdapterError> {
                Ok(vec![])
            }
            async fn execute(
                &self,
                _: &str,
                _: serde_json::Value,
            ) -> Result<ActionResult, AdapterError> {
                Ok(ActionResult::ok())
            }
            async fn probe(&self) -> bool {
                true
            }
        }

        let manifest: AdapterManifest = serde_json::from_str(
            r#"{
            "name": "excel",
            "display_name": "Excel",
            "app_patterns": ["(?i)microsoft excel", "(?i)libreoffice calc"],
            "platform": ["macos"],
            "context": { "element_types": ["cell"] }
        }"#,
        )
        .unwrap();

        let registered = RegisteredAdapter::new(Box::new(MockDriver { manifest }));
        assert!(registered.matches_app("Microsoft Excel"));
        assert!(registered.matches_app("MICROSOFT EXCEL"));
        assert!(registered.matches_app("LibreOffice Calc"));
        assert!(!registered.matches_app("Google Chrome"));
    }

    fn make_entry(
        dir: &str,
        name: &str,
        alias: Option<&str>,
    ) -> (std::path::PathBuf, AdapterManifest) {
        let manifest = AdapterManifest {
            name: name.into(),
            display_name: name.into(),
            app_patterns: vec![],
            platform: vec!["macos".into()],
            runtime: "native".into(),
            entrypoint: None,
            manifest_alias: alias.map(str::to_string),
            manifest_extends: None,
            context: ContextDeclaration {
                element_types: vec![],
                refresh_ms: 200,
                confidence: 0.9,
                truth_surface: "ui".into(),
            },
            lifecycle: LifecycleDeclaration::default(),
            verification: VerificationDeclaration::default(),
            actions: HashMap::new(),
        };
        (std::path::PathBuf::from(format!("/x/{dir}")), manifest)
    }

    #[test]
    fn manifest_alias_round_trips_through_json() {
        // The browser TS/Rust adapters use this field today. If a future
        // refactor drops it from serde, the two adapter.json files would
        // still parse silently but lose the pairing — discovery would
        // render them as two unrelated rows. Pin the round-trip so the
        // breakage shows up here instead of at a dashboard.
        let json = r#"{
            "name": "browser",
            "display_name": "Browser (TS)",
            "app_patterns": ["(?i)chrome"],
            "platform": ["macos"],
            "manifest_alias": "browser-rs",
            "context": { "element_types": ["button"] }
        }"#;
        let m: AdapterManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.manifest_alias.as_deref(), Some("browser-rs"));
        let re: AdapterManifest =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(re.manifest_alias.as_deref(), Some("browser-rs"));
    }

    #[test]
    fn manifest_alias_defaults_to_none_for_unaliased_adapters() {
        // Existing adapters (excel, numbers, sap-gui, …) don't declare
        // an alias — make sure adding the field is backward-compatible
        // and their JSON keeps parsing without modification.
        let json = r#"{
            "name": "excel",
            "display_name": "Excel",
            "app_patterns": ["(?i)excel"],
            "platform": ["macos"],
            "context": { "element_types": ["cell"] }
        }"#;
        let m: AdapterManifest = serde_json::from_str(json).unwrap();
        assert!(m.manifest_alias.is_none());
        assert!(m.manifest_extends.is_none());
    }

    #[test]
    fn group_paired_manifests_pairs_bidirectional_aliases() {
        // The canonical case: browser/ ↔ browser-rs/ point at each other.
        // The grouper should land them in one group of two so dashboards
        // can render "Browser (TS + Rust)" as one row.
        let found = vec![
            make_entry("browser", "browser", Some("browser-rs")),
            make_entry("excel", "excel", None),
            make_entry("browser-rs", "browser", Some("browser")),
        ];
        let groups = group_paired_manifests(&found);
        assert_eq!(groups.len(), 2, "expected browser-pair + excel singleton");

        let browser_group = &groups[0];
        assert_eq!(browser_group.len(), 2);
        assert_eq!(browser_group[0].0.file_name().unwrap(), "browser");
        assert_eq!(browser_group[1].0.file_name().unwrap(), "browser-rs");

        let excel_group = &groups[1];
        assert_eq!(excel_group.len(), 1);
        assert_eq!(excel_group[0].0.file_name().unwrap(), "excel");
    }

    #[test]
    fn group_paired_manifests_does_not_pair_one_way_alias() {
        // A one-way alias is almost always a typo or a half-finished
        // rename. Pairing it would silently mask the bug behind a "two
        // implementations" badge. The grouper deliberately falls back to
        // singletons so the missing reverse-alias is loud at discovery
        // time (the diagnostic surface shows two unpaired rows where one
        // pair was expected).
        let found = vec![
            make_entry("browser", "browser", Some("browser-rs")),
            make_entry("browser-rs", "browser", None),
        ];
        let groups = group_paired_manifests(&found);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.len() == 1));
    }

    #[test]
    fn group_paired_manifests_handles_empty_and_singleton_inputs() {
        // Edge cases the grouping logic must not panic on — discovery
        // can legitimately return zero (no adapters directory) or one
        // (only excel installed) entries during MCP-server boot before
        // any browser is around.
        assert!(group_paired_manifests(&[]).is_empty());
        let single = vec![make_entry("excel", "excel", None)];
        let groups = group_paired_manifests(&single);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 1);
    }

    #[test]
    fn merge_layers_overlay_replaces_scalars_and_arrays_but_recurses_into_objects() {
        // The semantic that drives Cut B: shared fields in the parent
        // manifest persist when the child doesn't override; per-runtime
        // fields in the child wholly win. Pin both directions in one
        // case so a future refactor of merge_manifest_layers can't
        // silently swap "merge arrays" or "parent wins" semantics.
        let base = serde_json::json!({
            "name": "browser",
            "platform": ["macos", "linux"],
            "context": {
                "confidence": 0.88,
                "truth_surface": "browser_dom"
            },
            "actions": { "click": { "description": "shared click" } }
        });
        let overlay = serde_json::json!({
            "display_name": "Browser (TS)",
            "platform": ["macos", "linux", "windows"],
            "context": {
                "refresh_ms": 200,
                "element_types": ["button", "input"]
            },
            "actions": { "type": { "description": "typing" } }
        });
        let merged = merge_manifest_layers(base, overlay);

        // Scalars from parent persist when not overridden.
        assert_eq!(merged["name"], "browser");
        assert_eq!(merged["context"]["confidence"], 0.88);
        assert_eq!(merged["context"]["truth_surface"], "browser_dom");
        // Scalars from overlay added where parent had nothing.
        assert_eq!(merged["display_name"], "Browser (TS)");
        assert_eq!(merged["context"]["refresh_ms"], 200);
        // Arrays from overlay wholly replace parent's array (not concat).
        assert_eq!(merged["platform"].as_array().unwrap().len(), 3);
        assert_eq!(
            merged["context"]["element_types"].as_array().unwrap().len(),
            2
        );
        // Object key from parent persists alongside new key from overlay.
        assert!(merged["actions"]["click"].is_object());
        assert!(merged["actions"]["type"].is_object());
    }

    #[test]
    fn load_manifest_resolves_extends_relative_to_child_directory() {
        // Cut B's load semantics: declaring manifest_extends pulls the
        // parent file (relative to *this* manifest's dir) underneath. If
        // a future refactor breaks the relative-path resolution, the
        // browser-rs adapter would silently fail to inherit the shared
        // truth_surface / confidence from adapters/browser/manifest.json,
        // and downstream attribution would regress to default
        // ("native_api"). Pin the resolution with a tempdir.
        let dir = tempfile::tempdir().unwrap();
        let parent_path = dir.path().join("shared.json");
        let child_path = dir.path().join("adapter.json");
        std::fs::write(
            &parent_path,
            r#"{
                "name": "browser",
                "platform": ["macos"],
                "app_patterns": ["(?i)chrome"],
                "context": {
                    "element_types": [],
                    "confidence": 0.88,
                    "truth_surface": "browser_dom"
                }
            }"#,
        )
        .unwrap();
        std::fs::write(
            &child_path,
            r#"{
                "manifest_extends": "shared.json",
                "display_name": "Browser (Test)",
                "runtime": "native",
                "context": {
                    "element_types": ["button"],
                    "refresh_ms": 300
                }
            }"#,
        )
        .unwrap();

        let m = load_manifest(&child_path).expect("layered load should succeed");
        // Inherited from parent:
        assert_eq!(m.name, "browser");
        assert_eq!(m.app_patterns, vec!["(?i)chrome"]);
        assert_eq!(m.context.confidence, 0.88);
        assert_eq!(m.context.truth_surface, "browser_dom");
        // From child:
        assert_eq!(m.display_name, "Browser (Test)");
        assert_eq!(m.runtime, "native");
        assert_eq!(m.context.element_types, vec!["button".to_string()]);
        assert_eq!(m.context.refresh_ms, 300);
        // Loader preserves the extends pointer for traceability.
        assert_eq!(m.manifest_extends.as_deref(), Some("shared.json"));
    }

    #[test]
    fn load_manifest_works_without_extends_for_unlayered_adapters() {
        // Backward-compat: every adapter except the browser pair has a
        // self-contained adapter.json today. Adding the extends/merge
        // machinery must not regress that path — pin it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adapter.json");
        std::fs::write(
            &path,
            r#"{
                "name": "excel",
                "display_name": "Excel",
                "app_patterns": ["(?i)excel"],
                "platform": ["macos"],
                "context": { "element_types": ["cell"], "confidence": 0.95 }
            }"#,
        )
        .unwrap();
        let m = load_manifest(&path).unwrap();
        assert_eq!(m.name, "excel");
        assert_eq!(m.context.confidence, 0.95);
        assert!(m.manifest_extends.is_none());
    }
}
