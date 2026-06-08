//! The compiler — orchestrates prompt → LLM → parse → validate → retry.
//!
//! When a `MemoryProvider` is attached via [`Compiler::with_memory`], the
//! compiler will fetch precedent rules and recent fires from memory before
//! the first LLM call. The precedents are inlined into the user prompt so
//! the model can mimic prior style and reuse existing watchlist/webhook
//! identifiers without the user having to repeat them.
//!
//! Precedent retrieval happens at most once per `compile` invocation (the
//! retry call reuses the same precedents). It is best-effort: a memory
//! error logs a warning and falls back to a precedent-free prompt rather
//! than failing the compile.

use cel_memory::{
    CallerScope, ChunkKind, MemoryChunk, MemoryProvider, MemoryQuery, RetrievalProfile,
};
use cellar_llm_router::{CompletionRequest, ContentBlock, LlmProvider};
use cellar_types::rule::Rule;
use std::sync::Arc;

use crate::error::CompileError;
use crate::parse::extract_json_object;
use crate::prompt::{retry_prompt, system_prompt, user_prompt};
use crate::summary::summarize_rule;
use crate::validate::validate_rule;

/// Caller identity used when the compiler retrieves precedents from memory.
/// Lives in its own caller bucket so audit/eviction policies can target
/// compiler activity specifically.
const COMPILER_CALLER_ID: &str = "nl_compiler";

/// Max precedent rules / fires to include in the prompt. Keeping this
/// small avoids token bloat — the LLM only needs a couple of examples.
const PRECEDENT_K: usize = 3;

/// Two ordered lists pulled from memory before each compile. Empty when
/// no memory is attached or both retrievals failed; an empty `Precedents`
/// produces a precedent-free prompt, matching the pre-memory behavior.
#[derive(Debug, Default)]
struct Precedents {
    /// Prior compiled rules — chunks written by `record_compile` carrying
    /// `nl_original` in their metadata and `NL: …\nRule: …` in content.
    rules: Vec<MemoryChunk>,
    /// Recent rule firings — chunks written by the matcher consumer
    /// (`cel-cortex-daemon::matcher_task`) carrying `rule_name`,
    /// `event_kind`, action type, etc.
    fires: Vec<MemoryChunk>,
}

impl Precedents {
    fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.fires.is_empty()
    }
}

/// Inline precedents into the user prompt, or return the bare prompt
/// when there are none. The precedent block is delimited by a stable
/// marker so the LLM can distinguish prior context from the new request.
fn build_user_prompt_with_precedents(nl: &str, precedents: &Precedents) -> String {
    if precedents.is_empty() {
        return user_prompt(nl);
    }
    let mut block = String::new();
    block.push_str("PRECEDENT (for stylistic reference — do not copy verbatim):\n");
    if !precedents.rules.is_empty() {
        block.push_str("\nSimilar rules previously compiled:\n");
        for (i, chunk) in precedents.rules.iter().enumerate() {
            block.push_str(&format!("  [{}] {}\n", i + 1, summarize_chunk(chunk)));
        }
    }
    if !precedents.fires.is_empty() {
        block.push_str("\nRecent firings that may be relevant:\n");
        for (i, chunk) in precedents.fires.iter().enumerate() {
            block.push_str(&format!("  [{}] {}\n", i + 1, summarize_chunk(chunk)));
        }
    }
    block.push_str("\nEND PRECEDENT\n\n");
    block.push_str(&user_prompt(nl));
    block
}

/// One-line excerpt of a chunk for the precedent block. Caps at 200 chars
/// so a chatty content field can't blow up the prompt.
fn summarize_chunk(chunk: &MemoryChunk) -> String {
    let mut s: String = chunk.content.lines().next().unwrap_or("").to_string();
    if s.len() > 200 {
        s.truncate(200);
        s.push('…');
    }
    s
}

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
    /// Optional precedent source. When set, [`Compiler::compile`] retrieves
    /// similar prior rules and recent fires before the first LLM call and
    /// inlines them into the user prompt. Unset → precedent-free prompts.
    memory: Option<Arc<dyn MemoryProvider>>,
}

impl Compiler {
    /// Construct with an LLM provider and model id (from `cellar-llm-router`).
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            memory: None,
        }
    }

    /// Attach a memory provider for precedent retrieval. The compiler will
    /// pull similar prior rules and recent fires using `RetrievalProfile`'s
    /// `NLCompilerSimilarRules` / `NLCompilerSimilarFires` weight tables
    /// and inline a compact summary into the user prompt.
    pub fn with_memory(mut self, memory: Arc<dyn MemoryProvider>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Compile a natural-language rule into a structured `Rule`.
    pub async fn compile(&self, req: CompileRequest) -> Result<CompileResult, CompileError> {
        if req.nl.trim().is_empty() {
            return Err(CompileError::EmptyInput);
        }

        let system = system_prompt(&req.watchlists);
        let precedents = self.retrieve_precedents(&req.nl).await;
        let user = build_user_prompt_with_precedents(&req.nl, &precedents);

        // First attempt
        let first_response = self.call(&system, &user, &req).await?;
        match self.try_finalize(&first_response, &req) {
            Ok(mut result) => {
                result.retried = false;
                self.record_compile(&req.nl, &result.draft_rule).await;
                Ok(result)
            }
            Err(first_err) => {
                // Retry once with the error fed back. The precedent block is
                // intentionally NOT re-included — the retry prompt is purely
                // about fixing JSON, and the model has already seen the
                // precedents in the original turn.
                tracing::warn!(error = %first_err, "compile validation failed; retrying");
                let retry = retry_prompt(&req.nl, &first_response, &first_err.to_string());
                let second_response = self.call(&system, &retry, &req).await?;
                match self.try_finalize(&second_response, &req) {
                    Ok(mut result) => {
                        result.retried = true;
                        self.record_compile(&req.nl, &result.draft_rule).await;
                        Ok(result)
                    }
                    Err(second_err) => Err(second_err),
                }
            }
        }
    }

    /// Fetch precedents for the compile prompt. Two parallel retrievals,
    /// each scoped to its tuned `RetrievalProfile`. Errors are swallowed
    /// (logged as warnings) — a memory failure must not block a compile.
    async fn retrieve_precedents(&self, nl: &str) -> Precedents {
        let Some(memory) = self.memory.as_ref() else {
            return Precedents::default();
        };
        let rules_q = MemoryQuery {
            text: nl.to_string(),
            kinds: Some(vec![ChunkKind::Context]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Global,
            project_root_prefix: None,
            k: PRECEDENT_K,
            include_rollups: false,
            min_importance: None,
            profile: RetrievalProfile::NLCompilerSimilarRules,
            caller_id: COMPILER_CALLER_ID.to_string(),
        };
        let fires_q = MemoryQuery {
            text: nl.to_string(),
            kinds: Some(vec![ChunkKind::Fire]),
            since: None,
            until: None,
            session_id: None,
            caller_scope: CallerScope::Global,
            project_root_prefix: None,
            k: PRECEDENT_K,
            include_rollups: false,
            min_importance: None,
            profile: RetrievalProfile::NLCompilerSimilarFires,
            caller_id: COMPILER_CALLER_ID.to_string(),
        };
        let (rules_res, fires_res) =
            tokio::join!(memory.retrieve(rules_q), memory.retrieve(fires_q));
        let rules = match rules_res {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "precedent retrieve (rules) failed; continuing without");
                Vec::new()
            }
        };
        let fires = match fires_res {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "precedent retrieve (fires) failed; continuing without");
                Vec::new()
            }
        };
        Precedents { rules, fires }
    }

    /// Persist a "compiled rule" chunk so future compile calls can
    /// retrieve it via `NLCompilerSimilarRules`. Best-effort — a memory
    /// write failure logs a warning but never propagates out of `compile`.
    async fn record_compile(&self, nl: &str, rule: &Rule) {
        let Some(memory) = self.memory.as_ref() else {
            return;
        };
        let content = format!("NL: {nl}\nRule: {}", rule.name);
        let metadata = serde_json::json!({
            "rule_id": rule.id,
            "rule_name": rule.name,
            "rule_kind": rule.kind,
            "nl_original": nl,
            "action_type": rule.action.action_type,
        });
        let new_chunk = cel_memory::NewMemoryChunk {
            kind: ChunkKind::Context,
            source: cel_memory::ChunkSource::System,
            session_id: None,
            project_root: None,
            caller_id: COMPILER_CALLER_ID.to_string(),
            content,
            metadata,
            importance: None,
            shareable: true,
            pinned: false,
        };
        if let Err(e) = memory.write(new_chunk).await {
            tracing::warn!(
                error = %e,
                rule_id = %rule.id,
                "failed to record NL→Rule compile precedent"
            );
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

    // ───── Precedent retrieval (Phase 2 closeout) ─────

    use cel_memory::{BasicMemoryProvider, ChunkSource, NewMemoryChunk};

    /// The first user-message text block from the *first* request the mock
    /// provider observed. Tests use this to assert the precedent block is
    /// (or isn't) present in the prompt the LLM saw.
    fn first_user_text(provider: &Arc<MockProvider>) -> String {
        let reqs = provider.requests();
        let req = reqs.first().expect("expected at least one LLM request");
        for msg in &req.messages {
            if msg.role == cellar_llm_router::Role::User {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        return text.clone();
                    }
                }
            }
        }
        panic!("no user-text content block in first request");
    }

    #[tokio::test]
    async fn precedent_block_absent_when_no_memory_attached() {
        let provider = mock_with_responses(&[GOOD_JSON]);
        let compiler = Compiler::new(provider.clone(), "mock-model");
        compiler
            .compile(CompileRequest::new("notify when big files are deleted"))
            .await
            .unwrap();
        let user_text = first_user_text(&provider);
        assert!(
            !user_text.contains("PRECEDENT"),
            "no memory → no PRECEDENT block; got:\n{user_text}"
        );
    }

    #[tokio::test]
    async fn precedent_block_absent_when_memory_is_empty() {
        let provider = mock_with_responses(&[GOOD_JSON]);
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let compiler = Compiler::new(provider.clone(), "mock-model").with_memory(memory);
        compiler
            .compile(CompileRequest::new("notify when big files are deleted"))
            .await
            .unwrap();
        let user_text = first_user_text(&provider);
        assert!(
            !user_text.contains("PRECEDENT"),
            "empty memory → no PRECEDENT block; got:\n{user_text}"
        );
    }

    #[tokio::test]
    async fn precedent_block_present_when_memory_has_prior_compile() {
        // Seed memory with a prior compiled rule so the next compile pulls
        // it as a precedent.
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        memory
            .write(NewMemoryChunk {
                kind: ChunkKind::Context,
                source: ChunkSource::System,
                session_id: None,
                project_root: None,
                caller_id: "nl_compiler".into(),
                content: "NL: notify when files are deleted\nRule: Big delete".into(),
                metadata: serde_json::json!({
                    "rule_id": "rule_big_delete",
                    "rule_name": "Big delete",
                    "nl_original": "notify when files are deleted",
                }),
                importance: None,
                shareable: true,
                pinned: false,
            })
            .await
            .unwrap();

        let provider = mock_with_responses(&[GOOD_JSON]);
        let compiler = Compiler::new(provider.clone(), "mock-model").with_memory(memory);
        // Use a query whose tokens overlap the seeded content — the v1
        // BasicMemoryProvider falls back to case-insensitive substring
        // matching (SqliteMemoryProvider does proper hybrid scoring, but
        // we're testing the wiring here, not the ranker).
        compiler
            .compile(CompileRequest::new("notify when files are deleted"))
            .await
            .unwrap();
        let user_text = first_user_text(&provider);
        assert!(
            user_text.contains("PRECEDENT"),
            "expected PRECEDENT block when memory is populated; got:\n{user_text}"
        );
        // The summarizer keeps only the first line of each chunk, so the
        // seeded `Rule: Big delete` second line is intentionally clipped.
        // We only verify the `NL: …` first line landed.
        assert!(
            user_text.contains("NL: notify when files are deleted"),
            "precedent block should excerpt the seeded chunk's first line; got:\n{user_text}"
        );
    }

    #[tokio::test]
    async fn record_compile_writes_context_chunk() {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let provider = mock_with_responses(&[GOOD_JSON]);
        let compiler =
            Compiler::new(provider.clone(), "mock-model").with_memory(Arc::clone(&memory));

        let before = memory.stats().await.unwrap().total_chunks;
        compiler
            .compile(CompileRequest::new("notify when big files are deleted"))
            .await
            .unwrap();
        let after = memory.stats().await.unwrap().total_chunks;
        assert_eq!(
            after,
            before + 1,
            "successful compile must persist exactly one precedent chunk"
        );
    }
}
