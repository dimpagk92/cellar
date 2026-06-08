//! Google Gemini provider — WS16.
//!
//! Gemini is reached through Google's official OpenAI-compatible endpoint
//! (`https://generativelanguage.googleapis.com/v1beta/openai`), so this is a
//! thin wrapper over [`OpenAiProvider`] pinned to that base URL. It reports its
//! own `name()` and delegates the wire protocol. Pass the key from
//! `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) and a `gemini-*` model on the request.
//!
//! (The native `generateContent` API would only be needed for Gemini-specific
//! features the OpenAI-compat shim doesn't expose; chat completions are
//! covered here.)

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::error::Result;
use crate::provider::LlmProvider;
use crate::providers::OpenAiProvider;
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse};

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";

/// Google Gemini — via the official OpenAI-compatible endpoint.
pub struct GeminiProvider {
    inner: OpenAiProvider,
}

impl GeminiProvider {
    /// Construct from an optional API key (the caller resolves `GEMINI_API_KEY`).
    pub fn new(api_key: Option<String>) -> Result<Self> {
        Ok(Self {
            inner: OpenAiProvider::new(api_key, Some(GEMINI_BASE_URL.to_string()))?,
        })
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        self.inner.complete(req).await
    }

    async fn stream<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'a, Result<CompletionChunk>>> {
        self.inner.stream(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_name() {
        let p = GeminiProvider::new(Some("test-key".into())).unwrap();
        assert_eq!(p.name(), "gemini");
    }
}
