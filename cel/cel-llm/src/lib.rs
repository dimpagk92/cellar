//! CEL LLM Provider Layer
//!
//! Unified multi-provider LLM client for the Context Execution Layer.
//! Supports OpenAI, Anthropic, Google Gemini, and any OpenAI-compatible endpoint.

mod client;
mod config;
mod error;

pub use client::LlmClient;
pub use config::{LlmProviderConfig, LlmRole, ModelProfile, ModelTier, ProviderKind};
pub use error::LlmError;

/// Content part in a chat message (text or image).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlPayload },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImageUrlPayload {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A single chat message.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Vec<ContentPart>,
}

impl ChatMessage {
    /// Create a text-only message.
    pub fn text(role: &str, text: &str) -> Self {
        Self {
            role: role.into(),
            content: vec![ContentPart::Text { text: text.into() }],
        }
    }

    /// Create a user message with an image (base64 data URL) and text prompt.
    /// Optionally specify an OpenAI `detail` level (`"low"`, `"high"`, or `"auto"`).
    pub fn image(data_url: &str, text: &str, detail: Option<&str>) -> Self {
        Self {
            role: "user".into(),
            content: vec![
                ContentPart::ImageUrl {
                    image_url: ImageUrlPayload {
                        url: data_url.into(),
                        detail: detail.map(|d| d.to_string()),
                    },
                },
                ContentPart::Text { text: text.into() },
            ],
        }
    }
}

/// Encode raw bytes as base64.
pub fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Rough token estimate for text (1 token ≈ 4 chars).
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4 + 1
}

/// Estimate vision tokens based on image dimensions and detail level.
/// OpenAI: low=85 tokens, high=170*tiles+85 where tiles=ceil(w/512)*ceil(h/512)
pub fn estimate_image_tokens(width: u32, height: u32, detail: &str) -> usize {
    match detail {
        "low" => 85,
        _ => {
            let tiles_w = (width as f64 / 512.0).ceil() as usize;
            let tiles_h = (height as f64 / 512.0).ceil() as usize;
            170 * tiles_w * tiles_h + 85
        }
    }
}

/// Create an [`LlmClient`] from environment variables, falling back to `~/.cellar/config.toml`.
///
/// Resolution order:
/// 1. Env vars (`CEL_LLM_PROVIDER`, `CEL_LLM_API_KEY`, `CEL_LLM_MODEL`, `CEL_LLM_ENDPOINT`,
///    and provider-specific keys like `GEMINI_API_KEY`).
/// 2. The `[llm]` section of `~/.cellar/config.toml`, written by `dilipod init`.
///
/// Returns `LlmError::NotConfigured` (with instructions) if neither source yields a provider.
pub fn create_client() -> Result<LlmClient, LlmError> {
    let config = LlmProviderConfig::from_env()
        .or_else(LlmProviderConfig::from_config_file)
        .ok_or(LlmError::NotConfigured)?;
    LlmClient::new(config)
}

/// Strip markdown code fences from an LLM response and return the inner content.
/// Also handles the case where the LLM returns prose text followed by a JSON object —
/// extracts the first `{...}` block with balanced braces.
pub fn strip_code_fences(content: &str) -> &str {
    let s = content.trim();

    // Case 1: code fences
    if let Some(inner) = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
    {
        return inner
            .strip_suffix("```")
            .unwrap_or(inner)
            .trim();
    }

    // Case 2: already valid JSON start
    if s.starts_with('{') || s.starts_with('[') {
        return s.strip_suffix("```").unwrap_or(s).trim();
    }

    // Case 3: prose followed by JSON — extract first balanced `{...}`
    if let Some(start) = s.find('{') {
        let bytes = s.as_bytes();
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut escape = false;
        for i in start..bytes.len() {
            if escape {
                escape = false;
                continue;
            }
            let ch = bytes[i];
            if ch == b'\\' {
                escape = true;
                continue;
            }
            if ch == b'"' {
                in_string = !in_string;
                continue;
            }
            if in_string {
                continue;
            }
            if ch == b'{' {
                depth += 1;
            }
            if ch == b'}' {
                depth -= 1;
                if depth == 0 {
                    return &s[start..=i];
                }
            }
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b"Hi"), "SGk=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_strip_code_fences_json() {
        let input = "```json\n[{\"key\": \"value\"}]\n```";
        assert_eq!(strip_code_fences(input), "[{\"key\": \"value\"}]");
    }

    #[test]
    fn test_strip_code_fences_plain() {
        let input = "```\nhello\n```";
        assert_eq!(strip_code_fences(input), "hello");
    }

    #[test]
    fn test_strip_code_fences_none() {
        let input = "[1, 2, 3]";
        assert_eq!(strip_code_fences(input), "[1, 2, 3]");
    }

    #[test]
    fn test_strip_code_fences_prose_then_json() {
        let input = r#"Looking at the screen, I can see the homepage. Let me click the search button.

{"reasoning": "Click search", "action": {"type": "click", "target_id": "dom:btn:1"}, "expected_outcome": "Opens search", "confidence": 0.9}"#;
        assert_eq!(
            strip_code_fences(input),
            r#"{"reasoning": "Click search", "action": {"type": "click", "target_id": "dom:btn:1"}, "expected_outcome": "Opens search", "confidence": 0.9}"#
        );
    }

    #[test]
    fn test_chat_message_text() {
        let msg = ChatMessage::text("system", "You are helpful.");
        assert_eq!(msg.role, "system");
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_chat_message_image() {
        let msg = ChatMessage::image("data:image/png;base64,abc", "Describe this.", None);
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content.len(), 2);
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("hello world!"), 4); // 12 chars / 4 + 1
        assert_eq!(estimate_tokens(""), 1);
    }

    #[test]
    fn test_estimate_image_tokens_low() {
        assert_eq!(estimate_image_tokens(1920, 1080, "low"), 85);
    }

    #[test]
    fn test_estimate_image_tokens_high() {
        // 1920/512 = ceil(3.75) = 4, 1080/512 = ceil(2.109) = 3 => 170*12+85 = 2125
        assert_eq!(estimate_image_tokens(1920, 1080, "high"), 2125);
    }
}
