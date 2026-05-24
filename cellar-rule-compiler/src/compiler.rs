//! The compiler — orchestrates prompt → LLM → parse → validate → retry.

use cellar_llm_router::{CompletionRequest, ContentBlock, LlmProvider};
use cellar_types::rule::Rule;
use std::sync::Arc;

use crate::error::CompileError;
use crate::parse::extract_json_object;
use crate::prompt::{retry_prompt, system_prompt, user_prompt};
use crate::summary::summarize_rule;
use crate::validate::validate_rule;

/// Inputs for a compile request.
#[derive(Debug, Clone)]
pub struct CompileRequest {
    /// The user's natural-language rule.
    pub nl: String,
    /// Names of watchlists that exist in the daemon (used to scope NL hints
    /// and produce a warning when the LLM references an unknown one).
    pub watchlists: Vec<String>,
    /// Hard cap on output tokens for the LLM call. Defaults to 2048 if
    /// `None` (rules are small JSON; this is generous).
    pub max_tokens: Option<u32>,
}

impl CompileRequest {
    /// Construct a minimal request with just an NL string.
    pub fn new(nl: impl Into<String>) -> Self {
        Self {
            nl: nl.into(),
            watchlists: Vec::new(),
            max_tokens: None,
        }
    }

    /// Builder: declare the watchlist names available in the daemon.
    pub fn with_watchlists(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.watchlists = names.into_iter().map(Into::into).collect();
        self
    }
}

/// Output of a successful compile.
#[derive(Debug, Clone)]
pub struct CompileResult {
    /// The compiled rule, ready to preview / save.
    pub draft_rule: Rule,
    /// Multi-line human-readable summary the UI shows to the user.
    pub human_readable: String,
    /// Non-blocking issues — e.g., "rule references watchlist X which doesn't exist".
    pub warnings: Vec<String>,
    /// Whether the LLM call took a retry.
    pub retried: bool,
}

/// The compiler.
pub struct Compiler {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl Compiler {
    /// Construct with an LLM provider and model id (from `cellar-llm-router`).
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    /// Compile a natural-language rule into a structured `Rule`.
    pub async fn compile(&self, req: CompileRequest) -> Result<CompileResult, CompileError> {
        if req.nl.trim().is_empty() {
            return Err(CompileError::EmptyInput);
        }

        let system = system_prompt(&req.watchlists);
        let user = user_prompt(&req.nl);

        // First attempt
        let first_response = self.call(&system, &user, &req).await?;
        match self.try_finalize(&first_response, &req) {
            Ok(mut result) => {
                result.retried = false;
                Ok(result)
            }
            Err(first_err) => {
                // Retry once with the error fed back
                tracing::warn!(error = %first_err, "compile validation failed; retrying");
                let retry = retry_prompt(&req.nl, &first_response, &first_err.to_string());
                let second_response = self.call(&system, &retry, &req).await?;
                match self.try_finalize(&second_response, &req) {
                    Ok(mut result) => {
                        result.retried = true;
                        Ok(result)
                    }
                    Err(second_err) => Err(second_err),
                }
            }
        }
    }

    async fn call(
        &self,
        system: &str,
        user: &str,
        req: &CompileRequest,
    ) -> Result<String, CompileError> {
        let completion = CompletionRequest::new(&self.model)
            .with_system(system)
            .user(user)
            .with_max_tokens(req.max_tokens.unwrap_or(2048));
        let resp = self.provider.complete(completion).await?;

        // Concatenate any text blocks
        let mut text = String::new();
        for block in &resp.content {
            if let ContentBlock::Text { text: t } = block {
                text.push_str(t);
            }
        }
        Ok(text)
    }

    fn try_finalize(
        &self,
        response_text: &str,
        req: &CompileRequest,
    ) -> Result<CompileResult, CompileError> {
        let raw_json = extract_json_object(response_text).ok_or(CompileError::NoJsonInResponse)?;

        // Try to deserialize into Rule
        let rule: Rule = match serde_json::from_str(raw_json) {
            Ok(r) => r,
            Err(e) => return Err(CompileError::Validation(format!("{e}"))),
        };

        let warnings = validate_rule(&rule, &req.watchlists);
        let human_readable = summarize_rule(&rule);

        Ok(CompileResult {
            draft_rule: rule,
            human_readable,
            warnings,
            retried: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_llm_router::{
        provider::MockProvider,
        types::{CompletionResponse, ContentBlock, StopReason, Usage},
    };

    fn mock_with_responses(texts: &[&str]) -> Arc<MockProvider> {
        let responses: Vec<CompletionResponse> = texts
            .iter()
            .map(|t| CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: t.to_string(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: None,
            })
            .collect();
        MockProvider::new(responses)
    }

    const GOOD_JSON: &str = r#"{
        "id": "draft",
        "name": "Big delete",
        "nl_original": "notify when files >1GB are deleted from Documents",
        "kind": "watcher",
        "enabled": true,
        "created_at": "1970-01-01T00:00:00Z",
        "match": {
            "all": [
                {"leaf": {"field": "kind", "op": "eq", "value": "file_deleted"}},
                {"leaf": {"field": "data.size_bytes", "op": "gte", "value": 1073741824}}
            ]
        },
        "action": {"type": "webhook", "webhook_id": "default"},
        "cooldown_seconds": 60
    }"#;

    #[tokio::test]
    async fn compile_succeeds_on_first_try() {
        let provider = mock_with_responses(&[GOOD_JSON]);
        let compiler = Compiler::new(provider, "mock-model");
        let req = CompileRequest::new("notify when files >1GB are deleted from Documents");
        let result = compiler.compile(req).await.unwrap();
        assert_eq!(result.draft_rule.name, "Big delete");
        assert!(!result.retried);
        assert!(result.warnings.is_empty());
        assert!(result.human_readable.contains("WHEN"));
    }

    #[tokio::test]
    async fn compile_strips_fenced_code_block() {
        let fenced = format!("Here you go:\n```json\n{}\n```\n", GOOD_JSON);
        let provider = mock_with_responses(&[&fenced]);
        let compiler = Compiler::new(provider, "mock-model");
        let req = CompileRequest::new("anything");
        let result = compiler.compile(req).await.unwrap();
        assert_eq!(result.draft_rule.kind, cellar_types::RuleKind::Watcher);
    }

    #[tokio::test]
    async fn compile_retries_on_invalid_json() {
        let bad = r#"{"id":"draft","kind":"not_a_real_kind"}"#;
        let provider = mock_with_responses(&[bad, GOOD_JSON]);
        let compiler = Compiler::new(provider, "mock-model");
        let req = CompileRequest::new("anything");
        let result = compiler.compile(req).await.unwrap();
        assert!(result.retried);
        assert_eq!(result.draft_rule.name, "Big delete");
    }

    #[tokio::test]
    async fn compile_fails_after_two_bad_responses() {
        let bad = r#"{"id":"draft","kind":"not_a_real_kind"}"#;
        let provider = mock_with_responses(&[bad, bad]);
        let compiler = Compiler::new(provider, "mock-model");
        let req = CompileRequest::new("anything");
        let err = compiler.compile(req).await.unwrap_err();
        assert!(matches!(err, CompileError::Validation(_)));
    }

    #[tokio::test]
    async fn empty_nl_errors_synchronously() {
        let provider = mock_with_responses(&[GOOD_JSON]);
        let compiler = Compiler::new(provider, "mock-model");
        let err = compiler
            .compile(CompileRequest::new("   "))
            .await
            .unwrap_err();
        assert!(matches!(err, CompileError::EmptyInput));
    }

    #[tokio::test]
    async fn no_json_in_response_errors() {
        let provider = mock_with_responses(&["I'm sorry, I can't help with that."]);
        let compiler = Compiler::new(provider, "mock-model");
        let err = compiler
            .compile(CompileRequest::new("rule please"))
            .await
            .unwrap_err();
        // First attempt: NoJsonInResponse; retry also fails because mock returns same text.
        // After two tries, the wrapper still surfaces NoJsonInResponse.
        assert!(matches!(err, CompileError::NoJsonInResponse));
    }

    #[tokio::test]
    async fn unknown_watchlist_surfaces_as_warning() {
        let provider = mock_with_responses(&[GOOD_JSON]);
        let compiler = Compiler::new(provider, "mock-model");
        // GOOD_JSON doesn't reference any watchlist, so no warnings.
        let req = CompileRequest::new("anything").with_watchlists(["approved_apps"]);
        let result = compiler.compile(req).await.unwrap();
        assert!(result.warnings.is_empty());

        // Now with a JSON that does reference an unknown watchlist:
        let json_with_watchlist = r#"{
            "id": "draft",
            "name": "App allowlist",
            "nl_original": "tell me when an app outside my approved list launches",
            "kind": "watcher",
            "enabled": true,
            "created_at": "1970-01-01T00:00:00Z",
            "match": {
                "all": [
                    {"leaf": {"field": "kind", "op": "eq", "value": "process_started"}},
                    {"leaf": {"field": "data.bundle_id", "op": "not_in_watchlist", "value": "approved_apps_v2"}}
                ]
            },
            "action": {"type": "webhook", "webhook_id": "default"},
            "cooldown_seconds": 60
        }"#;
        let provider2 = mock_with_responses(&[json_with_watchlist]);
        let compiler2 = Compiler::new(provider2, "mock-model");
        let req2 = CompileRequest::new("anything").with_watchlists(["approved_apps"]);
        let result2 = compiler2.compile(req2).await.unwrap();
        assert!(!result2.warnings.is_empty());
        assert!(result2
            .warnings
            .iter()
            .any(|w| w.contains("approved_apps_v2")));
    }
}
