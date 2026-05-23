//! Error types for the LLM router.

use thiserror::Error;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, LlmError>;

/// The errors that LLM operations can produce.
#[derive(Debug, Error)]
pub enum LlmError {
    /// Provider-side error (4xx/5xx response, malformed body, etc.).
    #[error("provider error: {0}")]
    Provider(String),

    /// HTTP transport error.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization / deserialization error.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Required config (env var, etc.) missing.
    #[error("missing config: {0}")]
    MissingConfig(String),

    /// Unknown provider kind in config.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    /// Subsystem not registered in router.
    #[error("unknown subsystem: {0}")]
    UnknownSubsystem(String),

    /// I/O error during streaming.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Server told us to back off.
    #[error("rate limited (retry after {retry_after_s}s)")]
    RateLimited {
        /// Retry-After hint from the server, in seconds.
        retry_after_s: u64,
    },

    /// Auth failed (bad API key, expired token, etc.).
    #[error("auth error: {0}")]
    Auth(String),

    /// Generic invariant violation.
    #[error("invariant: {0}")]
    Invariant(String),
}
