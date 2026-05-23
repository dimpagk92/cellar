//! End-to-end test of router env-var resolution.
//!
//! These tests inject env vars via `EnvSource::Map` (no real-env mutation,
//! parallel-safe).

use cellar_llm_router::{
    config::EnvSource,
    provider::{LlmProvider, MockProvider},
    router::{Router, SubsystemHandle},
    types::CompletionRequest,
};
use std::collections::HashMap;
use std::sync::Arc;

fn map_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn three_subsystems_three_providers_three_models() {
    let m = map_env(&[
        // Defaults
        ("CELLAR_DEFAULT_PROVIDER", "anthropic"),
        ("CELLAR_DEFAULT_MODEL", "claude-opus-4-7"),
        ("ANTHROPIC_API_KEY", "sk-ant-xxx"),
        // Override: NL_COMPILER uses OpenAI through OpenRouter
        ("CELLAR_NL_COMPILER_PROVIDER", "openai"),
        ("CELLAR_NL_COMPILER_MODEL", "gpt-4o-mini"),
        (
            "CELLAR_NL_COMPILER_BASE_URL",
            "https://openrouter.ai/api/v1",
        ),
        ("CELLAR_NL_COMPILER_API_KEY_ENV", "OPENROUTER_API_KEY"),
        ("OPENROUTER_API_KEY", "or-xxx"),
        // Override: MEMORY uses local Ollama
        ("CELLAR_MEMORY_PROVIDER", "ollama"),
        ("CELLAR_MEMORY_MODEL", "llama3.2:3b-instruct-q4_K_M"),
        ("CELLAR_MEMORY_BASE_URL", "http://localhost:11434"),
    ]);

    let router = Router::resolve(&["agent", "nl_compiler", "memory"], EnvSource::Map(&m))
        .expect("router resolved");

    let agent = router.get("agent").unwrap();
    assert_eq!(agent.provider.name(), "anthropic");
    assert_eq!(agent.model, "claude-opus-4-7");

    let nlc = router.get("nl_compiler").unwrap();
    assert_eq!(nlc.provider.name(), "openai");
    assert_eq!(nlc.model, "gpt-4o-mini");

    let mem = router.get("memory").unwrap();
    assert_eq!(mem.provider.name(), "ollama");
    assert_eq!(mem.model, "llama3.2:3b-instruct-q4_K_M");
}

#[test]
fn subsystem_lookup_is_case_insensitive() {
    let m = map_env(&[
        ("CELLAR_DEFAULT_PROVIDER", "anthropic"),
        ("CELLAR_DEFAULT_MODEL", "claude-opus-4-7"),
        ("ANTHROPIC_API_KEY", "sk-ant-xxx"),
    ]);
    let router = Router::resolve(&["agent"], EnvSource::Map(&m)).unwrap();
    assert!(router.get("AGENT").is_ok());
    assert!(router.get("Agent").is_ok());
    assert!(router.get("agent").is_ok());
}

#[test]
fn unknown_subsystem_errors() {
    let m = map_env(&[
        ("CELLAR_DEFAULT_PROVIDER", "anthropic"),
        ("CELLAR_DEFAULT_MODEL", "claude-opus-4-7"),
        ("ANTHROPIC_API_KEY", "sk-ant-xxx"),
    ]);
    let router = Router::resolve(&["agent"], EnvSource::Map(&m)).unwrap();
    assert!(router.get("nonexistent").is_err());
}

#[test]
fn with_handles_supports_mock_injection() {
    let mock = MockProvider::with_text("hello from mock");
    let mut handles = HashMap::new();
    handles.insert(
        "agent".into(),
        SubsystemHandle {
            provider: mock.clone() as Arc<dyn LlmProvider>,
            model: "mock-model".into(),
        },
    );
    let router = Router::with_handles(handles);
    let handle = router.get("agent").unwrap();
    assert_eq!(handle.model, "mock-model");
    assert_eq!(handle.provider.name(), "mock");
}

#[tokio::test]
async fn handle_can_complete_via_mock() {
    let mock = MockProvider::with_text("Cellar v1");
    let mut handles = HashMap::new();
    handles.insert(
        "agent".into(),
        SubsystemHandle {
            provider: mock.clone() as Arc<dyn LlmProvider>,
            model: "mock-model".into(),
        },
    );
    let router = Router::with_handles(handles);

    let handle = router.get("agent").unwrap();
    let req = CompletionRequest::new(&handle.model).user("what version?");
    let resp = handle.provider.complete(req).await.unwrap();

    assert_eq!(resp.content.len(), 1);
    // The mock recorded our request
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(mock.requests()[0].model, "mock-model");
}
