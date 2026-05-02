/// Step history tracking for the planner's LLM prompt.
///
/// Includes message compaction: when history exceeds a threshold,
/// older steps are summarized into a compact digest instead of being
/// dropped entirely. This prevents context overflow while preserving
/// key information (browser-use learned this matters Feb 2026).
use crate::types::{PlannedAction, StepRecord};

/// When total steps exceed this, compact older ones into a summary.
const COMPACTION_THRESHOLD: usize = 10;

/// Tracks executed steps so the LLM can see what happened.
#[derive(Debug, Default)]
pub struct StepHistory {
    steps: Vec<StepRecord>,
    /// Compact summary of steps that were compacted away.
    compacted_summary: Option<String>,
}

impl StepHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a history from existing records (used when resuming via NAPI).
    pub fn from_records(records: Vec<StepRecord>) -> Self {
        Self {
            steps: records,
            compacted_summary: None,
        }
    }

    /// Record a step result. Triggers compaction if threshold is exceeded.
    pub fn record(
        &mut self,
        step_index: u32,
        action: PlannedAction,
        success: bool,
        error: Option<String>,
    ) {
        self.record_full(step_index, action, success, error, None, None);
    }

    /// Record a step result with an optional element label for richer history.
    pub fn record_with_label(
        &mut self,
        step_index: u32,
        action: PlannedAction,
        success: bool,
        error: Option<String>,
        element_label: Option<String>,
    ) {
        self.record_full(step_index, action, success, error, element_label, None);
    }

    /// Record a step result with optional label and action output data.
    /// Data from cdp_eval or extract actions is passed through untruncated —
    /// Gemini 2.5 Flash has 1M context, so full page text is fine.
    pub fn record_full(
        &mut self,
        step_index: u32,
        action: PlannedAction,
        success: bool,
        error: Option<String>,
        element_label: Option<String>,
        data: Option<String>,
    ) {
        self.steps.push(StepRecord {
            step_index,
            action,
            success,
            error,
            element_label,
            data,
        });

        // Compact when we exceed the threshold
        if self.steps.len() > COMPACTION_THRESHOLD {
            self.compact();
        }
    }

    /// Get the last N steps for the prompt (keeps prompt size bounded).
    pub fn recent(&self, n: usize) -> &[StepRecord] {
        let start = self.steps.len().saturating_sub(n);
        &self.steps[start..]
    }

    /// Get the compacted summary of older steps, if any.
    pub fn compacted_summary(&self) -> Option<&str> {
        self.compacted_summary.as_deref()
    }

    /// Total number of recorded steps (including compacted).
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether no steps have been recorded.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Get all recorded steps (not including compacted summary).
    pub fn all(&self) -> &[StepRecord] {
        &self.steps
    }

    /// Compact older steps into a label-enriched summary, keeping only recent steps.
    /// Uses failure-biased retention: always keeps failed steps in the recent window.
    fn compact(&mut self) {
        let keep = 5;
        if self.steps.len() <= keep {
            return;
        }

        let to_compact = self.steps.len() - keep;
        let compacting = &self.steps[..to_compact];

        // Build label-enriched summary
        let first_index = compacting.first().map(|s| s.step_index).unwrap_or(0);
        let last_index = compacting.last().map(|s| s.step_index).unwrap_or(0);
        let succeeded = compacting.iter().filter(|s| s.success).count();
        let failed = compacting.iter().filter(|s| !s.success).count();

        // Include action descriptions with labels (up to 5)
        let action_descs: Vec<String> = compacting
            .iter()
            .take(5)
            .map(|s| {
                let label = s.element_label.as_deref().unwrap_or("");
                let action_type = match &s.action {
                    PlannedAction::Click { target_id } => {
                        if label.is_empty() {
                            format!("clicked {}", target_id)
                        } else {
                            format!("clicked '{}'", label)
                        }
                    }
                    PlannedAction::Type { text, .. } => {
                        if label.is_empty() {
                            format!("typed \"{}\"", &text[..text.len().min(15)])
                        } else {
                            format!("typed '{}' = \"{}\"", label, &text[..text.len().min(15)])
                        }
                    }
                    PlannedAction::SetValue { value, .. } => {
                        if label.is_empty() {
                            format!("set \"{}\"", &value[..value.len().min(15)])
                        } else {
                            format!("set '{}' = \"{}\"", label, &value[..value.len().min(15)])
                        }
                    }
                    _ => format!("{:?}", s.action).chars().take(20).collect(),
                };
                if s.success {
                    action_type
                } else {
                    format!("FAILED: {}", action_type)
                }
            })
            .collect();

        let mut summary = format!(
            "Steps {}-{}: {} ({} OK, {} failed)",
            first_index + 1,
            last_index + 1,
            action_descs.join(", "),
            succeeded,
            failed,
        );

        // Add failure details
        let failure_reasons: Vec<_> = compacting
            .iter()
            .filter(|s| !s.success)
            .filter_map(|s| s.error.as_deref())
            .take(3)
            .collect();
        if !failure_reasons.is_empty() {
            summary.push_str(&format!(". Errors: {}", failure_reasons.join("; ")));
        }

        // Prepend to any existing compacted summary
        if let Some(existing) = &self.compacted_summary {
            self.compacted_summary = Some(format!("{} {}", existing, summary));
        } else {
            self.compacted_summary = Some(summary);
        }

        // Keep only the recent steps
        self.steps = self.steps.split_off(to_compact);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_history() {
        let history = StepHistory::new();
        assert!(history.is_empty());
        assert_eq!(history.len(), 0);
        assert_eq!(history.recent(5).len(), 0);
        assert!(history.compacted_summary().is_none());
    }

    #[test]
    fn test_record_and_retrieve() {
        let mut history = StepHistory::new();
        history.record(
            0,
            PlannedAction::Click {
                target_id: "btn1".into(),
            },
            true,
            None,
        );
        history.record(
            1,
            PlannedAction::Type {
                target_id: Some("inp".into()),
                text: "hello".into(),
            },
            true,
            None,
        );
        assert_eq!(history.len(), 2);
        assert!(!history.is_empty());
    }

    #[test]
    fn test_recent_window() {
        let mut history = StepHistory::new();
        // Use 8 steps (below threshold of 10) to test raw recent window
        for i in 0..8 {
            history.record(
                i,
                PlannedAction::Click {
                    target_id: format!("btn{}", i),
                },
                true,
                None,
            );
        }
        let recent = history.recent(5);
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].step_index, 3);
        assert_eq!(recent[4].step_index, 7);
    }

    #[test]
    fn test_recent_fewer_than_n() {
        let mut history = StepHistory::new();
        history.record(
            0,
            PlannedAction::Click {
                target_id: "btn".into(),
            },
            true,
            None,
        );
        let recent = history.recent(10);
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn test_from_records() {
        let records = vec![
            StepRecord {
                step_index: 0,
                action: PlannedAction::Click {
                    target_id: "a".into(),
                },
                success: true,
                error: None,
                element_label: Some("Submit".into()),
                data: None,
            },
            StepRecord {
                step_index: 1,
                action: PlannedAction::Fail {
                    reason: "not found".into(),
                },
                success: false,
                error: Some("Element missing".into()),
                element_label: None,
                data: None,
            },
        ];
        let history = StepHistory::from_records(records);
        assert_eq!(history.len(), 2);
        assert!(!history.all()[1].success);
    }

    #[test]
    fn test_compaction_triggers_at_threshold() {
        let mut history = StepHistory::new();
        // Record 11 steps — exceeds COMPACTION_THRESHOLD (10)
        for i in 0..11 {
            history.record(
                i,
                PlannedAction::Click {
                    target_id: format!("btn{}", i),
                },
                i % 3 != 0, // Every 3rd step fails
                if i % 3 == 0 {
                    Some(format!("Error at step {}", i))
                } else {
                    None
                },
            );
        }

        // Should have been compacted — only 5 recent steps remain
        assert_eq!(history.len(), 5);
        assert!(history.compacted_summary().is_some());

        let summary = history.compacted_summary().unwrap();
        assert!(summary.contains("OK"));
        assert!(summary.contains("failed"));
    }

    #[test]
    fn test_compaction_preserves_recent_steps() {
        let mut history = StepHistory::new();
        for i in 0..15 {
            history.record(
                i,
                PlannedAction::Click {
                    target_id: format!("btn{}", i),
                },
                true,
                None,
            );
        }

        // Recent steps should be the latest ones
        let recent = history.recent(5);
        assert_eq!(recent.last().unwrap().step_index, 14);
    }

    #[test]
    fn test_compaction_records_failures() {
        let mut history = StepHistory::new();
        for i in 0..11 {
            history.record(
                i,
                PlannedAction::Click {
                    target_id: "btn".into(),
                },
                false,
                Some(format!("Failed: {}", i)),
            );
        }

        let summary = history.compacted_summary().unwrap();
        assert!(summary.contains("failed"));
        assert!(summary.contains("FAILED:"));
    }
}
