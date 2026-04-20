//! Data structures for the Cortex mental model.
//!
//! The MentalModel is the always-current perception state maintained by the
//! background tick loop. It is wrapped in `Arc<RwLock>` — the tick loop is the
//! sole writer, while IPC clients and Tauri commands are concurrent readers.

use cel_context::{ScreenContext, StreamStatus};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// ─── Constants ──────────────────────────────────────────────────────────────

/// Cortex tick interval in ms.
pub const TICK_INTERVAL_MS: u64 = 200;

/// How many cycles an element must survive unchanged to be "stable".
pub const STABLE_THRESHOLD: u32 = 5;

/// Max recent diffs to keep in the rolling window.
pub const MAX_RECENT_DIFFS: usize = 10;

/// Max focus trail entries.
pub const MAX_FOCUS_TRAIL: usize = 20;

/// Max anomalies in queue before oldest are dropped.
pub const MAX_ANOMALY_QUEUE: usize = 50;

/// Minimum actionable elements before vision is flagged as needed.
pub const SPARSE_CONTEXT_THRESHOLD: usize = 5;

/// Anomaly dedup window in ms (same type+description within this window is ignored).
pub const ANOMALY_DEDUP_WINDOW_MS: u64 = 5_000;

/// Anomaly TTL in ms (anomalies older than this are removed).
pub const ANOMALY_TTL_MS: u64 = 30_000;

/// Max element tracking map size before pruning.
pub const MAX_ELEMENT_TRACKING: usize = 2000;

/// Prune down to this size when cap is hit.
pub const PRUNE_ELEMENT_TARGET: usize = 1500;

// ─── Mental Model ───────────────────────────────────────────────────────────

/// The complete mental model maintained by the Cortex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MentalModel {
    /// Latest screen context from the accessibility tree.
    pub current_context: ScreenContext,
    /// Currently focused element.
    pub focused_element: Option<FocusedElement>,
    /// Rolling window of recent perception diffs.
    pub recent_diffs: VecDeque<PerceptionDiff>,
    /// Temporal pattern tracking.
    pub temporal: TemporalFlags,
    /// Element stability classification.
    pub stability: ElementStability,
    /// Detected anomalies waiting to be consumed.
    pub anomaly_queue: VecDeque<Anomaly>,
    /// Model confidence (1.0 after successful tick).
    pub confidence: f64,
    /// Whether vision (screenshot) is needed as fallback.
    pub vision_needed: bool,
    /// Milliseconds since last context update.
    pub age_ms: u64,
    /// Total perception cycles run.
    pub cycle_count: u64,
    /// Total uptime in milliseconds.
    pub uptime_ms: u64,

    // ── Adapter state ──────────────────────────────────────────────────────

    /// Maps element IDs to the adapter that sourced them.
    /// Used by the Cortex to route execution requests to the right adapter.
    #[serde(default)]
    pub element_adapter_index: HashMap<String, String>,
    /// Names of currently active adapters (apps are frontmost).
    #[serde(default)]
    pub active_adapters: Vec<String>,
    /// Which device streams are currently wired into the default cortex.
    #[serde(default)]
    pub stream_status: StreamStatus,
}

impl Default for MentalModel {
    fn default() -> Self {
        Self {
            current_context: ScreenContext {
                app: String::new(),
                window: String::new(),
                elements: vec![],
                network_events: vec![],
                http_events: vec![],
                timestamp_ms: 0,
                screen_width: None,
                screen_height: None,
                clipboard: None,
                window_list: vec![],
                audio: None,
                power: None,
                running_apps: vec![],
                recent_files: vec![],
                transcripts: vec![],
            },
            focused_element: None,
            recent_diffs: VecDeque::new(),
            temporal: TemporalFlags::default(),
            stability: ElementStability::default(),
            anomaly_queue: VecDeque::new(),
            confidence: 0.0,
            vision_needed: false,
            age_ms: 0,
            cycle_count: 0,
            uptime_ms: 0,
            element_adapter_index: HashMap::new(),
            active_adapters: Vec::new(),
            stream_status: StreamStatus::default(),
        }
    }
}

// ─── Sub-types ──────────────────────────────────────────────────────────────

/// Info about the currently focused element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusedElement {
    pub id: String,
    pub label: Option<String>,
}

/// A summarized diff between two context snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerceptionDiff {
    pub added_count: usize,
    pub removed_count: usize,
    pub changed_count: usize,
    pub unchanged_count: usize,
    /// Labels of added elements (up to 10).
    pub added_labels: Vec<String>,
    /// Labels of changed elements (up to 10).
    pub changed_labels: Vec<String>,
}

/// Temporal pattern tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemporalFlags {
    /// Loading state detection.
    pub loading: Option<LoadingState>,
    /// Error persistence tracking.
    pub error_persisting: Option<ErrorState>,
    /// Timestamp (ms since epoch) when the screen became idle.
    pub idle_since: Option<u64>,
    /// Recent focus breadcrumbs (element labels).
    pub focus_trail: VecDeque<String>,
    /// Consecutive cycles with no significant change.
    pub stagnant_cycles: u32,
}

/// Loading state info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadingState {
    pub detected: bool,
    pub duration_ms: u64,
}

/// Persistent error info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorState {
    pub detected: bool,
    pub duration_ms: u64,
    pub message: Option<String>,
}

/// Element stability classification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElementStability {
    /// Elements that have survived 5+ cycles unchanged — reliable click targets.
    pub stable: HashSet<String>,
    /// Elements seen ≤1 cycle — avoid for critical actions.
    pub volatile: HashSet<String>,
}

// ─── Anomaly types ──────────────────────────────────────────────────────────

/// The type of detected anomaly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyType {
    Dialog,
    Error,
    AppSwitch,
    AuthPrompt,
}

/// A detected anomaly in the UI state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    #[serde(rename = "type")]
    pub anomaly_type: AnomalyType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub element_ids: Vec<String>,
}

// ─── Dialog types ───────────────────────────────────────────────────────────

/// Type of dismissable dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogType {
    CookieConsent,
    NotificationPrompt,
    GenericDismiss,
}

/// A detected dismissable dialog (observe-only — cortex never auto-clicks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissableDialog {
    pub element_id: String,
    pub label: String,
    pub dialog_type: DialogType,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mental_model_default() {
        let model = MentalModel::default();
        assert_eq!(model.confidence, 0.0);
        assert_eq!(model.cycle_count, 0);
        assert!(model.anomaly_queue.is_empty());
        assert!(model.recent_diffs.is_empty());
    }

    #[test]
    fn test_anomaly_serialization() {
        let anomaly = Anomaly {
            anomaly_type: AnomalyType::Dialog,
            title: Some("Cookie Consent".into()),
            description: "A dialog appeared".into(),
            timestamp: 1000,
            element_ids: vec!["a11y:42".into()],
        };
        let json = serde_json::to_string(&anomaly).unwrap();
        assert!(json.contains("\"type\":\"dialog\""));
        assert!(json.contains("Cookie Consent"));
    }

    #[test]
    fn test_temporal_flags_default() {
        let flags = TemporalFlags::default();
        assert!(flags.loading.is_none());
        assert!(flags.error_persisting.is_none());
        assert!(flags.idle_since.is_none());
        assert_eq!(flags.stagnant_cycles, 0);
        assert!(flags.focus_trail.is_empty());
    }
}
