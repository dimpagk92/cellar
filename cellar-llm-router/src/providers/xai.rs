//! xAI (Grok) provider — WS16.
//!
//! xAI's API is OpenAI-compatible (`https://api.x.ai/v1`), so this is a thin
//! wrapper over [`OpenAiProvider`] pinned to the xAI base URL. It reports its
//! own `name()` for logs/diagnostics and delegates the wire protocol. Pass the
//! key from `XAI_API_KEY` and a `grok-*` model on the request.

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::error::Result;
use crate::provider::LlmProvider;
use crate::providers::OpenAiProvider;
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse};

const XAI_BASE_URL: &str = "https://api.x.ai/v1";

/// xAI (Grok) — OpenAI-compatible endpoint.
pub struct XaiProvider {
    inner: OpenAiProvider,
}

impl XaiProvider {
    /// Construct from an optional API key (the caller resolves `XAI_API_KEY`).
    pub fn new(api_key: Option<String>) -> Result<Self> {
        Ok(Self {
            inner: OpenAiProvider::new(api_key, Some(XAI_BASE_URL.to_string()))?,
        })
    }
}

#[async_trait]
impl LlmProvider for XaiProvider {
    fn name(&self) -> &str {
        "xai"
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
        let p = XaiProvider::new(Some("test-key".into())).unwrap();
        assert_eq!(p.name(), "xai");
    }
}
