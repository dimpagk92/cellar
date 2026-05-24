//! [`BriefReceipt`] — minimal Phase 1 surface so [`crate::types::Brief`] can
//! compile.
//!
//! The full receipt (per-source stats, dropped items, redactions, timings)
//! lands in Phase 2 per plan §8. Phase 1 ships only the fields that allow the
//! `Brief` type to compile end-to-end and lets a hand-built `Brief` carry an
//! empty receipt.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::types::{Priority, SourceId};

/// Per-source stats kept on the [`BriefReceipt`].
///
/// Phase 1 ships the field shape from plan §8; Phase 2's builder populates
/// it during `BriefBuilder::build`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStats {
    /// Number of [`crate::source::Contribution`]s the source returned.
    pub contributions: usize,
    /// Number of contributions that survived budget pruning.
    pub kept: usize,
    /// Total tokens attributed to the source after pruning.
    pub tokens: usize,
    /// Source's [`Priority`] at the time of the build.
    pub priority: Priority,
}

/// Reason a single [`crate::source::Contribution`] was dropped during
/// pruning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// Pruned to fit the [`crate::types::TokenBudget`].
    OverBudget,
    /// Removed by governance (Phase 4).
    Governance,
}

/// Record of a contribution that didn't make it into the final
/// [`crate::types::Brief`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedContribution {
    /// Source that produced the dropped contribution.
    pub source: SourceId,
    /// Why it was dropped.
    pub reason: DropReason,
    /// Tokens it would have cost.
    pub tokens: usize,
}

/// Record of a content rewrite applied by governance (Phase 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionRecord {
    /// Source whose contribution was redacted.
    pub source: SourceId,
    /// Short, opaque rule label (e.g. `"rule:no_bank_dom"`).
    pub rule: String,
}

/// Per-phase timings collected by the builder. All durations are wall-clock
/// time within `BriefBuilder::build`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Timings {
    /// Longest source `contribute()` time (max across the fan-out).
    pub fanout: Duration,
    /// Time spent tokenizing contributions.
    pub tokenize: Duration,
    /// Time spent in budget pruning.
    pub prune: Duration,
    /// Time spent in `governance.review`.
    pub governance: Duration,
    /// Total wall-clock time inside `build`.
    pub total: Duration,
}

/// Auditable record of how a [`crate::types::Brief`] was assembled.
///
/// Phase 1: types exist with the field shape from plan §8 so [`crate::types::Brief`]
/// can compile and callers can hand-construct an empty receipt for testing
/// (see [`BriefReceipt::empty`]). Phase 2's `BriefBuilder` populates every
/// field for real.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefReceipt {
    /// Wall-clock time the receipt was finalised.
    pub built_at: SystemTime,
    /// Total tokens in the assembled brief.
    pub total_tokens: usize,
    /// Per-source breakdown.
    pub by_source: HashMap<SourceId, SourceStats>,
    /// Contributions pruned for budget or governance reasons.
    pub dropped: Vec<DroppedContribution>,
    /// Governance rewrites applied to surviving contributions.
    pub redactions: Vec<RedactionRecord>,
    /// Per-phase timings.
    pub timings: Timings,
}

impl BriefReceipt {
    /// An empty receipt with `built_at = SystemTime::now()` and zero counts.
    ///
    /// Useful for tests and the Phase 1 example, where the builder isn't
    /// available yet. Phase 2 callers should let `BriefBuilder::build`
    /// produce the receipt.
    pub fn empty() -> Self {
        BriefReceipt {
            built_at: SystemTime::now(),
            total_tokens: 0,
            by_source: HashMap::new(),
            dropped: Vec::new(),
            redactions: Vec::new(),
            timings: Timings::default(),
        }
    }
}

impl Default for BriefReceipt {
    fn default() -> Self {
        BriefReceipt::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_receipt_has_zero_counts() {
        let receipt = BriefReceipt::empty();
        assert_eq!(receipt.total_tokens, 0);
        assert!(receipt.by_source.is_empty());
        assert!(receipt.dropped.is_empty());
        assert!(receipt.redactions.is_empty());
        assert_eq!(receipt.timings, Timings::default());
    }

    #[test]
    fn dropped_round_trips_through_serde_json() {
        let dropped = DroppedContribution {
            source: SourceId::new("history"),
            reason: DropReason::OverBudget,
            tokens: 42,
        };
        let json = serde_json::to_string(&dropped).expect("serialize");
        let back: DroppedContribution = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(dropped, back);
    }
}
