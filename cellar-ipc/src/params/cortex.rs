//! Parameter types for the `cortex.*` RPC group.
//!
//! These methods drive the daemon-hosted Cortex (Phase B of
//! `cellar-daemon-cortex.md`). The action payload is carried as untyped JSON
//! so this protocol crate stays engine-agnostic: the wire shape is
//! `cel-contracts::PlannedAction` (`tag = "type"`, snake_case), parsed by the
//! daemon at the boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Params for `cortex.act` — execute one canonical action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexActParams {
    /// The canonical action as `cel-contracts::PlannedAction` JSON,
    /// e.g. `{"type":"wait","ms":100}` or
    /// `{"type":"set_value","target_id":"dom:input:email","value":"x"}`.
    pub action: Value,
}

/// Params for `cortex.perceive.start` — begin a run scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexPerceiveStartParams {
    /// Run id stamped onto every execution receipt emitted while the run is
    /// active (Receipt-Backed Run Timeline). Renders via
    /// `cellar timeline <run_id>`.
    pub run_id: String,
}
