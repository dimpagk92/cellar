use crate::config::{LlmProviderConfig, ProviderKind};
use crate::error::LlmError;
use crate::ChatMessage;

/// Wire types for the OpenAI-compatible chat completions API.
#[derive(serde::Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    /// Standard max_tokens — used by most models. Skipped for reasoning models (o-series).
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// max_completion_tokens — required by OpenAI reasoning models (o4-mini, o3, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(serde::Serialize)]
struct ResponseFormat {
    r#type: String,
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(serde::Deserialize)]
struct ChoiceMessage {
    content: String,
}

/// Wire types for the Anthropic Messages API.
#[derive(serde::Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(serde::Serialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContent>,
}

#[derive(serde::Serialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
}

#[derive(serde::Serialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}

#[derive(serde::Deserialize)]
struct AnthropicResponse {
    content: Option<Vec<AnthropicResponseContent>>,
}

#[derive(serde::Deserialize)]
struct AnthropicResponseContent {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

type MockFn =
    std::sync::Arc<dyn Fn(Vec<ChatMessage>, u32) -> Result<String, LlmError> + Send + Sync>;

/// Reusable LLM client that speaks both the OpenAI-compatible chat completions
/// protocol and the Anthropic Messages API.
pub struct LlmClient {
    config: LlmProviderConfig,
    http: reqwest::Client,
    endpoint: String,
    model: String,
    /// When set, `chat()` calls this function instead of making an HTTP request.
    /// Used by tests to inject deterministic responses without a real LLM endpoint.
    mock_fn: Option<MockFn>,
}

impl LlmClient {
    /// Create a new client from config.
    pub fn new(config: LlmProviderConfig) -> Result<Self, LlmError> {
        let endpoint = config.resolved_endpoint().to_string();
        let model = config.resolved_model().to_string();

        if endpoint.is_empty() {
            return Err(LlmError::NotConfigured);
        }

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| LlmError::RequestFailed(e.to_string()))?;

        Ok(Self {
            config,
            http,
            endpoint,
            model,
            mock_fn: None,
        })
    }

    /// Create a mock client that returns responses from `f` instead of making HTTP calls.
    ///
    /// The closure receives `(messages, max_tokens)` and returns the same `Result<String, LlmError>`
    /// that a real client would return. Use this in tests to inject deterministic LLM responses.
    ///
    /// ```rust
    /// use cel_llm::LlmClient;
    ///
    /// let client = LlmClient::new_with_fn(|_msgs, _max_tokens| {
    ///     Ok(r#"{"action":{"type":"done"},"reasoning":"test","expected_outcome":"","confidence":1.0}"#.into())
    /// });
    /// ```
    pub fn new_with_fn<F>(f: F) -> Self
    where
        F: Fn(Vec<ChatMessage>, u32) -> Result<String, LlmError> + Send + Sync + 'static,
    {
        Self {
            config: LlmProviderConfig {
                provider: ProviderKind::Custom,
                endpoint: Some("mock://localhost".into()),
                api_key: None,
                model: Some("mock".into()),
                temperature: None,
                escalation_model: None,
            },
            http: reqwest::Client::new(),
            endpoint: "mock://localhost".into(),
            model: "mock".into(),
            mock_fn: Some(std::sync::Arc::new(f)),
        }
    }

    /// Provider name for logging.
    pub fn provider_name(&self) -> String {
        self.config.provider.to_string()
    }

    /// The resolved model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Whether this client targets the Anthropic Messages API.
    fn is_anthropic(&self) -> bool {
        self.config.provider == ProviderKind::Anthropic
    }

    /// Send a chat completion request with retry on rate limits.
    /// Retries up to 3 times with exponential backoff (1s, 2s, 4s) on HTTP 429.
    /// When a mock function is installed via [`LlmClient::new_with_fn`], it is called
    /// instead and no HTTP request is made.
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        if let Some(ref mock) = self.mock_fn {
            return mock(messages, max_tokens);
        }
        let mut last_err = LlmError::RequestFailed("no attempts made".into());
        for attempt in 0..3u32 {
            let msgs = messages.clone();
            let result = if self.is_anthropic() {
                self.chat_anthropic(msgs, max_tokens).await
            } else {
                self.chat_openai(msgs, max_tokens).await
            };
            match result {
                Ok(response) => return Ok(response),
                Err(LlmError::HttpError {
                    status: 429,
                    ref body,
                }) => {
                    tracing::warn!(
                        "Rate limited (429), retry {}/3 after {}s: {}",
                        attempt + 1,
                        1 << attempt,
                        &body[..body.len().min(100)]
                    );
                    last_err = result.unwrap_err();
                    tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                }
                Err(LlmError::HttpError { status: 529, .. }) => {
                    // Anthropic overloaded
                    tracing::warn!(
                        "Provider overloaded (529), retry {}/3 after {}s",
                        attempt + 1,
                        1 << attempt
                    );
                    last_err = result.unwrap_err();
                    tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
                }
                Err(e) => return Err(e), // Non-retryable error
            }
        }
        Err(last_err)
    }

    /// OpenAI-compatible chat completions path.
    /// Also used by Gemini (via its OpenAI-compatible endpoint).
    /// Both OpenAI and Gemini support `response_format: { type: "json_object" }`
    /// for structured JSON output, eliminating most parse failures.
    async fn chat_openai(
        &self,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        // Reasoning models (o-series) use max_completion_tokens instead of max_tokens,
        // and don't support response_format.
        let is_reasoning = self.model.starts_with("o1")
            || self.model.starts_with("o3")
            || self.model.starts_with("o4");

        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            max_tokens: if is_reasoning { None } else { Some(max_tokens) },
            max_completion_tokens: if is_reasoning { Some(max_tokens) } else { None },
            response_format: if is_reasoning {
                None // reasoning models don't support response_format
            } else {
                Some(ResponseFormat {
                    r#type: "json_object".to_string(),
                })
            },
            temperature: if is_reasoning {
                None
            } else {
                self.config.temperature
            },
        };

        let api_key = self.config.api_key.as_deref().unwrap_or("");

        let resp = self
            .http
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| LlmError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::HttpError { status, body });
        }

        let chat_resp: ChatResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::ParseError(e.to_string()))?;

        Ok(chat_resp
            .choices
            .and_then(|c| c.into_iter().next())
            .map(|c| c.message.content)
            .unwrap_or_default())
    }

    /// Anthropic Messages API path.
    async fn chat_anthropic(
        &self,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let api_key = self.config.api_key.as_deref().unwrap_or("");

        // Extract system message (Anthropic uses a top-level `system` field)
        let mut system_prompt: Option<String> = None;
        let mut user_messages = Vec::new();

        for msg in messages {
            if msg.role == "system" {
                // Concatenate system messages
                let text = msg
                    .content
                    .into_iter()
                    .filter_map(|c| match c {
                        crate::ContentPart::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                system_prompt = Some(match system_prompt {
                    Some(existing) => format!("{}\n{}", existing, text),
                    None => text,
                });
            } else {
                // Convert ContentParts to Anthropic format
                let content = msg
                    .content
                    .into_iter()
                    .map(|c| match c {
                        crate::ContentPart::Text { text } => AnthropicContent::Text { text },
                        crate::ContentPart::ImageUrl { image_url } => {
                            // Parse data URL: data:image/png;base64,<data>
                            let (media_type, data) = parse_data_url(&image_url.url);
                            AnthropicContent::Image {
                                source: AnthropicImageSource {
                                    source_type: "base64".to_string(),
                                    media_type,
                                    data,
                                },
                            }
                        }
                    })
                    .collect();

                user_messages.push(AnthropicMessage {
                    role: msg.role,
                    content,
                });
            }
        }

        // Prefill the assistant response with `{` to strongly encourage JSON output.
        // This is the standard Anthropic technique for structured output — the model
        // continues from the prefilled token, producing valid JSON without preamble.
        user_messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: vec![AnthropicContent::Text {
                text: "{".to_string(),
            }],
        });

        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            system: system_prompt,
            messages: user_messages,
            temperature: self.config.temperature,
        };

        // OAuth tokens (sk-ant-oat-…) need Authorization: Bearer + the
        // oauth-2025-04-20 beta header. Standard API keys (sk-ant-api-…
        // or sk-ant-…) use x-api-key. Detect by the OAuth-specific prefix.
        let mut req = self
            .http
            .post(&self.endpoint)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request);
        if api_key.starts_with("sk-ant-oat") {
            req = req
                .header("authorization", format!("Bearer {api_key}"))
                .header("anthropic-beta", "oauth-2025-04-20");
        } else {
            req = req.header("x-api-key", api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| LlmError::RequestFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::HttpError { status, body });
        }

        let anthropic_resp: AnthropicResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::ParseError(e.to_string()))?;

        // Prepend the `{` we used as the assistant prefill, since Anthropic
        // continues from that token and does not include it in the response.
        let body = anthropic_resp
            .content
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.content_type == "text")
            .filter_map(|c| c.text)
            .collect::<Vec<_>>()
            .join("");
        Ok(format!("{{{}", body))
    }

    /// Send a text-only chat completion (system + user prompt).
    pub async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let messages = vec![
            ChatMessage::text("system", system_prompt),
            ChatMessage::text("user", user_prompt),
        ];
        self.chat(messages, max_tokens).await
    }

    /// Create a clone of this client with a specific temperature override.
    pub fn with_temperature(&self, temperature: f64) -> Self {
        let mut config = self.config.clone();
        config.temperature = Some(temperature);
        Self {
            config,
            http: self.http.clone(),
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            mock_fn: self.mock_fn.clone(),
        }
    }

    /// Create a clone of this client with a specific model override.
    pub fn with_model_override(&self, model: &str) -> Self {
        Self {
            config: self.config.clone(),
            http: self.http.clone(),
            endpoint: self.endpoint.clone(),
            model: model.to_string(),
            mock_fn: self.mock_fn.clone(),
        }
    }

    /// Send a multi-turn chat completion with pre-built messages.
    /// Used by PlannerConversation for multi-step goal execution.
    pub async fn complete_with_messages(
        &self,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        self.chat(messages, max_tokens).await
    }

    /// Send a chat completion with an image (system prompt + image + user prompt).
    /// Optionally specify an OpenAI `detail` level for the image.
    pub async fn complete_with_image(
        &self,
        system_prompt: &str,
        image_data_url: &str,
        user_prompt: &str,
        max_tokens: u32,
        detail: Option<&str>,
    ) -> Result<String, LlmError> {
        let messages = vec![
            ChatMessage::text("system", system_prompt),
            ChatMessage::image(image_data_url, user_prompt, detail),
        ];
        self.chat(messages, max_tokens).await
    }
    /// Send a chat completion with multiple images.
    /// Each image is a base64 data URL. All images are in one user message with the text prompt.
    pub async fn complete_with_images(
        &self,
        system_prompt: &str,
        image_data_urls: &[&str],
        user_prompt: &str,
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        let mut content = Vec::with_capacity(image_data_urls.len() + 1);
        for url in image_data_urls {
            content.push(crate::ContentPart::ImageUrl {
                image_url: crate::ImageUrlPayload {
                    url: url.to_string(),
                    detail: None,
                },
            });
        }
        content.push(crate::ContentPart::Text {
            text: user_prompt.to_string(),
        });

        let messages = vec![
            ChatMessage::text("system", system_prompt),
            ChatMessage {
                role: "user".into(),
                content,
            },
        ];
        self.chat(messages, max_tokens).await
    }
}

/// Parse a data URL into (media_type, base64_data).
fn parse_data_url(url: &str) -> (String, String) {
    // Format: data:image/png;base64,<data>
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((header, data)) = rest.split_once(',') {
            let media_type = header.strip_suffix(";base64").unwrap_or(header).to_string();
            return (media_type, data.to_string());
        }
    }
    ("image/png".to_string(), url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let config = LlmProviderConfig {
            provider: ProviderKind::OpenAI,
            endpoint: None,
            api_key: Some("sk-test".into()),
            model: None,
            temperature: None,
            escalation_model: None,
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.model(), "gpt-4o");
        assert!(!client.is_anthropic());
    }

    #[test]
    fn test_client_anthropic() {
        let config = LlmProviderConfig {
            provider: ProviderKind::Anthropic,
            endpoint: None,
            api_key: Some("sk-ant-test".into()),
            model: None,
            temperature: None,
            escalation_model: None,
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.model(), "claude-sonnet-4-20250514");
        assert!(client.is_anthropic());
    }

    #[test]
    fn test_client_not_configured() {
        let config = LlmProviderConfig {
            provider: ProviderKind::Custom,
            endpoint: None,
            api_key: None,
            model: None,
            temperature: None,
            escalation_model: None,
        };
        assert!(LlmClient::new(config).is_err());
    }

    #[test]
    fn test_client_custom_endpoint() {
        let config = LlmProviderConfig {
            provider: ProviderKind::Custom,
            endpoint: Some("http://localhost:11434/v1/chat/completions".into()),
            api_key: None,
            model: Some("llama3".into()),
            temperature: None,
            escalation_model: None,
        };
        let client = LlmClient::new(config).unwrap();
        assert_eq!(client.model(), "llama3");
    }

    #[test]
    fn test_parse_data_url() {
        let (media, data) = parse_data_url("data:image/png;base64,abc123");
        assert_eq!(media, "image/png");
        assert_eq!(data, "abc123");
    }

    #[test]
    fn test_parse_data_url_jpeg() {
        let (media, data) = parse_data_url("data:image/jpeg;base64,xyz");
        assert_eq!(media, "image/jpeg");
        assert_eq!(data, "xyz");
    }

    #[test]
    fn test_parse_data_url_fallback() {
        let (media, data) = parse_data_url("raw_base64_data");
        assert_eq!(media, "image/png");
        assert_eq!(data, "raw_base64_data");
    }
}
