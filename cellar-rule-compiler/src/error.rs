//! Compiler error types.

use thiserror::Error;

/// Errors returned by [`crate::Compiler::compile`].
#[derive(Debug, Error)]
pub enum CompileError {
    /// Underlying LLM provider failed.
    #[error("llm provider: {0}")]
    Provider(#[from] cellar_llm_router::LlmError),

    /// LLM response didn't contain a JSON object.
    #[error("no JSON found in LLM response")]
    NoJsonInResponse,

    /// JSON parse failed.
    #[error("JSON parse: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// JSON parsed but didn't deserialize into a `Rule` even after one retry.
    #[error("validation: {0}")]
    Validation(String),

    /// Empty NL string.
    #[error("nl string is empty")]
    EmptyInput,
}
