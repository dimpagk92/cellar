//! CEL Vision Layer
//!
//! Multi-provider vision model integration. Supports Gemini, GPT-4o, Claude,
//! HuggingFace (local), and any OpenAI-compatible endpoint.
//!
//! Vision is only invoked when the accessibility tree and native APIs
//! cannot provide sufficient context.
//!
//! Uses [`cel_llm::LlmClient`] for all API communication.

mod openai_compat;
mod provider;

pub use cel_llm::{LlmProviderConfig, ProviderKind};
pub use openai_compat::OpenAICompatProvider;
pub use provider::{VisionBounds, VisionElement, VisionError, VisionProvider};

/// Create a vision provider from configuration.
pub fn create_provider(
    config: LlmProviderConfig,
) -> Result<Box<dyn VisionProvider>, VisionError> {
    // All known providers use the OpenAI-compatible chat completions protocol.
    Ok(Box::new(OpenAICompatProvider::new(config)?))
}

/// Create a vision provider from environment variables.
///
/// Reads `CEL_LLM_PROVIDER`, `CEL_LLM_API_KEY`, etc. See
/// [`LlmProviderConfig::from_env`] for full documentation.
pub fn create_provider_from_env() -> Result<Box<dyn VisionProvider>, VisionError> {
    let config = LlmProviderConfig::from_env()
        .ok_or(VisionError::NotConfigured)?;
    create_provider(config)
}
