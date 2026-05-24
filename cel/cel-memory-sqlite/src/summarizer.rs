//! Production [`Summarizer`] implementations backed by the cellar LLM
//! router.
//!
//! Two impls live here, one per backend the v1 plan calls out
//! (`cellar-memory-manager.md` §9.3):
//!
//! - [`AnthropicSummarizer`] — default cloud path, Claude Haiku 4.5.
//! - [`OllamaSummarizer`] — local fallback, pinned to
//!   `llama3.2:3b-instruct-q4_K_M` per §1.1 decision 3.
//!
//! Selection lives in [`build_default`], which reads
//! `CELLAR_MEMORY_SUMMARIZER_PROVIDER` (`anthropic` | `ollama`,
//! default `anthropic`) and falls back to Ollama when the requested
//! provider is Anthropic but `ANTHROPIC_API_KEY` is missing. Daemons
//! wire this at boot and pass the resulting handle into
//! [`crate::SqliteMemoryProvider::with_summarizer`].
//!
//! The transport is [`cellar_llm_router`]'s `LlmProvider`. Both impls
//! own an `Arc<dyn LlmProvider>` so callers can supply a real client
//! or a `MockProvider` for tests — but **production code should not
//! pass a mock**; tests that need to short-circuit the LLM call should
//! use [`cel_memory::MockSummarizer`] directly.

use std::sync::Arc;

use async_trait::async_trait;
use cel_memory::{
    summarizer::{Summarizer, SummarizerError, SummarizerResult, SummaryContext},
    MemoryChunk,
};
use cellar_llm_router::{
    providers::{AnthropicProvider, OllamaProvider},
    types::{CompletionRequest, ContentBlock},
    LlmProvider,
};

/// Default Anthropic model used for summarization. Cellar pins Haiku 4.5
/// per the memory-manager plan §9.3 — cheap enough for daily rollups,
/// strong enough for multi-event synthesis.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-4-5";

/// Default Ollama model used for local-fallback summarization. Pinned
/// per `cellar-memory-manager.md` §1.1 decision 3; `cellar doctor`
/// verifies presence.
pub const DEFAULT_OLLAMA_MODEL: &str = "llama3.2:3b-instruct-q4_K_M";

/// Default cap on a summary's length, in words. The model is told to
/// stay under this; the SQLite provider does not truncate further.
const DEFAULT_MAX_WORDS: u32 = 200;

/// Env var selecting the summarizer backend at daemon boot. Values:
/// `anthropic` (default) or `ollama`.
pub const PROVIDER_ENV: &str = "CELLAR_MEMORY_SUMMARIZER_PROVIDER";
/// Env var holding the Anthropic API key for [`AnthropicSummarizer`].
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Anthropic-backed [`Summarizer`].
///
/// Wraps an `Arc<dyn LlmProvider>` so injecting a mock for unit tests
/// is trivial. Production callers construct via
/// [`AnthropicSummarizer::from_env`] or by passing a real
/// [`AnthropicProvider`].
pub struct AnthropicSummarizer {
    name: String,
    model: String,
    client: Arc<dyn LlmProvider>,
}

impl AnthropicSummarizer {
    /// Construct from an existing `LlmProvider` and the model id to
    /// request. The provider type is erased on purpose — pass an
    /// `Arc::new(AnthropicProvider::new(...)?)` in production, or any
    /// other provider for parity testing.
    pub fn new(client: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        let model: String = model.into();
        let name = format!("anthropic:{model}");
        Self {
            name,
            model,
            client,
        }
    }

    /// Construct from environment. Requires `ANTHROPIC_API_KEY` to be
    /// set; uses [`DEFAULT_ANTHROPIC_MODEL`] unless
    /// `CELLAR_MEMORY_SUMMARIZER_MODEL` overrides it.
    pub fn from_env() -> SummarizerResult<Self> {
        let key = std::env::var(ANTHROPIC_API_KEY_ENV).ok();
        let provider = AnthropicProvider::new(key, None).map_err(|e| {
            SummarizerError::InvalidConfig(format!(
                "constructing AnthropicProvider for memory summarizer: {e}"
            ))
        })?;
        let model = std::env::var("CELLAR_MEMORY_SUMMARIZER_MODEL")
            .unwrap_or_else(|_| DEFAULT_ANTHROPIC_MODEL.to_string());
        Ok(Self::new(Arc::new(provider), model))
    }
}

#[async_trait]
impl Summarizer for AnthropicSummarizer {
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
        let req = build_request(&self.model, chunks, ctx);
        let resp = self
            .client
            .complete(req)
            .await
            .map_err(|e| SummarizerError::Provider(e.to_string()))?;
        Ok(collect_text(resp.content))
    }
}

/// Ollama-backed [`Summarizer`]. Local fallback.
///
/// Wraps an `Arc<dyn LlmProvider>` so injecting a mock for unit tests
/// is trivial. Production callers construct via
/// [`OllamaSummarizer::from_env`] or by passing a real
/// [`OllamaProvider`]. The default model is pinned to
/// [`DEFAULT_OLLAMA_MODEL`] per §1.1 decision 3.
pub struct OllamaSummarizer {
    name: String,
    model: String,
    client: Arc<dyn LlmProvider>,
}

impl OllamaSummarizer {
    /// Construct from an existing `LlmProvider` and the model id.
    pub fn new(client: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        let model: String = model.into();
        let name = format!("ollama:{model}");
        Self {
            name,
            model,
            client,
        }
    }

    /// Construct from environment. Honors `OLLAMA_BASE_URL` if set;
    /// otherwise uses Ollama's default local port. Model is pinned to
    /// [`DEFAULT_OLLAMA_MODEL`] regardless of caller env — the pin is
    /// the entire point of the local-fallback contract — but advanced
    /// users may override via `CELLAR_MEMORY_SUMMARIZER_MODEL`.
    pub fn from_env() -> SummarizerResult<Self> {
        let base = std::env::var("OLLAMA_BASE_URL").ok();
        let provider = OllamaProvider::new(base).map_err(|e| {
            SummarizerError::InvalidConfig(format!(
                "constructing OllamaProvider for memory summarizer: {e}"
            ))
        })?;
        let model = std::env::var("CELLAR_MEMORY_SUMMARIZER_MODEL")
            .unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_string());
        Ok(Self::new(Arc::new(provider), model))
    }
}

#[async_trait]
impl Summarizer for OllamaSummarizer {
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
        let req = build_request(&self.model, chunks, ctx);
        let resp = self
            .client
            .complete(req)
            .await
            .map_err(|e| SummarizerError::Provider(e.to_string()))?;
        Ok(collect_text(resp.content))
    }
}

/// Build the default summarizer from environment. Selection rules:
///
/// 1. `CELLAR_MEMORY_SUMMARIZER_PROVIDER=anthropic` (or unset) → try
///    [`AnthropicSummarizer::from_env`]. If `ANTHROPIC_API_KEY` is
///    missing OR construction fails, fall back to Ollama.
/// 2. `CELLAR_MEMORY_SUMMARIZER_PROVIDER=ollama` → build
///    [`OllamaSummarizer::from_env`] directly.
/// 3. Any other value → treat as Anthropic with fallback (forgiving).
///
/// Returns an `Arc<dyn Summarizer>` ready to plug into
/// [`crate::SqliteMemoryProvider::with_summarizer`].
pub fn build_default() -> SummarizerResult<Arc<dyn Summarizer>> {
    let kind = std::env::var(PROVIDER_ENV)
        .ok()
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "anthropic".to_string());

    match kind.as_str() {
        "ollama" => Ok(Arc::new(OllamaSummarizer::from_env()?)),
        _ => {
            // Anthropic (or unknown — treat as anthropic w/ fallback).
            let key_present = std::env::var(ANTHROPIC_API_KEY_ENV)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .is_some();
            if key_present {
                match AnthropicSummarizer::from_env() {
                    Ok(s) => Ok(Arc::new(s)),
                    Err(_) => Ok(Arc::new(OllamaSummarizer::from_env()?)),
                }
            } else {
                tracing::info!(
                    "ANTHROPIC_API_KEY not set; memory summarizer falling back to Ollama \
                     ({DEFAULT_OLLAMA_MODEL})"
                );
                Ok(Arc::new(OllamaSummarizer::from_env()?))
            }
        }
    }
}

/// Build the system prompt for a summarization call, structured to the
/// sketch in `cellar-memory-manager.md` §9.4.
fn build_system_prompt(ctx: &SummaryContext) -> String {
    let max_words = ctx.max_words.unwrap_or(DEFAULT_MAX_WORDS);
    let unit = ctx.kind_label.as_deref().unwrap_or("session");
    format!(
        "You are a memory summarizer for Cellar, a personal computer-use assistant.\n\
         Your job is to produce concise, high-recall summaries that the agent will later\n\
         search for the user's situation, decisions, and outcomes — not verbose paraphrases.\n\
         \n\
         Produce a {unit} summary covering:\n\
         - What the user was trying to do\n\
         - What the agent or rules actually did\n\
         - Any user corrections or surprises\n\
         - Outcome (success/failure/ongoing)\n\
         - Concrete nouns: file paths, app names, URLs, named entities\n\
         \n\
         Maximum {max_words} words. Use neutral past tense. Begin with the most important sentence."
    )
}

/// Render the chunks plus optional caller note as a single user
/// message. We tag each chunk with its kind + caller so the model can
/// distinguish chat from actions from fires without us pre-clustering.
fn build_user_prompt(chunks: &[MemoryChunk], ctx: &SummaryContext) -> String {
    let mut s = String::with_capacity(chunks.len() * 200);
    s.push_str("Memory chunks (oldest first):\n\n");
    for (i, c) in chunks.iter().enumerate() {
        let ts = c.created_at.to_rfc3339();
        s.push_str(&format!(
            "[{idx}] {ts} kind={kind:?} caller={caller}\n{content}\n\n",
            idx = i + 1,
            ts = ts,
            kind = c.kind,
            caller = c.caller_id,
            content = c.content.trim()
        ));
    }
    if let Some(note) = &ctx.note {
        s.push_str("Note: ");
        s.push_str(note);
        s.push('\n');
    }
    s
}

fn build_request(model: &str, chunks: &[MemoryChunk], ctx: &SummaryContext) -> CompletionRequest {
    let max_words = ctx.max_words.unwrap_or(DEFAULT_MAX_WORDS);
    // Budget output tokens around the requested word count — ~1.4
    // tokens/word for English, plus a small cushion so the model can
    // finish a sentence cleanly. We never go below 256 to avoid
    // pathological clipping.
    let max_tokens = ((max_words as f32 * 1.4).ceil() as u32 + 64).max(256);
    CompletionRequest::new(model)
        .with_system(build_system_prompt(ctx))
        .user(build_user_prompt(chunks, ctx))
        .with_max_tokens(max_tokens)
}

fn collect_text(content: Vec<ContentBlock>) -> String {
    let mut out = String::new();
    for b in content {
        if let ContentBlock::Text { text } = b {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text.trim());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_memory::{ChunkKind, ChunkSource, MemoryTier};
    use cellar_llm_router::provider::MockProvider;
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
    async fn anthropic_summarizer_returns_concatenated_text() {
        // Use the router's MockProvider so we don't hit the network. The
        // summarizer should forward the LLM's text back verbatim.
        let llm = MockProvider::with_text("the user closed the chat");
        let s = AnthropicSummarizer::new(llm, "claude-haiku-4-5");
        assert_eq!(s.name(), "anthropic:claude-haiku-4-5");
        let out = s
            .summarize(
                &[chunk("a", "hi"), chunk("b", "bye")],
                &SummaryContext {
                    kind_label: Some("session".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out, "the user closed the chat");
    }

    #[tokio::test]
    async fn ollama_summarizer_returns_concatenated_text() {
        let llm = MockProvider::with_text("local model summary");
        let s = OllamaSummarizer::new(llm, DEFAULT_OLLAMA_MODEL);
        assert!(s.name().starts_with("ollama:"));
        let out = s
            .summarize(&[chunk("a", "x")], &SummaryContext::default())
            .await
            .unwrap();
        assert_eq!(out, "local model summary");
    }

    #[tokio::test]
    async fn anthropic_summarizer_empty_chunks_errors_no_input() {
        let llm = MockProvider::with_text("should not be called");
        let s = AnthropicSummarizer::new(llm.clone(), "claude-haiku-4-5");
        let err = s
            .summarize(&[], &SummaryContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, SummarizerError::NoInput));
        // The LLM was never reached.
        assert_eq!(llm.requests().len(), 0);
    }

    #[tokio::test]
    async fn ollama_summarizer_empty_chunks_errors_no_input() {
        let llm = MockProvider::with_text("should not be called");
        let s = OllamaSummarizer::new(llm.clone(), DEFAULT_OLLAMA_MODEL);
        let err = s
            .summarize(&[], &SummaryContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, SummarizerError::NoInput));
        assert_eq!(llm.requests().len(), 0);
    }

    #[tokio::test]
    async fn anthropic_summarizer_assembles_expected_prompt() {
        // The LLM receives the §9.4 system prompt + a user message that
        // enumerates each chunk with kind/caller/timestamp.
        let llm = MockProvider::with_text("ok");
        let s = AnthropicSummarizer::new(llm.clone(), "claude-haiku-4-5");
        let _ = s
            .summarize(
                &[chunk("a", "alpha")],
                &SummaryContext {
                    kind_label: Some("day 2026-05-23".into()),
                    note: Some("first daily rollup".into()),
                    max_words: Some(120),
                },
            )
            .await
            .unwrap();
        let reqs = llm.requests();
        assert_eq!(reqs.len(), 1);
        let req = &reqs[0];
        // Model wired through.
        assert_eq!(req.model, "claude-haiku-4-5");
        // System prompt picked up the kind label and the word cap.
        let sys = req.system.as_deref().unwrap_or("");
        assert!(sys.contains("day 2026-05-23 summary"));
        assert!(sys.contains("Maximum 120 words"));
        // User message references the chunk content and the note.
        let user_block = req.messages.first().expect("user message present");
        match &user_block.content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("alpha"));
                assert!(text.contains("first daily rollup"));
            }
            _ => panic!("expected text content"),
        }
    }

    #[tokio::test]
    async fn build_default_falls_back_to_ollama_when_no_api_key() {
        // Snapshot + scrub the env so this test is hermetic. We don't
        // unset OLLAMA_BASE_URL because OllamaProvider::new ignores it
        // when None.
        let prior_provider = std::env::var(PROVIDER_ENV).ok();
        let prior_key = std::env::var(ANTHROPIC_API_KEY_ENV).ok();
        // SAFETY: tests in this crate are run on tokio's single-threaded
        // runtime by default, and this module's tests don't share env
        // state with concurrent ones.
        std::env::remove_var(PROVIDER_ENV);
        std::env::remove_var(ANTHROPIC_API_KEY_ENV);

        let summ = build_default().expect("ollama fallback should construct");
        assert!(summ.name().starts_with("ollama:"));

        // Restore.
        if let Some(v) = prior_provider {
            std::env::set_var(PROVIDER_ENV, v);
        }
        if let Some(v) = prior_key {
            std::env::set_var(ANTHROPIC_API_KEY_ENV, v);
        }
    }

    #[tokio::test]
    async fn build_default_picks_ollama_when_env_says_so() {
        let prior_provider = std::env::var(PROVIDER_ENV).ok();
        std::env::set_var(PROVIDER_ENV, "ollama");
        let summ = build_default().expect("ollama explicit should construct");
        assert!(summ.name().starts_with("ollama:"));
        match prior_provider {
            Some(v) => std::env::set_var(PROVIDER_ENV, v),
            None => std::env::remove_var(PROVIDER_ENV),
        }
    }
}
