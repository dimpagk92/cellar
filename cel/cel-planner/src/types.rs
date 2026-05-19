/// Core types for the CEL planner.
///
/// Boundary types (`PlannedAction`, `CellWrite`) live in `cel-contracts`. They
/// are re-exported here for backward compatibility — prefer importing them
/// directly from `cel_contracts` in new code.
use serde::{Deserialize, Deserializer, Serialize};

pub use cel_contracts::{CellWrite, PlannedAction};

// ─── Cognitive Loop Extensions ──────────────────────────────────────────────

/// A notebook write from the LLM — records data discovered during execution.
/// Persists across replans so discovered data isn't lost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookWrite {
    pub key: String,
    pub value: String,
    /// Category: "data" (prices, names), "url" (links), "observation", "error"
    #[serde(default = "default_notebook_category")]
    pub category: String,
}

fn default_notebook_category() -> String {
    "data".to_string()
}

/// Progress assessment from the LLM — proactive self-evaluation.
/// Single string parsed by the system:
/// - "on_track" — continue normally
/// - "stalled" — inject nudge into next step
/// - "wrong_approach" — trigger proactive replan (before failure)
/// - "milestone:label" — milestone reached, capture checkpoint
pub type ProgressAssessment = String;

/// How much detail to include in the context prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContextDetail {
    /// Full 6-column table: ID, Type, Label, Value, State, Actions.
    #[default]
    Full,
    /// Compact 3-column table: ID, Type, Label. ~40% fewer tokens.
    Compact,
    /// Only elements that are enabled, visible, and have actions. ~60-80% fewer.
    ActionableOnly,
    /// Indented tree format with parent-child relationships (inspired by browser-use).
    /// Shows spatial structure that helps the LLM understand layout.
    Tree,
}

/// Configuration for a planning session.
#[derive(Debug, Clone)]
pub struct GoalConfig {
    /// The natural-language goal to achieve.
    pub goal: String,
    /// Maximum number of steps before the planner gives up.
    pub max_steps: u32,
    /// Maximum LLM retries per step on parse failure.
    pub max_retries: u32,
    /// LLM max_tokens for each planning call.
    pub max_tokens: u32,
    /// How much detail to include in the element table.
    pub context_detail: ContextDetail,
}

impl Default for GoalConfig {
    fn default() -> Self {
        Self {
            goal: String::new(),
            max_steps: 50,
            max_retries: 3,
            // 2048 was fine for non-reasoning models but Gemini 2.5 Flash
            // burns a variable chunk on thinking tokens before emitting
            // visible output — runs regularly truncated JSON at ~300 chars.
            // 8192 gives headroom for the thinking pass plus a cdp_eval
            // batch (which can easily exceed 1kB of JS).
            max_tokens: 8192,
            context_detail: ContextDetail::Full,
        }
    }
}

impl GoalConfig {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            ..Default::default()
        }
    }
}

/// How much context the planner requests for the NEXT step.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextTier {
    /// No context needed — blind action (key_combo, key, wait, scroll).
    None,
    /// Minimal context — focused element + app/window name only (~200ms).
    Minimal,
    /// Full accessibility tree walk (~2-15s).
    #[default]
    Full,
}

/// A single planned step from the LLM.
/// Inspired by Browser-Use's structured output: forces self-reflection at every step.
/// Supports multi-action output: up to 5 actions per step (like Browser-Use).
///
/// Cognitive loop extensions:
/// - `thinking`: replaces evaluation+memory+reasoning (3→1 field)
/// - `progress`: proactive self-assessment (on_track/stalled/wrong_approach/milestone:X)
/// - `notebook_writes`: data to persist across replans
/// - `batch_next`: signal to skip context re-gathering on next step
#[derive(Debug, Clone, Serialize)]
pub struct PlannedStep {
    /// Evaluation of the PREVIOUS step's result. Forces the LLM to verify
    /// whether its last action succeeded before deciding the next one.
    pub evaluation: String,
    /// Working memory — 1-3 sentences tracking progress across steps.
    pub memory: String,
    /// Updated plan with status markers.
    pub plan: Vec<String>,
    /// Why the LLM chose this step.
    pub reasoning: String,
    /// The primary action to take (always present for backward compat).
    pub action: PlannedAction,
    /// Additional actions to execute after the primary action (up to 4 more).
    /// When populated, ALL actions (primary + additional) execute in sequence.
    /// This reduces LLM calls by 3-5x for predictable sequences.
    pub additional_actions: Vec<PlannedAction>,
    /// What should change after these actions.
    pub expected_outcome: String,
    /// LLM's self-assessed confidence (0.0-1.0).
    pub confidence: f64,
    /// Context tier requested for the NEXT step.
    pub context_tier: ContextTier,

    // ── Cognitive loop extensions ──────────────────────────────────────────
    /// Free-form internal monologue. Replaces evaluation+memory+reasoning
    /// when present. Contains the LLM's narration of what it sees and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,

    /// Proactive progress assessment. Values:
    /// "on_track", "stalled", "wrong_approach", "milestone:label"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressAssessment>,

    /// Data discovered during this step to persist in the notebook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notebook_writes: Vec<NotebookWrite>,

    /// When true, the system should skip context re-gathering on the next step.
    /// The LLM signals this when confident the next action doesn't need fresh context.
    #[serde(default)]
    pub batch_next: bool,
}

/// Custom deserializer that accepts EITHER the old format (`"action"` + `"additional_actions"`)
/// or the new prompt format (`"actions"` array where first = action, rest = additional_actions).
impl<'de> Deserialize<'de> for PlannedStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            evaluation: String,
            #[serde(default)]
            memory: String,
            #[serde(default, deserialize_with = "flexible_plan")]
            plan: Vec<String>,
            #[serde(default)]
            reasoning: String,
            // Old format: singular action
            action: Option<PlannedAction>,
            #[serde(default)]
            additional_actions: Vec<PlannedAction>,
            // New format: actions array (prompt tells LLM to use this)
            actions: Option<Vec<PlannedAction>>,
            #[serde(default)]
            expected_outcome: String,
            #[serde(default = "default_confidence")]
            confidence: f64,
            #[serde(default)]
            context_tier: ContextTier,
            // Cognitive loop extensions (all optional for backward compat)
            #[serde(default)]
            thinking: Option<String>,
            #[serde(default)]
            progress: Option<ProgressAssessment>,
            #[serde(default)]
            notebook_writes: Vec<NotebookWrite>,
            #[serde(default)]
            batch_next: bool,
        }

        let raw = Raw::deserialize(deserializer)?;

        let (action, additional_actions) = if let Some(mut actions) = raw.actions {
            // New format: "actions" array — first is primary, rest are additional
            if actions.is_empty() {
                return Err(serde::de::Error::custom("actions array is empty"));
            }
            let primary = actions.remove(0);
            (primary, actions)
        } else if let Some(action) = raw.action {
            // Old format: singular "action" + "additional_actions"
            (action, raw.additional_actions)
        } else {
            return Err(serde::de::Error::custom(
                "must have either \"action\" or \"actions\" field",
            ));
        };

        // When `thinking` is present, use it to populate legacy fields for backward compat
        let (evaluation, memory, reasoning) = if let Some(ref thinking) = raw.thinking {
            // New cognitive format: thinking replaces evaluation+memory+reasoning
            (String::new(), String::new(), thinking.clone())
        } else {
            (raw.evaluation, raw.memory, raw.reasoning)
        };

        Ok(PlannedStep {
            evaluation,
            memory,
            plan: raw.plan,
            reasoning,
            action,
            additional_actions,
            expected_outcome: raw.expected_outcome,
            confidence: raw.confidence,
            context_tier: raw.context_tier,
            thinking: raw.thinking,
            progress: raw.progress,
            notebook_writes: raw.notebook_writes,
            batch_next: raw.batch_next,
        })
    }
}

impl PlannedStep {
    /// Get all actions to execute (primary + additional).
    pub fn all_actions(&self) -> Vec<&PlannedAction> {
        let mut actions = vec![&self.action];
        actions.extend(self.additional_actions.iter());
        actions
    }
}

fn default_confidence() -> f64 {
    0.5
}

/// Accept `plan` as either a JSON array of strings or a single string (split by newlines).
/// Gemini Flash sometimes returns the plan as a flat string instead of an array.
fn flexible_plan<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(arr) => Ok(arr
            .into_iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .collect()),
        serde_json::Value::String(s) => Ok(s
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()),
        serde_json::Value::Null => Ok(vec![]),
        _ => Ok(vec![value.to_string()]),
    }
}

/// A multi-step plan from the LLM (plan-ahead mode).
/// Only the first step is executed immediately; remaining steps are tentative
/// and will be re-evaluated if the context diverges from expectations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedPlan {
    /// High-level reasoning for the overall plan.
    pub overall_reasoning: String,
    /// Ordered list of planned steps. First step is executed, rest are tentative.
    pub steps: Vec<PlannedStep>,
}

// ─────────────────────────────────────────────────────────────────────────────
// PlannedAction + CellWrite definitions moved to `cel-contracts`. Re-exported
// at the top of this module via `pub use cel_contracts::{...};`. Look in
// `cel-contracts/src/actions.rs` for the canonical action contract.
// ─────────────────────────────────────────────────────────────────────────────

/// Events emitted during the planning loop for observability.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum PlannerEvent {
    /// A step was planned.
    StepPlanned { step_index: u32, step: PlannedStep },
    /// A step was executed (caller reports success/failure).
    StepExecuted {
        step_index: u32,
        success: bool,
        error: Option<String>,
    },
    /// The goal was achieved.
    GoalAchieved { summary: String, total_steps: u32 },
    /// The planner failed to achieve the goal.
    GoalFailed { reason: String, total_steps: u32 },
    /// LLM returned unparseable output (will retry).
    ParseRetry {
        step_index: u32,
        attempt: u32,
        raw_output: String,
    },
    /// Context had too few actionable elements, retrying.
    EmptyContextRetry {
        step_index: u32,
        actionable_count: usize,
        retry_attempt: u32,
    },
    /// Grounding validation failed (element ID not in context or blocking error).
    GroundingRejected { step_index: u32, reason: String },
    /// Loop detected in agent behaviour.
    LoopDetected { step_index: u32, signal: String },
}

/// Result of a single step in the history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub step_index: u32,
    pub action: PlannedAction,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Human-readable label of the target element (e.g. "Submit", "Username").
    /// Used by the prompt to show *what* was acted on, not just the raw ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_label: Option<String>,
    /// Data returned by the action (e.g. cdp_eval result, extracted text).
    /// Included in the prompt so the LLM can reason about action outputs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planned_action_click_roundtrip() {
        let action = PlannedAction::Click {
            target_id: "dom:submit".into(), expect_after: None,
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        match parsed {
            PlannedAction::Click { target_id, .. } => assert_eq!(target_id, "dom:submit"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_planned_action_type_roundtrip() {
        let action = PlannedAction::Type {
            target_id: Some("dom:email".into()),
            text: "admin@example.com".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        match parsed {
            PlannedAction::Type { target_id, text } => {
                assert_eq!(target_id.as_deref(), Some("dom:email"));
                assert_eq!(text, "admin@example.com");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_planned_action_done_roundtrip() {
        let action = PlannedAction::Done {
            summary: "Login successful".into(),
            evidence_ids: vec!["dom:welcome-banner".into()],
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        match parsed {
            PlannedAction::Done {
                summary,
                evidence_ids,
            } => {
                assert_eq!(summary, "Login successful");
                assert_eq!(evidence_ids, vec!["dom:welcome-banner"]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_planned_action_done_without_evidence_deserializes() {
        // Backwards compatibility: old LLM responses without evidence_ids should still parse
        let json = r#"{"type":"done","summary":"All done"}"#;
        let parsed: PlannedAction = serde_json::from_str(json).unwrap();
        match parsed {
            PlannedAction::Done {
                summary,
                evidence_ids,
            } => {
                assert_eq!(summary, "All done");
                assert!(evidence_ids.is_empty());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_planned_action_custom_roundtrip() {
        let action = PlannedAction::Custom {
            adapter: "browser".into(),
            action: "fill".into(),
            params: serde_json::json!({"selector": "#name", "value": "test"}),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
        match parsed {
            PlannedAction::Custom {
                adapter, action, ..
            } => {
                assert_eq!(adapter, "browser");
                assert_eq!(action, "fill");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_planned_step_full_roundtrip() {
        let step = PlannedStep {
            evaluation: String::new(),
            memory: String::new(),
            plan: vec![],
            reasoning: "The email field is visible and empty".into(),
            action: PlannedAction::Type {
                target_id: Some("dom:email".into()),
                text: "user@test.com".into(),
            },
            additional_actions: vec![],
            expected_outcome: "Email field filled".into(),
            confidence: 0.92,
            context_tier: ContextTier::Full,
            thinking: None,
            progress: None,
            notebook_writes: vec![],
            batch_next: false,
        };
        let json = serde_json::to_string(&step).unwrap();
        let parsed: PlannedStep = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reasoning, step.reasoning);
        assert_eq!(parsed.confidence, step.confidence);
        assert_eq!(parsed.expected_outcome, step.expected_outcome);
    }

    #[test]
    fn test_all_action_variants_serialize() {
        let actions = vec![
            PlannedAction::Click {
                target_id: "btn".into(), expect_after: None,
            },
            PlannedAction::Type {
                target_id: Some("inp".into()),
                text: "hi".into(),
            },
            PlannedAction::Key {
                key: "Enter".into(),
            },
            PlannedAction::KeyCombo {
                keys: vec!["Ctrl".into(), "S".into()],
            },
            PlannedAction::Scroll { dx: 0, dy: -3 },
            PlannedAction::Wait { ms: 1000 },
            PlannedAction::Custom {
                adapter: "browser".into(),
                action: "navigate".into(),
                params: serde_json::json!({"url": "https://example.com"}),
            },
            PlannedAction::Act {
                instruction: "click the search button".into(),
            },
            PlannedAction::Done {
                summary: "Done!".into(),
                evidence_ids: vec!["el1".into()],
            },
            PlannedAction::Fail {
                reason: "Not found".into(),
            },
        ];

        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let parsed: PlannedAction = serde_json::from_str(&json).unwrap();
            // Verify type field is present in JSON
            let obj: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(
                obj.get("type").is_some(),
                "Missing 'type' field in: {}",
                json
            );
            // Verify roundtrip doesn't panic
            let _ = serde_json::to_string(&parsed).unwrap();
        }
    }

    #[test]
    fn test_notebook_writes_action_parses_as_noop() {
        // LLM sometimes puts notebook_writes inside the actions array.
        // This should parse gracefully as a no-op, not crash.
        let json = r#"{"type":"notebook_writes","key":"price","value":"$149","category":"data"}"#;
        let parsed: PlannedAction = serde_json::from_str(json).unwrap();
        match parsed {
            PlannedAction::NotebookWrites {
                key,
                value,
                category,
            } => {
                assert_eq!(key, "price");
                assert_eq!(value, "$149");
                assert_eq!(category, "data");
            }
            _ => panic!("Expected NotebookWrites variant"),
        }
    }

    #[test]
    fn test_planned_step_with_cognitive_fields() {
        // New format: thinking + progress + notebook_writes + batch_next
        let json = r#"{
            "thinking": "I see a search form with destination and date fields.",
            "progress": "on_track",
            "plan": ["[x] Navigate", "[>] Fill form"],
            "actions": [{"type": "click", "target_id": "3"}],
            "notebook_writes": [{"key": "destination", "value": "Amsterdam", "category": "data"}],
            "expected_outcome": "Form field focused",
            "confidence": 0.85,
            "batch_next": true
        }"#;
        let step: PlannedStep = serde_json::from_str(json).unwrap();
        assert_eq!(
            step.thinking.as_deref(),
            Some("I see a search form with destination and date fields.")
        );
        assert_eq!(step.progress.as_deref(), Some("on_track"));
        assert_eq!(step.notebook_writes.len(), 1);
        assert_eq!(step.notebook_writes[0].key, "destination");
        assert!(step.batch_next);
        // thinking should populate reasoning for backward compat
        assert!(step.reasoning.contains("search form"));
    }

    #[test]
    fn test_planned_step_without_cognitive_fields() {
        // Old format: should still parse (backward compat via serde defaults)
        let json = r#"{
            "evaluation": "Previous step succeeded",
            "memory": "On the search page",
            "reasoning": "Need to fill the form",
            "actions": [{"type": "click", "target_id": "1"}],
            "expected_outcome": "Field focused",
            "confidence": 0.9
        }"#;
        let step: PlannedStep = serde_json::from_str(json).unwrap();
        assert!(step.thinking.is_none());
        assert!(step.progress.is_none());
        assert!(step.notebook_writes.is_empty());
        assert!(!step.batch_next);
        assert_eq!(step.evaluation, "Previous step succeeded");
        assert_eq!(step.reasoning, "Need to fill the form");
    }
}
