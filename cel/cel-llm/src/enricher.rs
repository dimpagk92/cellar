//! Tier A4: memory enrichment infrastructure.
//!
//! Ships the **seam** for LLM-driven enrichment of cortex memories at
//! write time: a trait, the input/output shapes, and a no-op default
//! pattern. No bundled LLM call; production callers wire one.
//!
//! ## Why write-time, not read-time?
//!
//! Enrichment costs an LLM call per memory write. With write-time
//! enrichment, that cost is amortized across all future reads of the
//! memory — one call vs hundreds. Read-time enrichment (per-turn
//! re-summarization) has the opposite economics and would multiply
//! latency on the hot path. We never want a hot-path LLM call we
//! can't avoid.
//!
//! ## What enrichment does
//!
//! Given a memory's raw content + the runner's plain summary, the
//! enricher produces:
//!
//! - A **richer summary** — multi-sentence if useful, mentions
//!   keywords / entities the planner will want to match against.
//! - A **set of tags** — short keywords (one or two words each) that
//!   describe the memory's domain, action, and outcome. Tags
//!   complement the summary in the FTS5 index (WK1 indexes summary +
//!   content + tags).
//!
//! ## Fallback contract
//!
//! Enrichment is **opt-in**:
//!
//! - No enricher wired → `canonical_runner` writes the plain summary
//!   + the `["canonical_runner"]` tag (current pre-A4 behaviour).
//! - Enricher wired AND succeeds → the LLM-produced summary + the
//!   merged `["canonical_runner", ...llm_tags]` tag set.
//! - Enricher wired AND fails (LLM error / timeout) → log WARN, fall
//!   through to the plain-summary path. Memory still lands; just less
//!   richly. Never blocks the run.

use async_trait::async_trait;

use crate::LlmError;

/// Input to one enrichment call.
///
/// Borrowed-data — the runner has these strings already and the
/// enricher implementation is expected to hand them to its prompt
/// without copying.
#[derive(Debug, Clone)]
pub struct MemoryEnrichmentInput<'a> {
    /// The plain one-line summary the runner produced (e.g.
    /// `"Submitted invoice via Concur successfully"`). Always present.
    pub plain_summary: &'a str,
    /// Discriminator for the structured payload. Mirrors
    /// `cel_store::cortex_memory::MemoryKind::as_str()` — `"outcome"`
    /// / `"prior"` / `"failure"` / `"preference"`.
    pub kind: &'a str,
    /// The structured content payload as JSON text. Lets the enricher
    /// see action targets, extracted data, etc. without forcing the
    /// runner to re-parse and pass them as separate fields.
    pub content_json: &'a str,
    /// The goal text that produced this memory. Helps the enricher
    /// align tags + summary phrasing with planner-side keywords.
    pub goal: &'a str,
}

/// Output of one enrichment call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEnrichmentOutput {
    /// Replaces the plain summary on the memory record. Must be
    /// non-empty — implementations that have nothing useful to add
    /// should return `Ok(MemoryEnrichmentOutput::passthrough(input))`
    /// or simply return Err so the runner falls through.
    pub enriched_summary: String,
    /// Tags to **merge** with the runner's default tag set
    /// (`["canonical_runner"]`). Implementations should produce 1–6
    /// short tokens (one or two words each); the runner caps the
    /// final tag count at 16 to bound storage growth.
    pub tags: Vec<String>,
}

impl MemoryEnrichmentOutput {
    /// Convenience constructor for an enricher that just wants to
    /// pass the plain summary through with no added tags. Useful as a
    /// safe default for implementations that don't yet know what to
    /// add.
    pub fn passthrough(input: &MemoryEnrichmentInput<'_>) -> Self {
        Self {
            enriched_summary: input.plain_summary.to_string(),
            tags: Vec::new(),
        }
    }
}

/// The contract `cel-goal-runner` calls once per memory write when
/// the caller has wired an enricher via `with_memory_enricher`.
///
/// Keep the surface minimal — one async call per memory. If you need
/// batching (enrich many memories at once), wrap one of these at a
/// higher layer rather than expanding the trait.
#[async_trait]
pub trait MemoryEnricher: Send + Sync {
    /// Enrich one memory. On success, the runner uses the enriched
    /// summary + merges the tags with `"canonical_runner"`. On error,
    /// the runner logs WARN and writes the plain summary unchanged
    /// (always-safe fallback — A4 never blocks the run).
    async fn enrich(
        &self,
        input: &MemoryEnrichmentInput<'_>,
    ) -> Result<MemoryEnrichmentOutput, LlmError>;

    /// Optional model identifier (e.g. `"openai:gpt-4o-mini"` or
    /// `"local-onnx:enricher-v1"`). When stable, the runner can stamp
    /// it on writes for later observability. Returns `None` for test
    /// stubs and impls without meaningful identity.
    fn model_id(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic stub for downstream tests (cel-goal-runner). Adds
    /// a fixed tag set and prefixes the summary so we can assert that
    /// enrichment fired vs not. Sync internally; the async signature
    /// is just trait conformance.
    pub struct StubEnricher;

    #[async_trait]
    impl MemoryEnricher for StubEnricher {
        async fn enrich(
            &self,
            input: &MemoryEnrichmentInput<'_>,
        ) -> Result<MemoryEnrichmentOutput, LlmError> {
            Ok(MemoryEnrichmentOutput {
                enriched_summary: format!("[enriched] {}", input.plain_summary),
                tags: vec!["stub_tag".into(), input.kind.to_string()],
            })
        }
        fn model_id(&self) -> Option<&str> {
            Some("stub:test")
        }
    }

    /// Deterministic always-error stub — used to verify the runner's
    /// fallback path actually fires.
    pub struct AlwaysErrEnricher;

    #[async_trait]
    impl MemoryEnricher for AlwaysErrEnricher {
        async fn enrich(
            &self,
            _input: &MemoryEnrichmentInput<'_>,
        ) -> Result<MemoryEnrichmentOutput, LlmError> {
            Err(LlmError::RequestFailed("simulated enricher failure".into()))
        }
    }

    #[tokio::test]
    async fn stub_enricher_returns_prefixed_summary_and_tags() {
        let input = MemoryEnrichmentInput {
            plain_summary: "Submitted invoice",
            kind: "outcome",
            content_json: "{}",
            goal: "submit invoice in Concur",
        };
        let out = StubEnricher.enrich(&input).await.unwrap();
        assert_eq!(out.enriched_summary, "[enriched] Submitted invoice");
        assert_eq!(out.tags, vec!["stub_tag", "outcome"]);
    }

    #[tokio::test]
    async fn always_err_enricher_returns_err_for_fallback_test() {
        let input = MemoryEnrichmentInput {
            plain_summary: "x",
            kind: "outcome",
            content_json: "{}",
            goal: "g",
        };
        assert!(AlwaysErrEnricher.enrich(&input).await.is_err());
    }

    #[test]
    fn passthrough_output_uses_plain_summary_and_no_tags() {
        let input = MemoryEnrichmentInput {
            plain_summary: "the original",
            kind: "outcome",
            content_json: "{}",
            goal: "g",
        };
        let out = MemoryEnrichmentOutput::passthrough(&input);
        assert_eq!(out.enriched_summary, "the original");
        assert!(out.tags.is_empty());
    }
}
