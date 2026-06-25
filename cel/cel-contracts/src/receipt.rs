//! Execution receipt — the canonical, core-emitted record of one dispatched
//! action: `intent → dispatch route → observed effect → evidence`.
//!
//! A runtime can emit an [`ExecutionReceipt`] for every action it dispatches.
//! Surfaces can propagate it unflattened, timelines can append it, and later
//! prompts or memory systems can consume it.
//!
//! Before this type, the only "receipt" with route/verification detail was the
//! MCP `ActionReceipt`, *reconstructed at the surface* from a static switch on
//! the action kind — so it mislabels e.g. a DOM `click` routed via CDP as
//! `native_input`. The point of this contract is that the route and the
//! observed-effect verdict are recorded by the code that actually did the work.
//!
//! **Contracts policy:** types + serde only. Id / timestamp generation is a
//! runtime concern and lives at the emission site, not here.

use crate::actions::EffectExpectation;
use crate::view::EvidenceRef;
use serde::{Deserialize, Serialize};

/// The route the runtime *actually* took to dispatch an action — observed
/// truth, not a guess from the action kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum DispatchRoute {
    /// Chrome DevTools Protocol (browser DOM).
    Cdp,
    /// macOS Accessibility action.
    Accessibility,
    /// Synthesized native input (CGEvent key / mouse).
    NativeInput,
    /// A registered adapter's typed action.
    Adapter { name: String, op: String },
    /// App focus / lifecycle (activate / launch / quit / focus lock).
    Focus,
    /// Not yet classified.
    Other { detail: String },
}

/// Whether the post-dispatch effect was confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedStatus {
    /// The expected effect was observed.
    Observed,
    /// An effect was expected but did not materialise before timeout.
    TimedOut,
    /// No effect verification was requested for this action.
    NotChecked,
    /// The observed state contradicted the expectation.
    Contradicted,
}

/// How the effect was (or would be) verified. Generalised beyond the browser
/// [`EffectExpectation`] so native / adapter routes fit the same model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservedKind {
    /// Browser DOM predicate (CDP `wait_for_effect`).
    SelectorEffect { expectation: EffectExpectation },
    /// An adapter's declared readback action.
    AdapterReadback { action: String },
    /// A native accessibility re-read.
    AxReread,
    /// No verification mechanism applies.
    None,
}

/// The post-dispatch observation: did the expected effect happen?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedEffect {
    pub status: ObservedStatus,
    pub kind: ObservedKind,
    /// Human-readable detail (what was observed / why it timed out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ObservedEffect {
    /// No verification was requested for this action.
    pub fn not_checked() -> Self {
        Self {
            status: ObservedStatus::NotChecked,
            kind: ObservedKind::None,
            detail: None,
        }
    }

    /// A browser [`EffectExpectation`] was confirmed.
    pub fn selector_observed(expectation: EffectExpectation) -> Self {
        Self {
            status: ObservedStatus::Observed,
            kind: ObservedKind::SelectorEffect { expectation },
            detail: None,
        }
    }

    /// A browser [`EffectExpectation`] timed out; `detail` names what we saw
    /// instead (the diagnostic `wait_for_effect` produced).
    pub fn selector_timed_out(expectation: EffectExpectation, detail: impl Into<String>) -> Self {
        Self {
            status: ObservedStatus::TimedOut,
            kind: ObservedKind::SelectorEffect { expectation },
            detail: Some(detail.into()),
        }
    }
}

/// Terminal status of a dispatched action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Ok,
    Failed,
    Vetoed,
    Denied,
    TimedOut,
}

/// Canonical, core-emitted record of one dispatched action.
///
/// One receipt per action the runtime executes. Carries the intent (action kind
/// + target), the actual dispatch route, the observed effect, evidence
/// references, timing, and terminal status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    /// Process-unique id, assigned at the emission site.
    pub receipt_id: String,
    /// Run / session scope, when the caller or session provides one. `None`
    /// until run-scoping lands (see plan Phase 1 / open decision #1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Per-request trace id (reuses the IPC `trace_id` when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Action kind, e.g. `"click"`, `"set_value"`, `"type"`.
    pub action_kind: String,
    /// Target identifier (element id / app / url) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The route the runtime actually took.
    pub route: DispatchRoute,
    /// The post-dispatch observation.
    pub observed_effect: ObservedEffect,
    /// Post-execution evidence references (observations / readbacks). Empty in
    /// the first slice; populated by reference in a later phase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    pub requested_at_ms: u64,
    pub completed_at_ms: u64,
    pub duration_ms: u64,
    pub status: ReceiptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::EffectExpectation;

    fn sample() -> ExecutionReceipt {
        ExecutionReceipt {
            receipt_id: "rcpt_1".into(),
            run_id: None,
            trace_id: Some("trace-abc".into()),
            action_kind: "click".into(),
            target: Some("dom:button:submit".into()),
            route: DispatchRoute::Cdp,
            observed_effect: ObservedEffect::selector_observed(
                EffectExpectation::SelectorAppears {
                    selector: ".success".into(),
                    timeout_ms: 2_000,
                },
            ),
            evidence: Vec::new(),
            requested_at_ms: 100,
            completed_at_ms: 250,
            duration_ms: 150,
            status: ReceiptStatus::Ok,
            error: None,
        }
    }

    #[test]
    fn execution_receipt_round_trips() {
        let r = sample();
        let json = serde_json::to_string(&r).unwrap();
        let back: ExecutionReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.route, DispatchRoute::Cdp);
        assert_eq!(back.observed_effect.status, ObservedStatus::Observed);
        assert_eq!(back.duration_ms, 150);
    }

    #[test]
    fn empty_optionals_are_omitted_from_json() {
        // run_id / error / evidence default away so the wire stays compact and
        // older readers that never saw them keep working.
        let json = serde_json::to_string(&sample()).unwrap();
        assert!(!json.contains("run_id"));
        assert!(!json.contains("\"error\""));
        assert!(!json.contains("evidence"));
        assert!(json.contains("trace-abc"));
    }

    #[test]
    fn not_checked_effect_defaults() {
        let e = ObservedEffect::not_checked();
        assert_eq!(e.status, ObservedStatus::NotChecked);
        assert!(matches!(e.kind, ObservedKind::None));
    }

    #[test]
    fn timed_out_effect_carries_detail() {
        let e = ObservedEffect::selector_timed_out(
            EffectExpectation::DomChanged { timeout_ms: 2_000 },
            "no DOM change observed",
        );
        assert_eq!(e.status, ObservedStatus::TimedOut);
        assert_eq!(e.detail.as_deref(), Some("no DOM change observed"));
    }

    #[test]
    fn adapter_route_serializes_with_fields() {
        let route = DispatchRoute::Adapter {
            name: "numbers".into(),
            op: "write_cells".into(),
        };
        let json = serde_json::to_string(&route).unwrap();
        assert!(json.contains("\"route\":\"adapter\""));
        let back: DispatchRoute = serde_json::from_str(&json).unwrap();
        assert_eq!(route, back);
    }
}
