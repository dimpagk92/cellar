/// Loop detection for the planner.
///
/// Detects when the agent is stuck: repeating the same action,
/// ping-ponging between two actions, or acting on an unchanging context.
/// Inspired by browser-use's loop detection (added Jan 2026).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use cel_context::ScreenContext;

use crate::types::PlannedAction;

/// Window of recent entries to consider for loop detection.
const WINDOW_SIZE: usize = 8;
/// How many consecutive repeats trigger a warning at each severity.
const REPEAT_GENTLE: usize = 3;
const REPEAT_DIRECT: usize = 5;
const REPEAT_FORCEFUL: usize = 8;
/// How many unchanged context snapshots trigger stale warnings.
const STALE_GENTLE: usize = 4;
const STALE_DIRECT: usize = 6;
const STALE_FORCEFUL: usize = 8;

/// Severity of loop detection — escalates with repetitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopSeverity {
    /// Soft nudge: "Consider a different approach."
    Gentle,
    /// Structured: "List 3 alternative approaches and pick the most promising."
    Direct,
    /// Override: "STOP. Try a completely different action type or report your answer."
    Forceful,
}

/// Signal from the loop detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopSignal {
    /// No loop detected.
    None,
    /// Same action repeated N times consecutively.
    Repeat {
        action_summary: String,
        count: usize,
        severity: LoopSeverity,
    },
    /// Alternating between two actions (A-B-A-B).
    PingPong {
        action_a: String,
        action_b: String,
        severity: LoopSeverity,
    },
    /// Context fingerprint hasn't changed for N steps despite actions.
    StaleContext {
        steps_unchanged: usize,
        severity: LoopSeverity,
    },
}

impl LoopSignal {
    /// Get the severity, or None if no loop.
    pub fn severity(&self) -> Option<LoopSeverity> {
        match self {
            LoopSignal::None => Option::None,
            LoopSignal::Repeat { severity, .. }
            | LoopSignal::PingPong { severity, .. }
            | LoopSignal::StaleContext { severity, .. } => Some(*severity),
        }
    }

    /// Get an escalating nudge message appropriate for the severity.
    pub fn nudge_message(&self) -> &'static str {
        match self.severity() {
            Option::None => "",
            Some(LoopSeverity::Gentle) => {
                "WARNING: You appear to be repeating actions without progress. \
                 Consider a different approach — try a different element, scroll \
                 to reveal new content, or report your answer."
            }
            Some(LoopSeverity::Direct) => {
                "STUCK: Your recent actions have not changed the page. \
                 List 3 alternative approaches, pick the most promising one, \
                 and execute it. Do NOT repeat any previous action."
            }
            Some(LoopSeverity::Forceful) => {
                "FINAL WARNING: You are stuck in a loop. You MUST either: \
                 (a) try a COMPLETELY DIFFERENT action type you haven't used, or \
                 (b) report your best answer now. Do NOT repeat any prior action."
            }
        }
    }
}

impl std::fmt::Display for LoopSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopSignal::None => write!(f, "none"),
            LoopSignal::Repeat { action_summary, count, severity } => {
                write!(f, "Repeated '{}' {} times [{:?}]", action_summary, count, severity)
            }
            LoopSignal::PingPong { action_a, action_b, severity } => {
                write!(f, "Ping-ponging '{}' ↔ '{}' [{:?}]", action_a, action_b, severity)
            }
            LoopSignal::StaleContext { steps_unchanged, severity } => {
                write!(f, "Context unchanged {} steps [{:?}]", steps_unchanged, severity)
            }
        }
    }
}

/// Tracks recent actions and context fingerprints to detect loops.
pub struct LoopDetector {
    action_hashes: Vec<u64>,
    action_summaries: Vec<String>,
    context_hashes: Vec<u64>,
    /// How many additional steps to allow after a warning before auto-failing.
    grace_steps_remaining: Option<u32>,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            action_hashes: Vec::with_capacity(WINDOW_SIZE),
            action_summaries: Vec::with_capacity(WINDOW_SIZE),
            context_hashes: Vec::with_capacity(WINDOW_SIZE),
            grace_steps_remaining: None,
        }
    }

    /// Check for loops after recording an action and its resulting context.
    /// Returns the strongest signal detected.
    pub fn check(&mut self, action: &PlannedAction, context_hash: u64) -> LoopSignal {
        let action_hash = hash_action(action);
        let action_summary = summarize_action(action);

        self.action_hashes.push(action_hash);
        self.action_summaries.push(action_summary);
        self.context_hashes.push(context_hash);

        // Keep windows bounded
        if self.action_hashes.len() > WINDOW_SIZE {
            self.action_hashes.remove(0);
            self.action_summaries.remove(0);
        }
        if self.context_hashes.len() > WINDOW_SIZE {
            self.context_hashes.remove(0);
        }

        // Track grace period
        if let Some(ref mut remaining) = self.grace_steps_remaining {
            if *remaining == 0 {
                // Grace expired — caller should auto-fail
                return self.detect_any();
            }
            *remaining = remaining.saturating_sub(1);
        }

        self.detect_any()
    }

    /// Whether the grace period after a warning has expired (caller should auto-fail).
    pub fn should_auto_fail(&self) -> bool {
        self.grace_steps_remaining == Some(0)
    }

    /// Start a grace period of N steps after issuing a warning.
    pub fn start_grace(&mut self, steps: u32) {
        self.grace_steps_remaining = Some(steps);
    }

    /// Reset the detector (e.g., when context changes significantly).
    pub fn reset(&mut self) {
        self.action_hashes.clear();
        self.action_summaries.clear();
        self.context_hashes.clear();
        self.grace_steps_remaining = None;
    }

    fn detect_any(&self) -> LoopSignal {
        // Check for repeats first (strongest signal)
        if let Some(signal) = self.detect_repeat() {
            return signal;
        }
        if let Some(signal) = self.detect_ping_pong() {
            return signal;
        }
        if let Some(signal) = self.detect_stale_context() {
            return signal;
        }
        LoopSignal::None
    }

    fn detect_repeat(&self) -> Option<LoopSignal> {
        let last = self.action_hashes.last()?;
        let summary = self.action_summaries.last().cloned().unwrap_or_default();

        // Count consecutive repeats from the tail
        let consecutive = self.action_hashes.iter().rev().take_while(|h| *h == last).count();

        let severity = if consecutive >= REPEAT_FORCEFUL {
            Some(LoopSeverity::Forceful)
        } else if consecutive >= REPEAT_DIRECT {
            Some(LoopSeverity::Direct)
        } else if consecutive >= REPEAT_GENTLE {
            Some(LoopSeverity::Gentle)
        } else {
            Option::None
        };

        severity.map(|s| LoopSignal::Repeat {
            action_summary: summary,
            count: consecutive,
            severity: s,
        })
    }

    fn detect_ping_pong(&self) -> Option<LoopSignal> {
        if self.action_hashes.len() < 4 {
            return None;
        }
        let n = self.action_hashes.len();
        let a = self.action_hashes[n - 4];
        let b = self.action_hashes[n - 3];
        if a != b
            && self.action_hashes[n - 2] == a
            && self.action_hashes[n - 1] == b
        {
            // Check severity: 4 entries = gentle (A-B-A-B), 6+ = direct, 8+ = forceful
            let pattern_len = self.action_hashes.iter().rev()
                .zip([b, a].iter().cycle())
                .take_while(|(h, p)| h == p)
                .count();
            let severity = if pattern_len >= 8 {
                LoopSeverity::Forceful
            } else if pattern_len >= 6 {
                LoopSeverity::Direct
            } else {
                LoopSeverity::Gentle
            };
            Some(LoopSignal::PingPong {
                action_a: self.action_summaries[n - 4].clone(),
                action_b: self.action_summaries[n - 3].clone(),
                severity,
            })
        } else {
            None
        }
    }

    fn detect_stale_context(&self) -> Option<LoopSignal> {
        let last = self.context_hashes.last()?;
        let consecutive = self.context_hashes.iter().rev().take_while(|h| *h == last).count();

        let severity = if consecutive >= STALE_FORCEFUL {
            Some(LoopSeverity::Forceful)
        } else if consecutive >= STALE_DIRECT {
            Some(LoopSeverity::Direct)
        } else if consecutive >= STALE_GENTLE {
            Some(LoopSeverity::Gentle)
        } else {
            Option::None
        };

        severity.map(|s| LoopSignal::StaleContext {
            steps_unchanged: consecutive,
            severity: s,
        })
    }
}

/// Compute a fingerprint for a ScreenContext (hash of element IDs + app + window).
pub fn context_fingerprint(ctx: &ScreenContext) -> u64 {
    let mut hasher = DefaultHasher::new();
    ctx.app.hash(&mut hasher);
    ctx.window.hash(&mut hasher);
    for el in &ctx.elements {
        el.id.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_action(action: &PlannedAction) -> u64 {
    let mut hasher = DefaultHasher::new();
    match action {
        PlannedAction::Click { target_id } => {
            "click".hash(&mut hasher);
            target_id.hash(&mut hasher);
        }
        PlannedAction::Type { target_id, text } => {
            "type".hash(&mut hasher);
            target_id.as_deref().unwrap_or_default().hash(&mut hasher);
            text.hash(&mut hasher);
        }
        PlannedAction::Key { key } => {
            "key".hash(&mut hasher);
            key.hash(&mut hasher);
        }
        PlannedAction::KeyCombo { keys } => {
            "key_combo".hash(&mut hasher);
            keys.hash(&mut hasher);
        }
        PlannedAction::SetValue { target_id, value } => {
            "set_value".hash(&mut hasher);
            target_id.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        PlannedAction::Drag { from_target_id, to_target_id } => {
            "drag".hash(&mut hasher);
            from_target_id.hash(&mut hasher);
            to_target_id.hash(&mut hasher);
        }
        PlannedAction::Scroll { dx, dy } => {
            "scroll".hash(&mut hasher);
            dx.hash(&mut hasher);
            dy.hash(&mut hasher);
        }
        PlannedAction::Wait { ms } => {
            "wait".hash(&mut hasher);
            ms.hash(&mut hasher);
        }
        PlannedAction::Custom { adapter, action, .. } => {
            "custom".hash(&mut hasher);
            adapter.hash(&mut hasher);
            action.hash(&mut hasher);
        }
        PlannedAction::Done { summary, .. } => {
            "done".hash(&mut hasher);
            summary.hash(&mut hasher);
        }
        PlannedAction::Fail { reason } => {
            "fail".hash(&mut hasher);
            reason.hash(&mut hasher);
        }
        PlannedAction::Extract { goal, .. } => {
            "extract".hash(&mut hasher);
            goal.hash(&mut hasher);
        }
        PlannedAction::Act { instruction } => {
            "act".hash(&mut hasher);
            instruction.hash(&mut hasher);
        }
        PlannedAction::Batch { actions } => {
            "batch".hash(&mut hasher);
            for a in actions {
                hash_action(a).hash(&mut hasher);
            }
        }
        PlannedAction::AxAction { target_id, action, .. } => {
            "ax_action".hash(&mut hasher);
            target_id.hash(&mut hasher);
            action.hash(&mut hasher);
        }
        PlannedAction::ActivateApp { app_name } => {
            "activate_app".hash(&mut hasher);
            app_name.hash(&mut hasher);
        }
        PlannedAction::Select { from_x, from_y, to_x, to_y } => {
            "select".hash(&mut hasher);
            from_x.hash(&mut hasher);
            from_y.hash(&mut hasher);
            to_x.hash(&mut hasher);
            to_y.hash(&mut hasher);
        }
        PlannedAction::CdpEval { expression } => {
            "cdp_eval".hash(&mut hasher);
            expression.hash(&mut hasher);
        }
        PlannedAction::NotebookWrites { .. } => {
            "noop".hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn summarize_action(action: &PlannedAction) -> String {
    match action {
        PlannedAction::Click { target_id } => format!("click({})", target_id),
        PlannedAction::Type { target_id, text } => {
            let t = if text.len() > 15 { &text[..15] } else { text };
            format!("type({}, \"{}\")", target_id.as_deref().unwrap_or("?"), t)
        }
        PlannedAction::Key { key } => format!("key({})", key),
        PlannedAction::KeyCombo { keys } => format!("combo({})", keys.join("+")),
        PlannedAction::SetValue { target_id, .. } => format!("set_value:{}", target_id),
        PlannedAction::Drag { from_target_id, to_target_id } => {
            format!("drag:{}→{}", from_target_id, to_target_id)
        }
        PlannedAction::Scroll { dx, dy } => format!("scroll({},{})", dx, dy),
        PlannedAction::Wait { ms } => format!("wait({}ms)", ms),
        PlannedAction::Custom { adapter, action, .. } => format!("custom({}.{})", adapter, action),
        PlannedAction::Extract { goal, data } => format!("extract({}: {})", goal, &data[..data.len().min(30)]),
        PlannedAction::Done { summary, .. } => format!("done({})", summary),
        PlannedAction::Fail { reason } => format!("fail({})", reason),
        PlannedAction::Act { instruction } => {
            let instr = if instruction.len() > 30 { &instruction[..30] } else { instruction };
            format!("act(\"{}\")", instr)
        }
        PlannedAction::Batch { actions } => {
            let parts: Vec<String> = actions.iter().map(summarize_action).collect();
            format!("batch[{}]", parts.join(","))
        }
        PlannedAction::AxAction { target_id, action, .. } => format!("ax_action({},{})", target_id, action),
        PlannedAction::ActivateApp { app_name } => format!("activate_app({})", app_name),
        PlannedAction::Select { from_x, from_y, to_x, to_y } => {
            format!("select({},{})→({},{})", from_x, from_y, to_x, to_y)
        }
        PlannedAction::CdpEval { expression } => {
            let expr = if expression.len() > 30 { &expression[..30] } else { expression };
            format!("cdp_eval(\"{}\")", expr)
        }
        PlannedAction::NotebookWrites { .. } => "noop(notebook)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(id: &str) -> PlannedAction {
        PlannedAction::Click {
            target_id: id.into(),
        }
    }

    #[test]
    fn test_no_loop_on_varied_actions() {
        let mut det = LoopDetector::new();
        assert_eq!(det.check(&click("a"), 1), LoopSignal::None);
        assert_eq!(det.check(&click("b"), 2), LoopSignal::None);
        assert_eq!(det.check(&click("c"), 3), LoopSignal::None);
    }

    #[test]
    fn test_repeat_gentle() {
        let mut det = LoopDetector::new();
        det.check(&click("btn"), 1);
        det.check(&click("btn"), 2);
        let signal = det.check(&click("btn"), 3);
        match signal {
            LoopSignal::Repeat { count, severity, .. } => {
                assert_eq!(count, 3);
                assert_eq!(severity, LoopSeverity::Gentle);
            }
            other => panic!("Expected Repeat Gentle, got {:?}", other),
        }
    }

    #[test]
    fn test_repeat_direct() {
        let mut det = LoopDetector::new();
        for i in 0..5 {
            det.check(&click("btn"), i);
        }
        match det.check(&click("btn"), 5) {
            LoopSignal::Repeat { severity, .. } => assert_eq!(severity, LoopSeverity::Direct),
            other => panic!("Expected Repeat Direct, got {:?}", other),
        }
    }

    #[test]
    fn test_ping_pong_detection() {
        let mut det = LoopDetector::new();
        det.check(&click("a"), 1);
        det.check(&click("b"), 2);
        det.check(&click("a"), 3);
        let signal = det.check(&click("b"), 4);
        match signal {
            LoopSignal::PingPong { severity, .. } => {
                assert_eq!(severity, LoopSeverity::Gentle);
            }
            other => panic!("Expected PingPong, got {:?}", other),
        }
    }

    #[test]
    fn test_stale_context_gentle() {
        let mut det = LoopDetector::new();
        // Different actions but same context hash — gentle at 4
        det.check(&click("a"), 42);
        det.check(&click("b"), 42);
        det.check(&click("c"), 42);
        let signal = det.check(&click("d"), 42);
        match signal {
            LoopSignal::StaleContext { steps_unchanged, severity } => {
                assert_eq!(steps_unchanged, 4);
                assert_eq!(severity, LoopSeverity::Gentle);
            }
            other => panic!("Expected StaleContext Gentle, got {:?}", other),
        }
    }

    #[test]
    fn test_stale_context_direct() {
        let mut det = LoopDetector::new();
        for i in 0..6 {
            det.check(&click(&format!("{}", i)), 42);
        }
        match det.check(&click("x"), 42) {
            LoopSignal::StaleContext { severity, .. } => assert_eq!(severity, LoopSeverity::Direct),
            other => panic!("Expected StaleContext Direct, got {:?}", other),
        }
    }

    #[test]
    fn test_grace_period() {
        let mut det = LoopDetector::new();
        det.check(&click("btn"), 1);
        det.check(&click("btn"), 2);
        det.check(&click("btn"), 3); // Gentle at 3

        det.start_grace(2);
        assert!(!det.should_auto_fail());

        det.check(&click("btn"), 4); // Grace 2 → 1
        assert!(!det.should_auto_fail());

        det.check(&click("btn"), 5); // Grace 1 → 0
        assert!(det.should_auto_fail());
    }

    #[test]
    fn test_reset_clears_state() {
        let mut det = LoopDetector::new();
        det.check(&click("a"), 1);
        det.check(&click("a"), 1);
        det.check(&click("a"), 1);
        det.reset();
        assert_eq!(det.check(&click("a"), 2), LoopSignal::None);
    }
}
