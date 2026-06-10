//! Result types for the `cortex.*` RPC group.
//!
//! Payloads are untyped JSON so this protocol crate stays engine-agnostic;
//! the wire shapes are owned by the engine crates (`cel-context`
//! `ScreenContext`, `cel-adapter-sdk` `ActionResult`, the cortex mental
//! model) and already round-trip through serde.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Result of `cortex.see` — the current fused screen context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexSeeResult {
    /// `cel-context::ScreenContext` JSON (app, window, elements, …).
    pub context: Value,
}

/// Result of `cortex.act` — mirrors the engine's `ActionResult`.
///
/// The canonical core-emitted `ExecutionReceipt` rides on
/// `data._cel_receipt` (Receipt-Backed Run Timeline), exactly as it does on
/// the in-process surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexActResult {
    /// Whether the dispatch succeeded.
    pub success: bool,
    /// Failure reason when `success` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Action payload; carries `_cel_receipt` when the cortex emitted one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Result of `cortex.perceive.read` — a snapshot of the Cortex mental model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexPerceiveReadResult {
    /// The cortex `MentalModel` JSON (current context, diffs, freshness, …).
    pub model: Value,
}
