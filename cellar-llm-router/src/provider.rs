//! The `LlmProvider` trait and a `MockProvider` for tests.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::types::{
    CompletionChunk, CompletionRequest, CompletionResponse, ContentBlock, StopReason, Usage,
};

/// Provider abstraction over any LLM backend.
///
/// Object-safe: use as `Arc<dyn LlmProvider>` for dynamic dispatch.
/// All implementations must be `Send + Sync` because the daemon shares them
/// across tokio tasks.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Identifier of the provider kind (e.g., `"anthropic"`, `"openai"`).
    /// Used for logs and diagnostics.
    fn name(&self) -> &str;

    /// Run a completion synchronously (no streaming).
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;

    /// Run a completion as a stream.
    ///
    /// The default implementation calls `complete()` and yields the result as
    /// a single `MessageStop` event preceded by reconstructed
    /// `ContentBlockStart`/`Stop` frames. Providers should override for real
    /// token-by-token streaming when supported.
    async fn stream<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'a, Result<CompletionChunk>>> {
        let resp = self.complete(req).await?;
        let mut chunks: Vec<Result<CompletionChunk>> = Vec::new();
        for (i, block) in resp.content.into_iter().enumerate() {
            let idx = i as u32;
            chunks.push(Ok(CompletionChunk::ContentBlockStart {
                index: idx,
                block: block.clone(),
            }));
            if let ContentBlock::Text { text } = &block {
                chunks.push(Ok(CompletionChunk::TextDelta {
                    index: idx,
                    text: text.clone(),
                }));
            }
            chunks.push(Ok(CompletionChunk::ContentBlockStop { index: idx }));
        }
        chunks.push(Ok(CompletionChunk::MessageStop {
            stop_reason: resp.stop_reason,
            usage: Some(resp.usage),
        }));
        Ok(Box::pin(stream::iter(chunks)))
    }
}

/// Test double. Returns a fixed response (or sequence of responses) on each call.
/// Records the requests it received for assertion.
pub struct MockProvider {
    name: String,
    responses: Mutex<Vec<CompletionResponse>>,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl MockProvider {
    /// Construct a mock that returns the given responses in order.
    /// When the queue runs out, subsequent calls reuse the last response.
    pub fn new(responses: Vec<CompletionResponse>) -> Arc<Self> {
        Arc::new(Self {
            name: "mock".into(),
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Construct a mock that returns a single plain-text response.
    pub fn with_text(text: impl Into<String>) -> Arc<Self> {
        Self::new(vec![CompletionResponse {
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: None,
        }])
    }

    /// Snapshot the requests this provider has seen.
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        self.requests.lock().unwrap().push(req);
        let mut responses = self.responses.lock().unwrap();
        let resp = if responses.len() > 1 {
            responses.remove(0)
        } else {
            responses.first().cloned().unwrap_or(CompletionResponse {
                content: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: None,
            })
        };
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CompletionRequest;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn mock_returns_response() {
        let p = MockProvider::with_text("hello");
        let req = CompletionRequest::new("test").user("hi");
        let resp = p.complete(req).await.unwrap();
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text block"),
        }
    }

    #[tokio::test]
    async fn mock_records_requests() {
        let p = MockProvider::with_text("ok");
        let req = CompletionRequest::new("test").user("question");
        let _ = p.complete(req.clone()).await.unwrap();
        let recorded = p.requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0], req);
    }

    #[tokio::test]
    async fn default_stream_impl_yields_text() {
        let p = MockProvider::with_text("streamed");
        let req = CompletionRequest::new("test").user("hi");
        let mut stream = p.stream(req).await.unwrap();
        let mut got_text = String::new();
        let mut got_stop = false;
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                CompletionChunk::TextDelta { text, .. } => got_text.push_str(&text),
                CompletionChunk::MessageStop { .. } => got_stop = true,
                _ => {}
            }
        }
        assert_eq!(got_text, "streamed");
        assert!(got_stop);
    }
}
