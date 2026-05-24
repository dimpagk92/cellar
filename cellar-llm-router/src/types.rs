//! Wire types for the LLM router.
//!
//! The internal model follows Anthropic's content-block shape — messages
//! contain a list of typed blocks (text, tool_use, tool_result). The OpenAI
//! adapter translates flat `tool_calls` to and from this shape.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request for a completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletionRequest {
    /// Model identifier (provider-specific, e.g. `claude-opus-4-7`, `gpt-4o-mini`).
    pub model: String,
    /// Conversation messages in order.
    pub messages: Vec<Message>,
    /// System prompt (separate from messages in the Anthropic model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Tool definitions the model can call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    /// Sampling temperature (provider-specific range, typically 0.0..=1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Role of the speaker.
    pub role: Role,
    /// Message content as a list of content blocks.
    pub content: Vec<ContentBlock>,
}

/// Speaker role.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System prompt role (used by OpenAI; Anthropic uses `system` field instead).
    System,
    /// End-user message.
    User,
    /// Model response.
    Assistant,
    /// Tool result fed back into the conversation.
    Tool,
}

/// A typed content block within a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text content.
        text: String,
    },
    /// Model is invoking a tool.
    ToolUse {
        /// Tool-call identifier (echoed by the model in the result).
        id: String,
        /// Tool name.
        name: String,
        /// Tool input (JSON object).
        input: Value,
    },
    /// Result of a previously-invoked tool.
    ToolResult {
        /// Identifier echoing the originating `ToolUse.id`.
        tool_use_id: String,
        /// Result content (string or structured).
        content: Value,
        /// True if the result represents an error.
        #[serde(default)]
        is_error: bool,
    },
}

/// A tool the model may call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Tool name (must match what the model will emit in `ToolUse.name`).
    pub name: String,
    /// Human-readable description shown to the model.
    pub description: String,
    /// JSON Schema for the tool's input.
    pub input_schema: Value,
}

/// A completion response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletionResponse {
    /// Output content blocks.
    pub content: Vec<ContentBlock>,
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// Token usage.
    pub usage: Usage,
    /// Provider-specific model id echoed back (often differs from request for routing reasons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Natural end of turn.
    EndTurn,
    /// Hit the max-tokens limit.
    MaxTokens,
    /// Matched a stop sequence.
    StopSequence,
    /// Stopped to wait for a tool result.
    ToolUse,
    /// Provider returned an unmapped reason.
    Other,
}

/// Token usage for a completion.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    /// Tokens in the prompt.
    pub input_tokens: u32,
    /// Tokens generated.
    pub output_tokens: u32,
}

/// A streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionChunk {
    /// A new content block started.
    ContentBlockStart {
        /// Index of the block within `content`.
        index: u32,
        /// Block type — first appearance of this block.
        block: ContentBlock,
    },
    /// Incremental text appended to a text block.
    TextDelta {
        /// Block index.
        index: u32,
        /// Text to append.
        text: String,
    },
    /// Incremental JSON appended to a tool_use block's `input`.
    ToolUseInputDelta {
        /// Block index.
        index: u32,
        /// Partial JSON string (concatenated by the consumer).
        partial_json: String,
    },
    /// Content block finished.
    ContentBlockStop {
        /// Block index.
        index: u32,
    },
    /// Message finished (terminal).
    MessageStop {
        /// Why the model stopped.
        stop_reason: StopReason,
        /// Final usage (may be absent for some providers).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    /// Streaming-level error.
    Error {
        /// Error message.
        message: String,
    },
}

impl CompletionRequest {
    /// Convenience constructor with sensible defaults.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            system: None,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            stop_sequences: Vec::new(),
        }
    }

    /// Builder: add a user message of plain text.
    pub fn user(mut self, text: impl Into<String>) -> Self {
        self.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        });
        self
    }

    /// Builder: add an assistant message of plain text.
    pub fn assistant(mut self, text: impl Into<String>) -> Self {
        self.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        });
        self
    }

    /// Builder: set system prompt.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Builder: add tools.
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    /// Builder: set max tokens.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_request() {
        let req = CompletionRequest::new("claude-opus-4-7")
            .with_system("You are helpful.")
            .user("Hello")
            .assistant("Hi!")
            .with_max_tokens(1024);
        let s = serde_json::to_string(&req).unwrap();
        let back: CompletionRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn content_block_tag() {
        let b = ContentBlock::Text {
            text: "hello".into(),
        };
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains("\"type\":\"text\""));
    }

    #[test]
    fn tool_use_round_trip() {
        let b = ContentBlock::ToolUse {
            id: "tc_1".into(),
            name: "cel_act".into(),
            input: json!({"verb": "click", "target": "button"}),
        };
        let s = serde_json::to_string(&b).unwrap();
        let back: ContentBlock = serde_json::from_str(&s).unwrap();
        assert_eq!(b, back);
    }
}
