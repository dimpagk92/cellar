//! Cortex-derived signals passed into the planner prompt.
//!
//! These fields are extracted by the runner from `MentalModel` and handed
//! to `Planner::plan_step` alongside the `ScreenContext`. They surface
//! information the planner can't derive from the element list alone —
//! stability, anomalies, vision-need — so the LLM can plan with awareness
//! of what Cortex thinks about its own certainty.
//!
//! Kept in `cel-planner` to avoid a `cel-planner -> cel-cortex` dependency
//! cycle (cortex already depends on planner for `PlannedAction`).

use serde::{Deserialize, Serialize};

/// Snapshot of perception signals at the moment the planner is invoked.
///
/// All fields are plain data. The runner populates them from `MentalModel`
/// and `Cortex` accessors; `Default` is "no signals available" so callers
/// that don't care can pass it through without constructing anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CortexSignals {
    /// Model confidence in its current_context, 0.0–1.0. 1.0 right after a
    /// successful tick; decays in Cortex when diffs look inconsistent.
    pub confidence: f64,

    /// Whether Cortex flagged the context as too sparse to plan against
    /// without vision backup. Runner uses this + failure history to decide
    /// whether to invoke the vision path (Phase 3C).
    pub vision_needed: bool,

    /// Loading state detected by Cortex (skeleton screen, spinner). When
    /// `Some`, the planner should generally `Wait` rather than act.
    pub loading: Option<LoadingSignal>,

    /// Count of elements Cortex classifies as stable (survived ≥
    /// STABLE_THRESHOLD ticks unchanged). Higher = safer click targets.
    pub stable_count: usize,

    /// Element IDs Cortex classifies as volatile (seen ≤1 tick). Callers
    /// are advised not to critical-path through these.
    pub volatile_ids: Vec<String>,

    /// Active anomalies (dialogs, error banners, auth prompts, app switches)
    /// that the runner has not yet consumed. Each entry is a short human
    /// description (`"dialog: Cookie Consent"`, `"error: Network failed"`).
    pub anomalies: Vec<String>,

    /// Age (ms) of the last successful cortex tick, or None if no tick has
    /// fired yet. Always-on refresh (Phase 2) keeps this <300ms in practice.
    pub tick_age_ms: Option<u64>,
}

/// Compact loading signal for the prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadingSignal {
    /// How long Cortex has been observing the loading state.
    pub duration_ms: u64,
}
