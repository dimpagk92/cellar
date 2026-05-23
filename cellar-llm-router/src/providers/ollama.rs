//! Ollama adapter for local models.
//!
//! Uses Ollama's `/api/chat` endpoint. No auth. Default base URL is the
//! standard local port.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::error::{LlmError, Result};
use crate::provider::LlmProvider;
use crate::types::{CompletionRequest, CompletionResponse, ContentBlock, Role, StopReason, Usage};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Ollama provider for local models.
#[derive(Debug)]
pub struct OllamaProvider {
    client: Client,
    base_url: String,
}

impl OllamaProvider {
    /// Construct with an optional base URL override.
    pub fn new(base_url: Option<String>) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        })
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let body = build_body(&req);
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            let body_str = String::from_utf8_lossy(&bytes).into_owned();
            return Err(LlmError::Provider(format!("{}: {}", status, body_str)));
        }

        let wire: wire::ChatResponse = serde_json::from_slice(&bytes)?;
        Ok(parse_response(wire))
    }
}

fn build_body(req: &CompletionRequest) -> Value {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    if let Some(sys) = &req.system {
        messages.push(serde_json::json!({"role": "system", "content": sys}));
    }
    for m in &req.messages {
        let role = match m.role {
            Role::System => "system",
            Role::User | Role::Tool => "user",
            Role::Assistant => "assistant",
        };
        // Ollama's chat format is flat text content per message. Concatenate text blocks;
        // ignore tool use (v1: Ollama is for summarization/memory, not tool-using agent loops).
        let text: String = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        messages.push(serde_json::json!({"role": role, "content": text}));
    }

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": messages,
        "stream": false,
    });
    let mut options = serde_json::Map::new();
    if let Some(temp) = req.temperature {
        options.insert("temperature".into(), serde_json::json!(temp));
    }
    if let Some(max) = req.max_tokens {
        options.insert("num_predict".into(), serde_json::json!(max));
    }
    if !options.is_empty() {
        body["options"] = Value::Object(options);
    }
    if !req.stop_sequences.is_empty() {
        body["options"]["stop"] = serde_json::json!(req.stop_sequences);
    }
    body
}

fn parse_response(wire: wire::ChatResponse) -> CompletionResponse {
    let content = if wire.message.content.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::Text {
            text: wire.message.content,
        }]
    };
    CompletionResponse {
        content,
        stop_reason: if wire.done {
            StopReason::EndTurn
        } else {
            StopReason::Other
        },
        usage: Usage {
            input_tokens: wire.prompt_eval_count.unwrap_or(0),
            output_tokens: wire.eval_count.unwrap_or(0),
        },
        model: Some(wire.model),
    }
}

mod wire {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatResponse {
        pub model: String,
        #[allow(dead_code)]
        pub created_at: Option<String>,
        pub message: ChatMessage,
        pub done: bool,
        pub prompt_eval_count: Option<u32>,
        pub eval_count: Option<u32>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatMessage {
        #[allow(dead_code)]
        pub role: String,
        pub content: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_body_basic() {
        let req = CompletionRequest::new("llama3.1")
            .with_system("Be concise.")
            .user("hello");
        let body = build_body(&req);
        assert_eq!(body["model"], "llama3.1");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn parse_response_basic() {
        let raw = json!({
            "model": "llama3.1",
            "created_at": "2026-05-14T00:00:00Z",
            "message": {"role": "assistant", "content": "Hi from llama"},
            "done": true,
            "prompt_eval_count": 5,
            "eval_count": 4
        });
        let wire: wire::ChatResponse = serde_json::from_value(raw).unwrap();
        let resp = parse_response(wire);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hi from llama"),
            _ => panic!(),
        }
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 4);
    }
}
