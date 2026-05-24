//! Summarizer trait and supporting types.
//!
//! The summarizer is the seam between the memory subsystem and whichever
//! LLM client a deployment uses to synthesize end-of-session summaries,
//! daily rollups, and rule-week rollups (see `cellar-memory-manager.md`
//! §9). The trait is intentionally tiny — given a list of chunks plus an
//! optional pre-prompt the caller wants to inject (e.g. "session
//! summary", "daily rollup for 2026-05-23"), produce a short string
//! synthesis. The provider is responsible for writing the resulting
//! `MemoryChunk` and linking its constituents in
//! `memory_summary_members`; the summarizer itself only generates text.
//!
//! Two production implementations live in
//! [`cel-memory-sqlite`](../../../cel_memory_sqlite/summarizer/index.html):
//! `AnthropicSummarizer` (default cloud path, Claude Haiku 4.5) and
//! `OllamaSummarizer` (local fallback, pinned to
//! `llama3.2:3b-instruct-q4_K_M` per §1.1 decision 3).
//!
//! Object-safe: stored as `Arc<dyn Summarizer>` everywhere.
//!
//! # Test support
//!
//! [`MockSummarizer`] returns a fixed canned string for every call and
//! records the chunks it received for assertion. It's a `pub` helper
//! (no feature gate) so downstream tests can wire it directly, in the
//! same shape as [`crate::ClosureHook`] for [`crate::MemoryWriteHook`].

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use thiserror::Error;

use crate::chunk::MemoryChunk;

/// Errors produced by a [`Summarizer`].
///
/// Distinct from [`crate::MemoryError`] so the trait can be used in
/// contexts that don't otherwise touch the memory crate's error type.
/// The SQLite provider wraps these into `MemoryError::Provider` at the
/// call site.
#[derive(Debug, Error)]
pub enum SummarizerError {
    /// The summarizer's LLM client returned an error (rate limit, auth,
    /// server error, malformed response).
    #[error("provider error: {0}")]
    Provider(String),

    /// The summarizer received no chunks to summarize and refuses to
    /// fabricate a summary out of thin air. Callers should treat this
    /// as "no summary applicable" rather than retrying.
    #[error("no chunks to summarize")]
    NoInput,

    /// The summarizer was configured incorrectly (missing API key, model
    /// not available, etc.). Surfaced at construction time when possible;
    /// at call time only if discovery is lazy.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// An unexpected internal error. Indicates a bug.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Result alias for summarizer operations.
pub type SummarizerResult<T> = std::result::Result<T, SummarizerError>;

/// Context the caller can pass alongside the chunks to bias the prompt
/// the summarizer assembles.
///
/// Provider-agnostic: the field is wire-shape neutral so the same value
/// flows through `AnthropicSummarizer`, `OllamaSummarizer`, or any
/// future implementation. The Anthropic/Ollama impls render this into a
/// system prompt that matches §9.4.
#[derive(Debug, Clone, Default)]
pub struct SummaryContext {
    /// Human-readable label for the kind of summary being produced —
    /// e.g. `"session"`, `"day 2026-05-23"`, `"week of 2026-05-18 for
    /// rule pii_redact"`. Inserted into the prompt so the model picks
    /// up the unit of summarization.
    pub kind_label: Option<String>,
    /// Optional caller-supplied note ("user closed the chat
    /// mid-conversation", "first daily rollup after deploy"). Appended
    /// verbatim after the chunks.
    pub note: Option<String>,
    /// Optional cap on the output length, in words. The default impls
    /// pass this through as a "Maximum N words" instruction; the
    /// caller is responsible for any post-hoc truncation.
    pub max_words: Option<u32>,
}

/// The summarizer trait. Object-safe.
///
/// Implementations MUST be `Send + Sync` because the provider may share
/// a single summarizer across tokio tasks (e.g. the cron sweeper and
/// the embedded agent calling `summarize_session` in parallel).
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Human-readable identifier for the summarizer (e.g.
    /// `"anthropic:claude-haiku-4-5"`, `"ollama:llama3.2:3b-instruct-q4_K_M"`,
    /// `"mock"`). Used for tracing/diagnostics; not parsed by the
    /// caller.
    fn name(&self) -> &str;

    /// Produce a summary string for the given chunks. Implementations
    /// should respect [`SummaryContext::max_words`] when set and
    /// produce neutral past-tense prose per §9.4.
    ///
    /// Implementations MUST return [`SummarizerError::NoInput`] when
    /// `chunks` is empty. The provider relies on this to skip writing
    /// a `JobSummary` for sessions that have no member chunks.
    async fn summarize(
        &self,
        chunks: &[MemoryChunk],
        ctx: &SummaryContext,
    ) -> SummarizerResult<String>;
}

/// Test double for [`Summarizer`]. Returns a fixed canned response and
/// records the chunks it received for assertion. Exposed unconditionally
/// so downstream integration tests can use it without a circular
/// dev-dep, matching [`crate::ClosureHook`]'s shape for write hooks.
pub struct MockSummarizer {
    name: String,
    canned: String,
    calls: Mutex<Vec<MockSummaryCall>>,
}

/// One recorded invocation of [`MockSummarizer::summarize`].
#[derive(Debug, Clone)]
pub struct MockSummaryCall {
    /// Snapshot of the chunk IDs received, in order.
    pub chunk_ids: Vec<String>,
    /// Snapshot of the context received.
    pub kind_label: Option<String>,
    /// Snapshot of any caller note.
    pub note: Option<String>,
    /// Snapshot of the requested cap.
    pub max_words: Option<u32>,
}

impl MockSummarizer {
    /// Construct a mock that returns a fixed canned summary for every
    /// call. Returns an `Arc` so the caller can clone the handle and
    /// still assert against the recorded calls.
    pub fn new(canned: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            name: "mock".into(),
            canned: canned.into(),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Snapshot the calls this summarizer has seen.
    pub fn calls(&self) -> Vec<MockSummaryCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Convenience: number of calls so far.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl Summarizer for MockSummarizer {
    fn name(&self) -> &str {
        &self.name
    }

    async fn summarize(
        &self,
        chunks: &[MemoryChunk],
        ctx: &SummaryContext,
    ) -> SummarizerResult<String> {
        if chunks.is_empty() {
            return Err(SummarizerError::NoInput);
        }
        self.calls.lock().unwrap().push(MockSummaryCall {
            chunk_ids: chunks.iter().map(|c| c.id.clone()).collect(),
            kind_label: ctx.kind_label.clone(),
            note: ctx.note.clone(),
            max_words: ctx.max_words,
        });
        Ok(self.canned.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkKind, ChunkSource, MemoryTier};
    use chrono::Utc;
    use serde_json::Value;

    fn chunk(id: &str, content: &str) -> MemoryChunk {
        MemoryChunk {
            id: id.into(),
            created_at: Utc::now(),
            kind: ChunkKind::Chat,
            tier: MemoryTier::Session,
            source: ChunkSource::Embedded,
            session_id: Some("s1".into()),
            project_root: None,
            caller_id: "embedded".into(),
            content: content.into(),
            metadata: Value::Null,
            importance: 0.5,
            pinned: false,
            superseded_by: None,
            embedding_model: "mock".into(),
            embedding_dim: 0,
        }
    }

    #[tokio::test]
    async fn mock_returns_canned_text() {
        let s = MockSummarizer::new("done");
        let chunks = vec![chunk("a", "first"), chunk("b", "second")];
        let ctx = SummaryContext {
            kind_label: Some("session".into()),
            ..Default::default()
        };
        let out = s.summarize(&chunks, &ctx).await.unwrap();
        assert_eq!(out, "done");
        assert_eq!(s.call_count(), 1);
        let calls = s.calls();
        assert_eq!(calls[0].chunk_ids, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(calls[0].kind_label.as_deref(), Some("session"));
    }

    #[tokio::test]
    async fn mock_no_input_errors() {
        let s = MockSummarizer::new("ignored");
        let err = s
            .summarize(&[], &SummaryContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, SummarizerError::NoInput));
    }

    #[tokio::test]
    async fn mock_name_is_mock() {
        let s = MockSummarizer::new("x");
        assert_eq!(s.name(), "mock");
    }
}
