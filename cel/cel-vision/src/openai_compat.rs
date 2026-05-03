use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use cel_display::{encode_for_llm, Frame};
use cel_llm::{strip_code_fences, LlmClient, LlmProviderConfig};

use crate::provider::{VisionElement, VisionError, VisionProvider};

/// Maximum number of retries on parse failure before giving up.
const MAX_RETRIES: usize = 2;

/// System prompt instructing the model to return structured UI element data.
const VISION_SYSTEM_PROMPT: &str = r#"You are a UI element detector. Analyze the screenshot and identify all visible UI elements.
Return a JSON array of objects with these fields:
- "label": the visible text or description of the element
- "element_type": one of "button", "input", "text", "link", "checkbox", "dropdown", "menu", "tab", "icon", "image", "dialog", "other"
- "bounds": {"x": int, "y": int, "width": int, "height": int} in pixel coordinates from top-left, or null if uncertain
- "confidence": float 0.0-1.0 indicating how confident you are

Return ONLY the JSON array, no other text."#;

/// Stricter retry prompt when the first attempt returns unparseable output.
const VISION_RETRY_PROMPT: &str = r#"Your previous response was not valid JSON. You MUST return ONLY a JSON array. No markdown, no explanation, no code fences. Example: [{"label":"OK","element_type":"button","bounds":null,"confidence":0.9}]"#;

/// Maximum number of cached responses.
const CACHE_MAX_ENTRIES: usize = 10;

/// Time-to-live for cached responses in seconds.
const CACHE_TTL_SECS: u64 = 30;

/// Vision provider backed by a [`LlmClient`].
pub struct OpenAICompatProvider {
    client: LlmClient,
    provider_name: String,
    /// Simple response cache: (hash_of_png_bytes, timestamp, elements).
    cache: Mutex<Vec<(u64, Instant, Vec<VisionElement>)>>,
}

impl OpenAICompatProvider {
    pub fn new(config: LlmProviderConfig) -> Result<Self, VisionError> {
        let client = LlmClient::new(config)?;
        let provider_name = client.provider_name();
        Ok(Self {
            client,
            provider_name,
            cache: Mutex::new(Vec::new()),
        })
    }

    /// Hash image bytes using [`DefaultHasher`].
    fn hash_bytes(data: &[u8]) -> u64 {
        let mut hasher = DefaultHasher::new();
        data.hash(&mut hasher);
        hasher.finish()
    }

    /// Look up a cached result by hash, returning it if still within TTL.
    fn cache_lookup(&self, hash: u64) -> Option<Vec<VisionElement>> {
        let cache = self.cache.lock().ok()?;
        for (h, ts, elements) in cache.iter() {
            if *h == hash && ts.elapsed().as_secs() < CACHE_TTL_SECS {
                return Some(elements.clone());
            }
        }
        None
    }

    /// Store a result in the cache, evicting the oldest entry if at capacity.
    fn cache_store(&self, hash: u64, elements: &[VisionElement]) {
        if let Ok(mut cache) = self.cache.lock() {
            // Evict expired entries first.
            cache.retain(|(_, ts, _)| ts.elapsed().as_secs() < CACHE_TTL_SECS);
            // If still at capacity, remove the oldest entry.
            if cache.len() >= CACHE_MAX_ENTRIES {
                cache.remove(0);
            }
            cache.push((hash, Instant::now(), elements.to_vec()));
        }
    }
}

#[async_trait]
impl VisionProvider for OpenAICompatProvider {
    async fn analyze(
        &self,
        frame: &Frame,
        prompt: &str,
        detail: Option<&str>,
    ) -> Result<Vec<VisionElement>, VisionError> {
        // Encode as JPEG+base64 in one call (5-10x smaller than PNG, faster API calls).
        let b64 = encode_for_llm(frame, 1568, 80)
            .map_err(|e| VisionError::EncodeFailed(e.to_string()))?;

        // Check the cache before making an API call.
        let frame_hash = Self::hash_bytes(b64.as_bytes());
        if let Some(cached) = self.cache_lookup(frame_hash) {
            tracing::debug!("Vision cache hit for hash {:#x}", frame_hash);
            return Ok(cached);
        }

        let data_url = format!("data:image/jpeg;base64,{}", b64);

        let user_prompt = if prompt.is_empty() {
            "Identify all UI elements in this screenshot."
        } else {
            prompt
        };

        let content = self
            .client
            .complete_with_image(VISION_SYSTEM_PROMPT, &data_url, user_prompt, 4096, detail)
            .await?;

        // Try to parse; on failure, retry with a stricter prompt
        let json_str = strip_code_fences(&content);
        match serde_json::from_str::<Vec<VisionElement>>(json_str) {
            Ok(elements) => {
                self.cache_store(frame_hash, &elements);
                Ok(elements)
            }
            Err(first_err) => {
                tracing::warn!(
                    "Vision response parse failed ({}), retrying with stricter prompt",
                    first_err
                );

                for attempt in 0..MAX_RETRIES {
                    let retry_content = self
                        .client
                        .complete_with_image(
                            VISION_SYSTEM_PROMPT,
                            &data_url,
                            VISION_RETRY_PROMPT,
                            4096,
                            detail,
                        )
                        .await?;

                    let retry_json = strip_code_fences(&retry_content);
                    match serde_json::from_str::<Vec<VisionElement>>(retry_json) {
                        Ok(elements) => {
                            self.cache_store(frame_hash, &elements);
                            return Ok(elements);
                        }
                        Err(e) => {
                            tracing::warn!("Vision retry {} failed: {}", attempt + 1, e);
                        }
                    }
                }

                Err(VisionError::ApiFailed(format!(
                    "Failed to parse vision response after {} retries: {}",
                    MAX_RETRIES, first_err
                )))
            }
        }
    }

    async fn ask(
        &self,
        frame: &Frame,
        question: &str,
        detail: Option<&str>,
    ) -> Result<String, VisionError> {
        let b64 = encode_for_llm(frame, 1568, 80)
            .map_err(|e| VisionError::EncodeFailed(e.to_string()))?;
        let data_url = format!("data:image/jpeg;base64,{}", b64);

        let system = "You are a UI analysis assistant. Answer the user's question about the screenshot concisely and accurately.";
        let answer = self
            .client
            .complete_with_image(system, &data_url, question, 1024, detail)
            .await?;

        Ok(answer)
    }

    async fn compare(
        &self,
        before: &Frame,
        after: &Frame,
        question: &str,
    ) -> Result<String, VisionError> {
        let before_b64 = encode_for_llm(before, 1024, 70)
            .map_err(|e| VisionError::EncodeFailed(e.to_string()))?;
        let after_b64 = encode_for_llm(after, 1024, 70)
            .map_err(|e| VisionError::EncodeFailed(e.to_string()))?;

        let system = "You are a UI change detector. You will see two screenshots: the BEFORE image (first) and the AFTER image (second). Compare them and answer the user's question about what changed.";
        let before_url = format!("data:image/jpeg;base64,{}", before_b64);
        let after_url = format!("data:image/jpeg;base64,{}", after_b64);

        let answer = self
            .client
            .complete_with_images(system, &[&before_url, &after_url], question, 1024)
            .await?;

        Ok(answer)
    }

    fn name(&self) -> &str {
        self.provider_name.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vision_elements() {
        let json = r#"[
            {"label": "Submit", "element_type": "button", "bounds": {"x": 100, "y": 200, "width": 80, "height": 30}, "confidence": 0.95},
            {"label": "Username", "element_type": "input", "bounds": null, "confidence": 0.8}
        ]"#;
        let elements: Vec<VisionElement> = serde_json::from_str(json).unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].label, "Submit");
        assert_eq!(elements[0].element_type, "button");
        assert!(elements[0].bounds.is_some());
        assert_eq!(elements[1].label, "Username");
        assert!(elements[1].bounds.is_none());
    }

    #[test]
    fn test_parse_markdown_wrapped() {
        let raw = "```json\n[{\"label\": \"OK\", \"element_type\": \"button\", \"bounds\": null, \"confidence\": 0.9}]\n```";
        let json_str = strip_code_fences(raw);
        let elements: Vec<VisionElement> = serde_json::from_str(json_str).unwrap();
        assert_eq!(elements.len(), 1);
    }
}
