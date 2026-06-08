//! The Router type. Holds per-subsystem provider handles resolved from env vars.

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{resolve, EnvSource, ProviderKind, SubsystemConfig};
use crate::error::{LlmError, Result};
use crate::provider::LlmProvider;
use crate::providers::{
    AnthropicProvider, GeminiProvider, OllamaProvider, OpenAiProvider, XaiProvider,
};

/// One subsystem's resolved handle: provider + model name.
#[derive(Clone)]
pub struct SubsystemHandle {
    /// Provider implementation to call.
    pub provider: Arc<dyn LlmProvider>,
    /// Model identifier to send in `CompletionRequest.model`.
    pub model: String,
}

/// The router: maps subsystem name → handle.
pub struct Router {
    handles: HashMap<String, SubsystemHandle>,
}

impl Router {
    /// Construct a router by resolving each named subsystem from the real
    /// process environment. Use this from the daemon entrypoint.
    pub fn from_env(subsystems: &[&str]) -> Result<Self> {
        Self::resolve(subsystems, EnvSource::Real)
    }

    /// Construct a router by resolving each named subsystem from an explicit
    /// env-source. Used by tests.
    pub fn resolve(subsystems: &[&str], env: EnvSource<'_>) -> Result<Self> {
        let mut handles = HashMap::new();
        for name in subsystems {
            let upper = name.to_ascii_uppercase();
            let cfg = resolve(&upper, &env)?;
            let handle = build_handle(cfg)?;
            handles.insert(name.to_ascii_lowercase(), handle);
        }
        Ok(Self { handles })
    }

    /// Construct a router with explicitly-provided handles. Used by tests that
    /// want to inject `MockProvider`.
    pub fn with_handles(handles: HashMap<String, SubsystemHandle>) -> Self {
        Self { handles }
    }

    /// Get the handle for a subsystem. Subsystem names are case-insensitive on
    /// lookup (the router normalizes to lowercase internally).
    pub fn get(&self, subsystem: &str) -> Result<&SubsystemHandle> {
        self.handles
            .get(&subsystem.to_ascii_lowercase())
            .ok_or_else(|| LlmError::UnknownSubsystem(subsystem.to_string()))
    }

    /// List the subsystems registered with this router.
    pub fn subsystems(&self) -> Vec<&str> {
        self.handles.keys().map(String::as_str).collect()
    }
}

fn build_handle(cfg: SubsystemConfig) -> Result<SubsystemHandle> {
    let model = cfg.model.clone();
    let provider: Arc<dyn LlmProvider> = match cfg.provider {
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(cfg.api_key, cfg.base_url)?),
        ProviderKind::Openai => Arc::new(OpenAiProvider::new(cfg.api_key, cfg.base_url)?),
        ProviderKind::Ollama => Arc::new(OllamaProvider::new(cfg.base_url)?),
        ProviderKind::Gemini => Arc::new(GeminiProvider::new(cfg.api_key)?),
        ProviderKind::Xai => Arc::new(XaiProvider::new(cfg.api_key)?),
    };
    Ok(SubsystemHandle { provider, model })
}
