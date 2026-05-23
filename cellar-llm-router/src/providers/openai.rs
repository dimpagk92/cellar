//! OpenAI-compatible adapter.
//!
//! Used for OpenAI itself and for every OpenAI-compatible endpoint:
//! OpenRouter, LiteLLM, vLLM, LM Studio, Together, Groq, Mistral, etc.
//! The base URL is configurable.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::error::{LlmError, Result};
use crate::provider::LlmProvider;
use crate::types::{CompletionRequest, CompletionResponse, ContentBlock, Role, StopReason, Usage};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI-compatible provider.
#[derive(Debug)]
pub struct OpenAiProvider {
    client: Client,
    api_key: Option<String>,
    base_url: String,
}

impl OpenAiProvider {
    /// Construct from an API key and base URL.
    ///
    /// The API key is optional because some compatible endpoints
    /// (self-hosted vLLM / LM Studio) don't require auth.
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let body = build_body(&req);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut builder = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let response = builder.send().await?;
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

        let wire: wire::ChatCompletionResponse = serde_json::from_slice(&bytes)?;
        parse_response(wire)
    }
}

fn build_body(req: &CompletionRequest) -> Value {
    // OpenAI doesn't have a separate system field — it's a message with role=system.
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    if let Some(sys) = &req.system {
        messages.push(serde_json::json!({"role": "system", "content": sys}));
    }
    for m in &req.messages {
        messages.push(message_to_wire(m));
    }

    let mut body = serde_json::json!({
        "model": req.model,
        "messages": messages,
    });
    if let Some(temp) = req.temperature {
        body["temperature"] = serde_json::json!(temp);
    }
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = serde_json::json!(max);
    }
    if !req.stop_sequences.is_empty() {
        body["stop"] = serde_json::json!(req.stop_sequences);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

fn message_to_wire(m: &crate::types::Message) -> Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    // OpenAI: a single message is either text content + tool_calls, or tool result.
    // Collect text into one string; collect ToolUse into tool_calls; ToolResult
    // converts the whole message to role=tool.
    let mut text_parts = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_result: Option<Value> = None;

    for block in &m.content {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                    }
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                tool_result = Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content.as_str().map(String::from).unwrap_or_else(|| content.to_string()),
                }));
            }
        }
    }

    if let Some(t) = tool_result {
        return t;
    }

    let mut out = serde_json::json!({
        "role": role,
        "content": text_parts.join(""),
    });
    if !tool_calls.is_empty() {
        out["tool_calls"] = Value::Array(tool_calls);
    }
    out
}

fn parse_response(wire: wire::ChatCompletionResponse) -> Result<CompletionResponse> {
    let choice = wire
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::Provider("no choices in response".into()))?;

    let mut content = Vec::new();
    if let Some(text) = choice.message.content {
        if !text.is_empty() {
            content.push(ContentBlock::Text { text });
        }
    }
    if let Some(tcs) = choice.message.tool_calls {
        for tc in tcs {
            let input: Value = match &tc.function.arguments {
                Some(s) if !s.is_empty() => serde_json::from_str(s).unwrap_or(Value::Null),
                _ => Value::Null,
            };
            content.push(ContentBlock::ToolUse {
                id: tc.id,
                name: tc.function.name,
                input,
            });
        }
    }

    Ok(CompletionResponse {
        content,
        stop_reason: stop_reason_from_finish(choice.finish_reason.as_deref()),
        usage: Usage {
            input_tokens: wire.usage.as_ref().map_or(0, |u| u.prompt_tokens),
            output_tokens: wire.usage.as_ref().map_or(0, |u| u.completion_tokens),
        },
        model: Some(wire.model),
    })
}

fn stop_reason_from_finish(s: Option<&str>) -> StopReason {
    match s {
        Some("stop") => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("content_filter") => StopReason::Other,
        _ => StopReason::Other,
    }
}

mod wire {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatCompletionResponse {
        #[allow(dead_code)]
        pub id: String,
        pub model: String,
        pub choices: Vec<Choice>,
        pub usage: Option<Usage>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Choice {
        #[allow(dead_code)]
        pub index: u32,
        pub message: WireMessage,
        pub finish_reason: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct WireMessage {
        #[allow(dead_code)]
        pub role: String,
        pub content: Option<String>,
        pub tool_calls: Option<Vec<WireToolCall>>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct WireToolCall {
        pub id: String,
        #[serde(rename = "type")]
        #[allow(dead_code)]
        pub call_type: String,
        pub function: WireFunction,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct WireFunction {
        pub name: String,
        pub arguments: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Usage {
        pub prompt_tokens: u32,
        pub completion_tokens: u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDefinition;
    use serde_json::json;

    #[test]
    fn build_body_with_system_as_message() {
        let req = CompletionRequest::new("gpt-4o-mini")
            .with_system("You are helpful.")
            .user("Hello")
            .with_max_tokens(1024);
        let body = build_body(&req);
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You are helpful.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "Hello");
    }

    #[test]
    fn build_body_tools_as_functions() {
        let req = CompletionRequest::new("gpt-4o-mini").with_tools(vec![ToolDefinition {
            name: "cel_act".into(),
            description: "Take an action".into(),
            input_schema: json!({"type": "object"}),
        }]);
        let body = build_body(&req);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "cel_act");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn parse_response_text_only() {
        let raw = json!({
            "id": "cmp_1",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hi"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        });
        let wire: wire::ChatCompletionResponse = serde_json::from_value(raw).unwrap();
        let resp = parse_response(wire).unwrap();
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hi"),
            _ => panic!(),
        }
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn parse_response_with_tool_calls() {
        let raw = json!({
            "id": "cmp_1",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "tc_1",
                        "type": "function",
                        "function": {"name": "cel_act", "arguments": "{\"verb\":\"click\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 4}
        });
        let wire: wire::ChatCompletionResponse = serde_json::from_value(raw).unwrap();
        let resp = parse_response(wire).unwrap();
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tc_1");
                assert_eq!(name, "cel_act");
                assert_eq!(input["verb"], "click");
            }
            _ => panic!(),
        }
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }
}
