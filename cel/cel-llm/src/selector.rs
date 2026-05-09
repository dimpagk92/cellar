//! Tier B1: LLM-based memory selector infrastructure.
//!
//! Ships the **seam** for LLM-driven re-ranking of cortex memories at
//! read time: a trait, the input/output shapes, and the always-safe
//! fallback contract. No bundled LLM impl — production callers wire one.
//!
//! ## Why re-rank, not re-select?
//!
//! WK1's deterministic selector (FTS5 + decay + cosine boost) already
//! produces a reasonable shortlist + ranking. The LLM selector's job
//! is to **re-order** the shortlist using semantic understanding the
//! deterministic scorer can't capture (e.g. "this failure memory
//! actually answers the question even though its keywords don't
//! overlap as well").
//!
//! Re-ranking is cheaper than re-selecting because:
//! - Input tokens scale with shortlist size (~20-30 candidates), not
//!   the full workflow memory pool (potentially 10K+).
//! - The LLM never has to do retrieval — just ordering.
//! - Failure cleanly falls back to WK1's existing order (already
//!   computed); no expensive recompute.
//!
//! ## Fallback contract
//!
//! Selection is **opt-in**:
//!
//! - No selector wired → `canonical_runner` uses WK1 ordering
//!   directly (pre-B1 behaviour).
//! - Selector wired AND succeeds → memories reordered per the LLM's
//!   priority list; ids the LLM omitted are dropped (the LLM is
//!   trusted to filter, not just sort).
//! - Selector wired AND fails (LLM error / timeout / parse error /
//!   returns ids not in input) → log WARN, fall through to WK1
//!   ordering. Memory hydration always lands; just less smartly
//!   ranked. Never blocks the run.
//!
//! ## Why operate on hydrated MemoryRefs, not raw store rows?
//!
//! Two reasons:
//! 1. **No store access in the LLM impl.** The LLM only needs id +
//!    kind + summary to rank. Keeping the trait shape away from
//!    `cel-store` types avoids a dependency cycle (cel-llm depends on
//!    cel-cortex would close one, since cel-cortex depends on cel-llm
//!    for the embedder + enricher traits).
//! 2. **Caller-controlled hydration.** The runner has already paid the
//!    cost to hydrate WK1's shortlist into the planning view; the
//!    selector just re-orders the result. Read-time LLM cost stays
//!    bounded.

use async_trait::async_trait;

use crate::LlmError;

/// One candidate memory in the rerank request — minimal shape the
/// LLM needs to reason about relevance. Mirrors the LLM-facing parts
/// of `cel_contracts::MemoryRef` without dragging in the contract crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRerankItem {
    /// Stable cortex memory id. Used as the LLM's reference token in
    /// its response — the runner re-orders the original `MemoryRef`
    /// list by matching these ids.
    pub id: i64,
    /// `"outcome"` / `"prior"` / `"failure"` / `"preference"`. Lets the
    /// LLM weight by kind (e.g. failures > outcomes when the goal
    /// looks like a likely-to-fail attempt).
    pub kind: String,
    /// Caller-provided summary text. Comes from `MemoryRef.summary`
    /// (which is the runner's plain summary in the pre-A4 path or
    /// the LLM-enriched summary in the post-A4 path). The richer this
    /// text, the better the LLM's re-ranking.
    pub summary: String,
}

/// Borrowed context passed to the selector on each call.
#[derive(Debug, Clone)]
pub struct MemoryRerankContext<'a> {
    pub goal: &'a str,
    /// WK1's shortlist, in WK1's deterministic priority order. The
    /// selector should treat this as the **complete candidate set** —
    /// it must not invent ids that aren't in this list.
    pub candidates: &'a [MemoryRerankItem],
    /// Maximum number of ids the selector may return. Implementations
    /// should respect this cap; the runner enforces it defensively
    /// (extra ids beyond `max_to_keep` are dropped).
    pub max_to_keep: usize,
}

/// The contract `cel-goal-runner` calls once per planner turn (when
/// memory hydration is enabled AND a selector is wired) to re-rank
/// WK1's shortlist.
#[async_trait]
pub trait MemorySelector: Send + Sync {
    /// Re-rank `ctx.candidates` for `ctx.goal`. Returns the ids of the
    /// memories to keep, in priority order. Implementations:
    ///
    /// - MUST only return ids present in `ctx.candidates` (the runner
    ///   silently drops unknown ids; the LLM doesn't get to invent
    ///   memories).
    /// - SHOULD return at most `ctx.max_to_keep` ids (the runner
    ///   defensively truncates if more are returned).
    /// - MAY return fewer than `ctx.max_to_keep` ids — the LLM is
    ///   trusted to filter as well as sort.
    /// - MAY return an empty Vec if no candidate is relevant — the
    ///   runner persists this as "no relevant memories for this goal."
    /// - On error: the runner logs WARN and falls through to WK1's
    ///   original ordering. Implementations should error rather than
    ///   return garbage.
    async fn rerank(&self, ctx: &MemoryRerankContext<'_>) -> Result<Vec<i64>, LlmError>;

    /// Optional model identifier (e.g. `"openai:gpt-4o-mini"`). When
    /// stable, lets the runner stamp it on observability traces.
    /// Returns `None` for stubs and impls without meaningful identity.
    fn model_id(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: i64, kind: &str, summary: &str) -> MemoryRerankItem {
        MemoryRerankItem {
            id,
            kind: kind.into(),
            summary: summary.into(),
        }
    }

    /// Stub selector: reverses the input order. Lets downstream tests
    /// verify "selector actually fired" by asserting the rev-order
    /// outcome.
    pub struct ReverseSelector;
    #[async_trait]
    impl MemorySelector for ReverseSelector {
        async fn rerank(&self, ctx: &MemoryRerankContext<'_>) -> Result<Vec<i64>, LlmError> {
            Ok(ctx.candidates.iter().rev().map(|c| c.id).collect())
        }
    }

    /// Stub selector: always errors. Verifies the fallback path.
    pub struct AlwaysErrSelector;
    #[async_trait]
    impl MemorySelector for AlwaysErrSelector {
        async fn rerank(&self, _: &MemoryRerankContext<'_>) -> Result<Vec<i64>, LlmError> {
            Err(LlmError::RequestFailed("simulated".into()))
        }
    }

    /// Stub selector: returns ids the runner can't possibly know
    /// about (1, 2, 3 — guaranteed not in the candidate pool the
    /// downstream tests use). Verifies the runner's defensive
    /// "unknown id → drop" behaviour.
    pub struct InventsIdsSelector;
    #[async_trait]
    impl MemorySelector for InventsIdsSelector {
        async fn rerank(&self, _: &MemoryRerankContext<'_>) -> Result<Vec<i64>, LlmError> {
            Ok(vec![999_991, 999_992, 999_993])
        }
    }

    #[tokio::test]
    async fn reverse_selector_returns_input_in_reverse() {
        let candidates = vec![
            item(10, "outcome", "a"),
            item(20, "prior", "b"),
            item(30, "failure", "c"),
        ];
        let ctx = MemoryRerankContext {
            goal: "any",
            candidates: &candidates,
            max_to_keep: 5,
        };
        let out = ReverseSelector.rerank(&ctx).await.unwrap();
        assert_eq!(out, vec![30, 20, 10]);
    }

    #[tokio::test]
    async fn always_err_selector_returns_err_for_fallback_test() {
        let candidates = vec![item(1, "outcome", "x")];
        let ctx = MemoryRerankContext {
            goal: "g",
            candidates: &candidates,
            max_to_keep: 1,
        };
        assert!(AlwaysErrSelector.rerank(&ctx).await.is_err());
    }

    #[tokio::test]
    async fn invents_ids_selector_returns_unknown_ids_for_filter_test() {
        let candidates = vec![item(10, "outcome", "real")];
        let ctx = MemoryRerankContext {
            goal: "g",
            candidates: &candidates,
            max_to_keep: 5,
        };
        let out = InventsIdsSelector.rerank(&ctx).await.unwrap();
        assert!(out.iter().all(|id| !candidates.iter().any(|c| c.id == *id)));
    }
}
