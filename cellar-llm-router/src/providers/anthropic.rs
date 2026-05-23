//! Anthropic Messages API adapter.
//!
//! Default provider for Cellar. Supports system prompts, tools, content
//! blocks, and streaming. This file holds:
//! - Wire types matching Anthropic's actual JSON shape (`wire` module).
//! - Translation functions to/from the crate's internal types.
//! - The `AnthropicProvider` struct implementing `LlmProvider`.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::error::{LlmError, Result};
use crate::provider::LlmProvider;
use crate::types::{
    CompletionRequest, CompletionResponse, ContentBlock, Role, StopReason, ToolDefinition, Usage,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Native Anthropic provider.
#[derive(Debug)]
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    /// Construct from an API key and optional base URL override.
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Result<Self> {
        let api_key = api_key.ok_or_else(|| {
            LlmError::MissingConfig(
                "AnthropicProvider requires an API key (set ANTHROPIC_API_KEY)".into(),
            )
        })?;
        Ok(Self {
            client: Client::new(),
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let body = build_body(&req);
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            let body_str = String::from_utf8_lossy(&bytes).into_owned();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(LlmError::Auth(body_str));
            }
            if status.as_u16() == 429 {
                return Err(LlmError::RateLimited { retry_after_s: 30 });
            }
            return Err(LlmError::Provider(format!("{}: {}", status, body_str)));
        }

        let wire: wire::MessageResponse = serde_json::from_slice(&bytes)?;
        Ok(parse_response(wire))
    }
}

fn build_body(req: &CompletionRequest) -> Value {
    let messages: Vec<wire::Message> = req
        .messages
        .iter()
        .map(|m| wire::Message {
            role: match m.role {
                Role::User | Role::Tool => "user".into(),
                Role::Assistant => "assistant".into(),
                Role::System => "user".into(), // Anthropic doesn't have system role in messages
            },
            content: m.content.iter().map(content_block_to_wire).collect(),
        })
        .collect();

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": req.max_tokens.unwrap_or(4096),
    });

    if let Some(sys) = &req.system {
        body["system"] = Value::String(sys.clone());
    }
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if !req.stop_sequences.is_empty() {
        body["stop_sequences"] = serde_json::json!(req.stop_sequences);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req.tools.iter().map(tool_to_wire).collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

fn content_block_to_wire(b: &ContentBlock) -> Value {
    match b {
        ContentBlock::Text { text } => serde_json::json!({
            "type": "text",
            "text": text,
        }),
        ContentBlock::ToolUse { id, name, input } => serde_json::json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => serde_json::json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
    }
}

fn tool_to_wire(t: &ToolDefinition) -> Value {
    serde_json::json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.input_schema,
    })
}

fn parse_response(wire: wire::MessageResponse) -> CompletionResponse {
    let content = wire
        .content
        .into_iter()
        .map(content_block_from_wire)
        .collect();
    CompletionResponse {
        content,
        stop_reason: stop_reason_from_wire(wire.stop_reason.as_deref()),
        usage: Usage {
            input_tokens: wire.usage.input_tokens,
            output_tokens: wire.usage.output_tokens,
        },
        model: Some(wire.model),
    }
}

fn content_block_from_wire(b: wire::ContentBlock) -> ContentBlock {
    match b.block_type.as_str() {
        "text" => ContentBlock::Text {
            text: b.text.unwrap_or_default(),
        },
        "tool_use" => ContentBlock::ToolUse {
            id: b.id.unwrap_or_default(),
            name: b.name.unwrap_or_default(),
            input: b.input.unwrap_or(Value::Null),
        },
        _ => ContentBlock::Text {
            text: format!("[unknown block: {}]", b.block_type),
        },
    }
}

fn stop_reason_from_wire(s: Option<&str>) -> StopReason {
    match s {
        Some("end_turn") => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("tool_use") => StopReason::ToolUse,
        _ => StopReason::Other,
    }
}

/// Wire types matching Anthropic's response JSON exactly.
mod wire {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    #[derive(Debug, Serialize)]
    pub(super) struct Message {
        pub role: String,
        pub content: Vec<Value>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct MessageResponse {
        #[allow(dead_code)]
        pub id: String,
        #[allow(dead_code)]
        #[serde(rename = "type")]
        pub message_type: String,
        #[allow(dead_code)]
        pub role: String,
        pub model: String,
        pub content: Vec<ContentBlock>,
        pub stop_reason: Option<String>,
        pub usage: Usage,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct ContentBlock {
        #[serde(rename = "type")]
        pub block_type: String,
        pub text: Option<String>,
        pub id: Option<String>,
        pub name: Option<String>,
        pub input: Option<Value>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Usage {
        pub input_tokens: u32,
        pub output_tokens: u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_body_basic() {
        let req = CompletionRequest::new("claude-opus-4-7")
            .with_system("You are helpful.")
            .user("Hello")
            .with_max_tokens(1024);
        let body = build_body(&req);
        assert_eq!(body["model"], "claude-opus-4-7");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn build_body_with_tools() {
        let req = CompletionRequest::new("claude-opus-4-7").with_tools(vec![ToolDefinition {
            name: "cel_act".into(),
            description: "Take an action on the device".into(),
            input_schema: json!({"type": "object", "properties": {"verb": {"type": "string"}}}),
        }]);
        let body = build_body(&req);
        assert_eq!(body["tools"][0]["name"], "cel_act");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn parse_response_text_only() {
        let raw = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-7",
            "content": [{"type": "text", "text": "Hi there"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let wire: wire::MessageResponse = serde_json::from_value(raw).unwrap();
        let resp = parse_response(wire);
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hi there"),
            _ => panic!(),
        }
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
    }

    #[test]
    fn parse_response_with_tool_use() {
        let raw = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-7",
            "content": [
                {"type": "text", "text": "I'll click for you."},
                {"type": "tool_use", "id": "tc_1", "name": "cel_act", "input": {"verb": "click"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 8}
        });
        let wire: wire::MessageResponse = serde_json::from_value(raw).unwrap();
        let resp = parse_response(wire);
        assert_eq!(resp.content.len(), 2);
        match &resp.content[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tc_1");
                assert_eq!(name, "cel_act");
                assert_eq!(input["verb"], "click");
            }
            _ => panic!(),
        }
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn missing_api_key_errors() {
        let err = AnthropicProvider::new(None, None).unwrap_err();
        assert!(matches!(err, LlmError::MissingConfig(_)));
    }
}
