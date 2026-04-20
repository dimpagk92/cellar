//! Strategy Router — route selection for action execution.
//!
//! Ported from agent/src/strategy-router.ts (138 lines).
//! Determines which execution route to use for a planned action:
//! structured → semantic → vision → terminal_failure
//!
//! The router is a pure function: (action, context, freshness, attempts) → route decision.

use serde::{Deserialize, Serialize};

/// Execution route for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyRoute {
    /// Execute using structured selectors (CSS, a11y ID, backend node).
    Structured,
    /// Resolve semantically (a11y tree / LLM disambiguation) then execute.
    Semantic,
    /// Screenshot → vision model → coordinate-based execution.
    Vision,
    /// Force context re-read (stale data detected).
    Refresh,
    /// Escalation ceiling reached — stop trying.
    TerminalFailure,
}

/// Record of a previous route attempt for escalation tracking.
#[derive(Debug, Clone)]
pub struct StrategyAttempt {
    pub route: StrategyRoute,
    pub success: bool,
    pub verified: bool,
}

/// Freshness assessment from the Cortex.
#[derive(Debug, Clone)]
pub struct FreshnessState {
    pub state: FreshnessLevel,
    pub confidence: f64,
    pub causes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessLevel {
    Fresh,
    SoftStale,
    HardStale,
}

/// Result of route selection.
#[derive(Debug, Clone)]
pub struct StrategySelection {
    pub route: StrategyRoute,
    pub confidence: f64,
    pub reason: String,
    pub terminal: bool,
}

/// Action types that can escalate to semantic/vision routes.
fn is_semantic_capable(action_type: &str) -> bool {
    matches!(action_type, "click" | "type" | "set_value" | "act")
}

fn is_vision_capable(action_type: &str) -> bool {
    is_semantic_capable(action_type) || matches!(action_type, "done" | "extract")
}

/// Max retries for non-escalatable actions (key, scroll, key_combo, drag).
const MAX_NON_ESCALATABLE_ATTEMPTS: usize = 3;

/// Select the best execution route for an action.
pub fn select_route(
    action_type: &str,
    freshness: Option<&FreshnessState>,
    attempts: &[StrategyAttempt],
    ambiguous: bool,
) -> StrategySelection {
    let attempted_routes: Vec<StrategyRoute> = attempts.iter().map(|a| a.route).collect();

    // Terminal ceiling: vision already tried
    if attempted_routes.contains(&StrategyRoute::Vision) {
        return StrategySelection {
            route: StrategyRoute::TerminalFailure,
            confidence: 0.0,
            reason: "Vision route already attempted and failed verification".into(),
            terminal: true,
        };
    }

    // Terminal ceiling for non-escalatable actions
    if !is_semantic_capable(action_type) && attempts.len() >= MAX_NON_ESCALATABLE_ATTEMPTS {
        return StrategySelection {
            route: StrategyRoute::TerminalFailure,
            confidence: 0.0,
            reason: format!(
                "Non-escalatable action \"{action_type}\" failed {} times without verification",
                attempts.len()
            ),
            terminal: true,
        };
    }

    // Hard-stale → refresh
    if let Some(f) = freshness {
        if f.state == FreshnessLevel::HardStale {
            return StrategySelection {
                route: StrategyRoute::Refresh,
                confidence: f.confidence,
                reason: format!("Model is hard-stale ({})", f.causes.join(", ")),
                terminal: false,
            };
        }
    }

    // Ambiguous target + semantic capable → semantic (first attempt)
    if ambiguous && is_semantic_capable(action_type) && attempts.is_empty() {
        return StrategySelection {
            route: StrategyRoute::Semantic,
            confidence: 0.82,
            reason: "Ambiguous target — using semantic resolution".into(),
            terminal: false,
        };
    }

    // Semantic already tried → vision
    if attempted_routes.contains(&StrategyRoute::Semantic) && is_vision_capable(action_type) {
        return StrategySelection {
            route: StrategyRoute::Vision,
            confidence: 0.35,
            reason: "Structured/semantic execution could not verify the action".into(),
            terminal: false,
        };
    }

    // Structured already tried → semantic
    if attempted_routes.contains(&StrategyRoute::Structured) && is_semantic_capable(action_type) {
        return StrategySelection {
            route: StrategyRoute::Semantic,
            confidence: 0.55,
            reason: "Structured execution was insufficient; escalate to semantic resolution".into(),
            terminal: false,
        };
    }

    // Soft-stale → semantic
    if let Some(f) = freshness {
        if f.state == FreshnessLevel::SoftStale && is_semantic_capable(action_type) {
            return StrategySelection {
                route: StrategyRoute::Semantic,
                confidence: f.confidence.max(0.45),
                reason: format!("Model is soft-stale ({}); prefer semantic resolution", f.causes.join(", ")),
                terminal: false,
            };
        }
    }

    // Default: structured
    StrategySelection {
        route: StrategyRoute::Structured,
        confidence: freshness.map(|f| f.confidence).unwrap_or(0.9),
        reason: "Grounded structured execution is preferred".into(),
        terminal: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_structured() {
        let sel = select_route("click", None, &[], false);
        assert_eq!(sel.route, StrategyRoute::Structured);
        assert!(!sel.terminal);
    }

    #[test]
    fn test_ambiguous_semantic() {
        let sel = select_route("click", None, &[], true);
        assert_eq!(sel.route, StrategyRoute::Semantic);
    }

    #[test]
    fn test_escalation_structured_to_semantic() {
        let attempts = vec![StrategyAttempt {
            route: StrategyRoute::Structured,
            success: false,
            verified: false,
        }];
        let sel = select_route("click", None, &attempts, false);
        assert_eq!(sel.route, StrategyRoute::Semantic);
    }

    #[test]
    fn test_escalation_semantic_to_vision() {
        let attempts = vec![
            StrategyAttempt { route: StrategyRoute::Structured, success: false, verified: false },
            StrategyAttempt { route: StrategyRoute::Semantic, success: false, verified: false },
        ];
        let sel = select_route("click", None, &attempts, false);
        assert_eq!(sel.route, StrategyRoute::Vision);
    }

    #[test]
    fn test_terminal_after_vision() {
        let attempts = vec![
            StrategyAttempt { route: StrategyRoute::Structured, success: false, verified: false },
            StrategyAttempt { route: StrategyRoute::Semantic, success: false, verified: false },
            StrategyAttempt { route: StrategyRoute::Vision, success: false, verified: false },
        ];
        let sel = select_route("click", None, &attempts, false);
        assert_eq!(sel.route, StrategyRoute::TerminalFailure);
        assert!(sel.terminal);
    }

    #[test]
    fn test_non_escalatable_terminal() {
        let attempts: Vec<StrategyAttempt> = (0..3).map(|_| StrategyAttempt {
            route: StrategyRoute::Structured, success: false, verified: false,
        }).collect();
        let sel = select_route("key", None, &attempts, false);
        assert_eq!(sel.route, StrategyRoute::TerminalFailure);
    }

    #[test]
    fn test_hard_stale_refresh() {
        let freshness = FreshnessState {
            state: FreshnessLevel::HardStale,
            confidence: 0.3,
            causes: vec!["time".into()],
        };
        let sel = select_route("click", Some(&freshness), &[], false);
        assert_eq!(sel.route, StrategyRoute::Refresh);
    }

    #[test]
    fn test_soft_stale_semantic() {
        let freshness = FreshnessState {
            state: FreshnessLevel::SoftStale,
            confidence: 0.5,
            causes: vec!["event".into()],
        };
        let sel = select_route("click", Some(&freshness), &[], false);
        assert_eq!(sel.route, StrategyRoute::Semantic);
    }
}
