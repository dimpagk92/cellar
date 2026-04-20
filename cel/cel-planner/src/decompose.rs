/// Goal decomposition — breaks complex goals into advisory milestones.
///
/// Milestones are NOT rigid phases with entry/exit conditions. They're
/// advisory checkpoints that help the LLM track progress and enable
/// Tier 3 replanning (backtrack to last checkpoint).
///
/// The LLM can add, skip, or reorder milestones during execution.
/// They're guidance, not gates.

use serde::{Deserialize, Serialize};

/// A single milestone from the decomposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    /// Short label (e.g. "on_search_page", "form_filled").
    pub label: String,
    /// Human-readable description of what this milestone represents.
    pub description: String,
    /// Suggested step budget for this milestone.
    pub step_budget: u32,
}

/// Result of goal decomposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionResult {
    /// Whether the goal is feasible from the current context.
    pub feasible: bool,
    /// Confidence in feasibility assessment (0.0-1.0).
    pub feasibility_confidence: f64,
    /// Why the goal is/isn't feasible.
    pub feasibility_reasoning: String,
    /// Missing prerequisites (empty if feasible).
    pub missing_prerequisites: Vec<String>,
    /// Advisory milestones for the goal.
    pub milestones: Vec<Milestone>,
    /// Overall reasoning for the decomposition.
    pub reasoning: String,
}

/// Build the system prompt for goal decomposition.
///
/// This is a lightweight LLM call that produces milestones, not a full plan.
/// Used for goals with maxSteps > 15 (simple goals skip decomposition).
pub fn build_decomposition_prompt() -> String {
    r#"You are a goal decomposition assistant. Given a goal and current screen context, produce:

1. A feasibility assessment: can this goal be achieved from the current state?
2. A list of milestones (3-6) that break the goal into natural checkpoints.

Milestones are NOT individual actions. They're human-recognizable progress markers:
- "on_search_page" (not "click search link")
- "form_filled" (not "type into field 1, type into field 2")
- "results_visible" (not "wait 2 seconds")

Each milestone should represent a state you can visually verify.

Response format (JSON only):
{
  "feasible": true,
  "feasibility_confidence": 0.9,
  "feasibility_reasoning": "Chrome is open and can navigate to the booking site",
  "missing_prerequisites": [],
  "milestones": [
    {"label": "on_search_page", "description": "Navigate to booking site search page", "step_budget": 5},
    {"label": "search_submitted", "description": "Fill and submit search form", "step_budget": 8},
    {"label": "option_selected", "description": "Review results and select best option", "step_budget": 10},
    {"label": "booking_confirmed", "description": "Complete booking and get confirmation", "step_budget": 12}
  ],
  "reasoning": "This is a 4-phase booking flow. Most time in selection and form filling."
}

Rules:
- Step budgets should sum to roughly the total step budget given.
- If the goal is simple (1-2 milestones), that's fine. Don't over-decompose.
- If the goal is not feasible, set feasible=false and explain in missing_prerequisites.
- milestones should be in execution order.

JSON only:"#
        .to_string()
}

/// Build the user prompt for goal decomposition.
pub fn build_decomposition_user_prompt(
    goal: &str,
    app: &str,
    window: &str,
    element_summary: &str,
    total_step_budget: u32,
    history_advice: Option<&str>,
) -> String {
    let mut prompt = format!(
        "GOAL: {}\nCURRENT STATE: App={}, Window={}\nELEMENTS (first 20): {}\nSTEP BUDGET: {}\n",
        goal, app, window, element_summary, total_step_budget,
    );

    if let Some(advice) = history_advice {
        prompt.push_str(&format!("\nPAST EXPERIENCE:\n{}\n", advice));
    }

    prompt.push_str("\nJSON only:");
    prompt
}

/// Parse the LLM's decomposition response.
pub fn parse_decomposition(raw: &str) -> Result<DecompositionResult, String> {
    // Strip markdown code fences if present
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(cleaned).map_err(|e| format!("Failed to parse decomposition: {}", e))
}

/// Format milestones for injection into the planner's system prompt.
/// Returns empty string if no milestones.
pub fn format_milestones_for_prompt(milestones: &[Milestone]) -> String {
    if milestones.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n\nMILESTONES for this goal:\n");
    for (i, m) in milestones.iter().enumerate() {
        out.push_str(&format!("{}. {} — {} (~{} steps)\n", i + 1, m.label, m.description, m.step_budget));
    }
    out.push_str("When you reach a milestone, set progress to \"milestone:<label>\".\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_decomposition() {
        let json = r#"{
            "feasible": true,
            "feasibility_confidence": 0.9,
            "feasibility_reasoning": "Chrome is open",
            "missing_prerequisites": [],
            "milestones": [
                {"label": "on_search_page", "description": "Navigate to search", "step_budget": 5}
            ],
            "reasoning": "Simple navigation task"
        }"#;
        let result = parse_decomposition(json).unwrap();
        assert!(result.feasible);
        assert_eq!(result.milestones.len(), 1);
        assert_eq!(result.milestones[0].label, "on_search_page");
    }

    #[test]
    fn test_parse_decomposition_with_code_fence() {
        let json = "```json\n{\"feasible\":true,\"feasibility_confidence\":0.8,\"feasibility_reasoning\":\"ok\",\"missing_prerequisites\":[],\"milestones\":[],\"reasoning\":\"simple\"}\n```";
        let result = parse_decomposition(json).unwrap();
        assert!(result.feasible);
    }

    #[test]
    fn test_format_milestones_empty() {
        assert_eq!(format_milestones_for_prompt(&[]), "");
    }

    #[test]
    fn test_format_milestones() {
        let milestones = vec![
            Milestone { label: "search".into(), description: "Fill search form".into(), step_budget: 5 },
            Milestone { label: "select".into(), description: "Pick best option".into(), step_budget: 10 },
        ];
        let formatted = format_milestones_for_prompt(&milestones);
        assert!(formatted.contains("1. search"));
        assert!(formatted.contains("2. select"));
        assert!(formatted.contains("milestone:<label>"));
    }
}
