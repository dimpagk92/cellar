#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error(
        "LLM provider not configured. Set CEL_LLM_PROVIDER + an API key env var \
         (e.g. GEMINI_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY), or run `dilipod init` \
         to pick a provider or install Gemma 4 locally via Ollama."
    )]
    NotConfigured,
    #[error("LLM API call failed: {0}")]
    RequestFailed(String),
    #[error("LLM returned HTTP {status}: {body}")]
    HttpError { status: u16, body: String },
    #[error("Failed to parse LLM response: {0}")]
    ParseError(String),
}

impl LlmError {
    /// True when the error means "stop retrying — the configuration is broken
    /// and no number of additional attempts will fix it." Today: 401 / 403
    /// auth failures and `NotConfigured`. Callers (planner, eval harness)
    /// should bail immediately rather than burning steps and tokens.
    pub fn is_unrecoverable(&self) -> bool {
        match self {
            LlmError::NotConfigured => true,
            LlmError::HttpError { status, .. } => matches!(status, 401 | 403),
            _ => false,
        }
    }

    /// True when the error is an exhausted rate-limit (HTTP 429) or provider
    /// overload (HTTP 529 — Anthropic-specific). The LLM client retries
    /// these internally with 1s/2s/4s backoff; if a `HttpError` with this
    /// status reaches the planner, it means the retries already happened.
    ///
    /// Runners use this to fail a goal fast after a small number of
    /// consecutive 429s instead of chewing through `max_consecutive_failures`
    /// × built-in-retry-latency. See `GoalRunner::max_consecutive_rate_limits`.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, LlmError::HttpError { status: 429 | 529, .. })
    }
}
