#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error(
        "LLM provider not configured. Set CEL_LLM_PROVIDER + an API key env var \
         (e.g. GEMINI_API_KEY, ANTHROPIC_API_KEY, OPENAI_API_KEY), or run `cellar init` \
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
        matches!(
            self,
            LlmError::HttpError {
                status: 429 | 529,
                ..
            }
        )
    }

    /// True when the error is a transient transport / network failure that
    /// the LLM client should retry internally before bubbling up. Covers:
    ///
    /// * `RequestFailed` — `reqwest::Error::send()` failures (DNS errors,
    ///   connection refused, TCP reset, timeout). At runtime these are
    ///   nearly always transient — DNS hiccups, brief Anthropic
    ///   gateway flaps, the user's wifi blinking. Without retry they
    ///   currently kill the run on the first decide_next call (eval saw
    ///   `error sending request for url (https://api.anthropic.com/...)`
    ///   take down `browser_desktop_handoff` turn 2 even though every
    ///   other call in the run succeeded). One transient blip should
    ///   not be a single point of failure.
    /// * 5xx server errors EXCEPT 529 (which is rate-limit-style and
    ///   handled by `is_rate_limited`). 500/502/503/504 are transient
    ///   server-side issues; the request itself is well-formed and a
    ///   retry usually succeeds.
    ///
    /// `is_transient_network` deliberately excludes 4xx (auth/bad
    /// request — those won't fix themselves), `NotConfigured`,
    /// `ParseError`, and the rate-limit statuses (which have their
    /// own retry policy).
    pub fn is_transient_network(&self) -> bool {
        match self {
            LlmError::RequestFailed(_) => true,
            LlmError::HttpError { status, .. } => {
                // 5xx range, excluding the rate-limit-style 529 which
                // `is_rate_limited` already covers.
                *status >= 500 && *status < 600 && *status != 529
            }
            _ => false,
        }
    }
}
