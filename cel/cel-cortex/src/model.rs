//! Data structures for the Cortex mental model.
//!
//! The MentalModel is the always-current perception state maintained by the
//! background tick loop. It is wrapped in `Arc<RwLock>` — the tick loop is the
//! sole writer, while IPC clients and Tauri commands are concurrent readers.

use cel_context::{ContextSource, ScreenContext, StreamStatus};
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

/// Age threshold where the model becomes soft-stale.
pub const SOFT_STALE_MS: u64 = 1_500;

/// Age threshold where the model becomes hard-stale.
pub const HARD_STALE_MS: u64 = 5_000;

/// Confidence threshold where the model becomes soft-stale.
pub const SOFT_STALE_CONFIDENCE: f64 = 0.75;

/// Confidence threshold where the model becomes hard-stale.
pub const HARD_STALE_CONFIDENCE: f64 = 0.4;

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
    /// Current freshness classification for routing decisions.
    #[serde(default)]
    pub freshness: Option<FreshnessAssessment>,
    /// Last diff summary, if a meaningful diff was recently observed.
    #[serde(default)]
    pub last_diff_summary: Option<DiffSummary>,
    /// High-level heuristic interpretation of the current device state.
    #[serde(default)]
    pub semantic: Option<SemanticInsight>,
    /// Current source coverage for the fused context.
    #[serde(default)]
    pub source_summary: Option<SourceSummary>,
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
            freshness: None,
            last_diff_summary: None,
            semantic: None,
            source_summary: None,
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

/// Simplified diff summary kept in the mental model for consumers that don't
/// need the full rolling diff history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub added_count: usize,
    pub removed_count: usize,
    pub changed_count: usize,
    pub unchanged_count: usize,
}

/// Freshness state used by routing and inspection surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Fresh,
    SoftStale,
    HardStale,
}

/// Why the model is considered stale.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StalenessCause {
    Time,
    Event,
    Confidence,
    Verification,
}

/// Current freshness classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshnessAssessment {
    pub state: FreshnessState,
    pub causes: Vec<StalenessCause>,
    pub age_ms: u64,
    pub confidence: f64,
    pub last_update_ms: u64,
    pub last_event_ms: Option<u64>,
    pub last_significant_event_ms: Option<u64>,
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

/// High-level task phase inferred from the current fused device state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Idle,
    Navigation,
    Input,
    Review,
    Loading,
    Blocked,
}

/// Lightweight semantic interpretation of the current model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticInsight {
    pub current_activity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_transition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likely_blocker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_next_step: Option<String>,
    pub task_phase: TaskPhase,
}

/// Summary of which context sources contributed to the current fused snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceSummary {
    pub accessibility: usize,
    pub native_api: usize,
    pub vision: usize,
    pub merged: usize,
    pub adapter_backed: usize,
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

impl MentalModel {
    pub fn refresh_derived(
        &mut self,
        now_ms: u64,
        last_event_ms: Option<u64>,
        last_significant_event_ms: Option<u64>,
    ) {
        let last_update_ms = self.current_context.timestamp_ms;
        self.age_ms = now_ms.saturating_sub(last_update_ms);

        let mut causes: HashSet<StalenessCause> = HashSet::new();
        let mut state = FreshnessState::Fresh;
        if let Some(ts) = last_significant_event_ms {
            if ts >= last_update_ms && ts > 0 {
                causes.insert(StalenessCause::Event);
                state = FreshnessState::HardStale;
            }
        }
        if self.age_ms >= HARD_STALE_MS {
            causes.insert(StalenessCause::Time);
            state = FreshnessState::HardStale;
        } else if self.age_ms >= SOFT_STALE_MS && state != FreshnessState::HardStale {
            causes.insert(StalenessCause::Time);
            state = FreshnessState::SoftStale;
        }
        if self.confidence <= HARD_STALE_CONFIDENCE {
            causes.insert(StalenessCause::Confidence);
            state = FreshnessState::HardStale;
        } else if self.confidence <= SOFT_STALE_CONFIDENCE {
            causes.insert(StalenessCause::Confidence);
            if state == FreshnessState::Fresh {
                state = FreshnessState::SoftStale;
            }
        }
        self.freshness = Some(FreshnessAssessment {
            state,
            causes: causes.into_iter().collect(),
            age_ms: self.age_ms,
            confidence: self.confidence,
            last_update_ms,
            last_event_ms,
            last_significant_event_ms,
        });

        self.last_diff_summary = self.recent_diffs.back().map(|diff| DiffSummary {
            added_count: diff.added_count,
            removed_count: diff.removed_count,
            changed_count: diff.changed_count,
            unchanged_count: diff.unchanged_count,
        });

        let mut source_summary = SourceSummary::default();
        for element in &self.current_context.elements {
            match element.source {
                ContextSource::AccessibilityTree => source_summary.accessibility += 1,
                ContextSource::NativeApi => source_summary.native_api += 1,
                ContextSource::Vision => source_summary.vision += 1,
                ContextSource::Merged => source_summary.merged += 1,
            }
        }
        source_summary.adapter_backed = self.element_adapter_index.len();
        self.source_summary = Some(source_summary);

        let current_activity = {
            let mut parts = Vec::new();
            if self.current_context.app.is_empty() {
                parts.push("Reading the current device state".to_string());
            } else {
                parts.push(format!("Using {}", self.current_context.app));
            }
            if !self.current_context.window.is_empty()
                && self.current_context.window != self.current_context.app
            {
                parts.push(format!("in {}", self.current_context.window));
            }
            if let Some(focused) = &self.focused_element {
                if let Some(label) = &focused.label {
                    parts.push(format!("focused on {}", label));
                }
            }
            parts.join(" ")
        };

        let recent_transition = self
            .anomaly_queue
            .iter()
            .rev()
            .find(|anomaly| anomaly.anomaly_type == AnomalyType::AppSwitch)
            .map(|anomaly| anomaly.description.clone())
            .or_else(|| {
                let len = self.temporal.focus_trail.len();
                if len >= 2 {
                    let previous = self.temporal.focus_trail.get(len - 2)?;
                    let current = self.temporal.focus_trail.get(len - 1)?;
                    if previous != current {
                        Some(format!("Focus moved from {} to {}.", previous, current))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .or_else(|| {
                self.last_diff_summary.as_ref().and_then(|diff| {
                    let changed_total = diff.added_count + diff.removed_count + diff.changed_count;
                    if changed_total > 0 {
                        Some(format!(
                            "Context changed (+{} / -{} / ~{}).",
                            diff.added_count, diff.removed_count, diff.changed_count
                        ))
                    } else {
                        None
                    }
                })
            });

        let first_anomaly = self.anomaly_queue.front();
        let likely_blocker = first_anomaly
            .map(|anomaly| anomaly.description.clone())
            .or_else(|| {
                self.temporal.error_persisting.as_ref().map(|error| {
                    error
                        .message
                        .clone()
                        .map(|message| format!("Persistent error: {}", message))
                        .unwrap_or_else(|| "A persistent error is still visible.".to_string())
                })
            })
            .or_else(|| {
                self.temporal.loading.as_ref().and_then(|loading| {
                    if loading.detected && loading.duration_ms >= 1_500 {
                        Some(format!(
                            "The UI is still loading ({}s).",
                            (loading.duration_ms / 1_000).max(1)
                        ))
                    } else {
                        None
                    }
                })
            })
            .or_else(|| {
                if self.vision_needed {
                    Some(
                        "Structured streams are still sparse; a richer read may be needed."
                            .to_string(),
                    )
                } else {
                    None
                }
            });

        let focused_input = self.focused_element.as_ref().and_then(|focused| {
            self.current_context
                .elements
                .iter()
                .find(|element| element.id == focused.id)
                .filter(|element| {
                    matches!(
                        element.element_type.as_str(),
                        "input" | "textarea" | "textfield" | "searchfield" | "combobox" | "select"
                    )
                })
        });
        let first_actionable = self.current_context.elements.iter().find(|element| {
            element.state.enabled
                && element.state.visible
                && !element.actions.is_empty()
                && element.label.is_some()
        });

        let suggested_next_step = if let Some(anomaly) = first_anomaly {
            match anomaly.anomaly_type {
                AnomalyType::Dialog | AnomalyType::AuthPrompt => {
                    Some("Handle the blocking dialog or prompt before continuing.".to_string())
                }
                _ => Some("Inspect the anomaly before continuing.".to_string()),
            }
        } else if self.temporal.error_persisting.is_some() {
            Some("Acknowledge the error, then retry or backtrack.".to_string())
        } else if self.temporal.loading.is_some() {
            Some("Wait for the UI to settle before taking the next action.".to_string())
        } else if let Some(element) = focused_input {
            element
                .label
                .as_ref()
                .map(|label| format!("Continue entering or selecting data in \"{}\".", label))
        } else if let Some(element) = first_actionable {
            element
                .label
                .as_ref()
                .map(|label| format!("Inspect or use \"{}\" if it matches the goal.", label))
        } else if self.vision_needed {
            Some("Refresh context or fall back to vision for a denser read.".to_string())
        } else {
            Some("Inspect the current screen and choose the next actionable control.".to_string())
        };

        let task_phase = if self.temporal.loading.is_some() {
            TaskPhase::Loading
        } else if first_anomaly.is_some() || self.temporal.error_persisting.is_some() {
            TaskPhase::Blocked
        } else if focused_input.is_some() {
            TaskPhase::Input
        } else if self.temporal.idle_since.is_some() {
            TaskPhase::Idle
        } else if self.last_diff_summary.as_ref().map_or(false, |diff| {
            diff.added_count + diff.removed_count + diff.changed_count > 0
        }) {
            TaskPhase::Navigation
        } else {
            TaskPhase::Review
        };

        self.semantic = Some(SemanticInsight {
            current_activity,
            recent_transition,
            likely_blocker,
            suggested_next_step,
            task_phase,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_context::{ContentRole, ContextElement};
    use std::collections::{HashMap, VecDeque};

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

    #[test]
    fn test_refresh_derived_populates_canonical_fields() {
        let mut model = MentalModel::default();
        model.current_context.app = "Google Chrome".into();
        model.current_context.window = "Checkout".into();
        model.current_context.timestamp_ms = 10_000;
        model.current_context.elements = vec![
            ContextElement {
                id: "field-email".into(),
                label: Some("Email".into()),
                description: None,
                element_type: "input".into(),
                value: None,
                bounds: None,
                state: cel_context::ElementState {
                    focused: true,
                    enabled: true,
                    visible: true,
                    selected: false,
                    expanded: None,
                    checked: None,
                },
                parent_id: None,
                actions: vec!["type".into()],
                confidence: 0.95,
                source: ContextSource::AccessibilityTree,
                content_role: ContentRole::Interactive,
                properties: HashMap::new(),
            },
            ContextElement {
                id: "btn-pay".into(),
                label: Some("Pay now".into()),
                description: None,
                element_type: "button".into(),
                value: None,
                bounds: None,
                state: cel_context::ElementState {
                    focused: false,
                    enabled: true,
                    visible: true,
                    selected: false,
                    expanded: None,
                    checked: None,
                },
                parent_id: None,
                actions: vec!["click".into()],
                confidence: 0.92,
                source: ContextSource::NativeApi,
                content_role: ContentRole::Interactive,
                properties: HashMap::new(),
            },
        ];
        model.focused_element = Some(FocusedElement {
            id: "field-email".into(),
            label: Some("Email".into()),
        });
        model.temporal.focus_trail = VecDeque::from(vec!["Cart".into(), "Email".into()]);
        model.recent_diffs.push_back(PerceptionDiff {
            added_count: 1,
            removed_count: 0,
            changed_count: 2,
            unchanged_count: 8,
            added_labels: vec!["Pay now".into()],
            changed_labels: vec!["Email".into()],
        });
        model
            .element_adapter_index
            .insert("btn-pay".into(), "browser".into());
        model.confidence = 0.72;

        model.refresh_derived(12_400, Some(12_000), None);

        let freshness = model.freshness.as_ref().expect("freshness");
        assert_eq!(freshness.state, FreshnessState::SoftStale);
        assert!(freshness.causes.contains(&StalenessCause::Time));
        assert!(freshness.causes.contains(&StalenessCause::Confidence));

        let summary = model.source_summary.as_ref().expect("source summary");
        assert_eq!(summary.accessibility, 1);
        assert_eq!(summary.native_api, 1);
        assert_eq!(summary.adapter_backed, 1);

        let semantic = model.semantic.as_ref().expect("semantic insight");
        assert_eq!(semantic.task_phase, TaskPhase::Input);
        assert!(semantic.current_activity.contains("Google Chrome"));
        assert!(semantic
            .suggested_next_step
            .as_ref()
            .is_some_and(|step| step.contains("Email")));
    }
}
