use napi_derive::napi;

pub(crate) fn build_llm_client(
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    role: Option<cel_llm::LlmRole>,
) -> napi::Result<cel_llm::LlmClient> {
    let config = match provider {
        Some(p) => cel_llm::LlmProviderConfig {
            provider: cel_llm::ProviderKind::from(p.as_str()),
            endpoint,
            api_key,
            model,
            temperature: None,
            escalation_model: None,
        },
        None => cel_llm::LlmProviderConfig::from_env_with_role(
            role.unwrap_or(cel_llm::LlmRole::General),
        )
        .ok_or_else(|| {
            napi::Error::from_reason(
                "LLM not configured: set CEL_LLM_PROVIDER env var or pass provider param"
                    .to_string(),
            )
        })?,
    };
    cel_llm::LlmClient::new(config).map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Send a text-only LLM chat completion. Returns the model response string.
///
/// If `provider` is omitted, reads config from env vars (CEL_LLM_PROVIDER, etc.).
#[napi]
pub async fn llm_complete(
    system_prompt: String,
    user_prompt: String,
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    max_tokens: Option<u32>,
) -> napi::Result<String> {
    let client = build_llm_client(provider, api_key, model, endpoint, Some(cel_llm::LlmRole::General))?;
    client
        .complete(&system_prompt, &user_prompt, max_tokens.unwrap_or(4096))
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Send a text-only LLM chat completion using a specific role for provider resolution.
///
/// The role determines which env vars are used (e.g., CEL_LLM_VALIDATOR_PROVIDER).
/// Valid roles: "planner", "observer", "vision", "general", "validator", "localizer", "orchestrator".
#[napi]
pub async fn llm_complete_with_role(
    system_prompt: String,
    user_prompt: String,
    role: String,
    max_tokens: Option<u32>,
) -> napi::Result<String> {
    let llm_role = match role.to_lowercase().as_str() {
        "planner" => cel_llm::LlmRole::Planner,
        "observer" => cel_llm::LlmRole::Observer,
        "vision" => cel_llm::LlmRole::Vision,
        "validator" => cel_llm::LlmRole::Validator,
        "localizer" => cel_llm::LlmRole::Localizer,
        "orchestrator" => cel_llm::LlmRole::Orchestrator,
        _ => cel_llm::LlmRole::General,
    };
    let client = build_llm_client(None, None, None, None, Some(llm_role))?;
    client
        .complete(&system_prompt, &user_prompt, max_tokens.unwrap_or(4096))
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Send an LLM chat completion with an attached image. Returns the model response string.
///
/// If `provider` is omitted, reads config from env vars.
/// Uses the Vision role for provider resolution.
#[napi]
pub async fn llm_complete_with_image(
    system_prompt: String,
    image_base64: String,
    user_prompt: String,
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    endpoint: Option<String>,
    max_tokens: Option<u32>,
) -> napi::Result<String> {
    let client = build_llm_client(provider, api_key, model, endpoint, Some(cel_llm::LlmRole::Vision))?;
    let data_url = format!("data:image/png;base64,{}", image_base64);
    client
        .complete_with_image(
            &system_prompt,
            &data_url,
            &user_prompt,
            max_tokens.unwrap_or(4096),
            None,
        )
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

/// Send a multi-turn LLM chat completion. Takes a JSON array of messages,
/// each with "role" and "content" fields. Uses the Planner role for provider resolution.
#[napi]
pub async fn llm_complete_with_messages(
    messages_json: String,
    max_tokens: Option<u32>,
) -> napi::Result<String> {
    let raw_messages: Vec<serde_json::Value> = serde_json::from_str(&messages_json)
        .map_err(|e| napi::Error::from_reason(format!("Invalid messages JSON: {}", e)))?;

    let messages: Vec<cel_llm::ChatMessage> = raw_messages
        .into_iter()
        .map(|m| {
            let role = m["role"].as_str().unwrap_or("user").to_string();
            let content = m["content"].as_str().unwrap_or("").to_string();
            cel_llm::ChatMessage::text(&role, &content)
        })
        .collect();

    let client = build_llm_client(None, None, None, None, Some(cel_llm::LlmRole::Planner))?;
    client
        .complete_with_messages(messages, max_tokens.unwrap_or(8192))
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}
