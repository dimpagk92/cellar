//! Config resolution from environment variables.
//!
//! Env var pattern:
//! ```text
//! CELLAR_DEFAULT_PROVIDER          (anthropic | openai | ollama)
//! CELLAR_DEFAULT_MODEL
//! CELLAR_DEFAULT_BASE_URL          (optional)
//! CELLAR_DEFAULT_API_KEY_ENV       (optional — names the env var holding the secret)
//!
//! CELLAR_<SUBSYSTEM>_PROVIDER      (overrides DEFAULT for that subsystem)
//! CELLAR_<SUBSYSTEM>_MODEL
//! CELLAR_<SUBSYSTEM>_BASE_URL
//! CELLAR_<SUBSYSTEM>_API_KEY_ENV
//! ```
//!
//! `<SUBSYSTEM>` is upper-snake-case (e.g., `AGENT`, `NL_COMPILER`, `MEMORY`).
//! Anything not explicitly set for a subsystem inherits from `DEFAULT`.
//!
//! In addition, `AnthropicProvider` reads `ANTHROPIC_API_KEY` if no
//! `*_API_KEY_ENV` is set, since that's the canonical Anthropic env var.
//! `OpenAIProvider` falls back to `OPENAI_API_KEY` similarly. `OllamaProvider`
//! has no auth.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::str::FromStr;

use crate::error::{LlmError, Result};

/// Known provider kinds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Native Anthropic.
    Anthropic,
    /// OpenAI-compatible (OpenAI itself, OpenRouter, LiteLLM, vLLM, LM Studio, …).
    Openai,
    /// Local models via Ollama.
    Ollama,
    /// Google Gemini (via the official OpenAI-compatible endpoint).
    Gemini,
    /// xAI / Grok (OpenAI-compatible).
    Xai,
}

impl FromStr for ProviderKind {
    type Err = LlmError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "anthropic" => Ok(ProviderKind::Anthropic),
            "openai" | "openai-compatible" => Ok(ProviderKind::Openai),
            "ollama" => Ok(ProviderKind::Ollama),
            "gemini" | "google" => Ok(ProviderKind::Gemini),
            "xai" | "grok" => Ok(ProviderKind::Xai),
            other => Err(LlmError::UnknownProvider(other.to_string())),
        }
    }
}

/// Resolved configuration for a single subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsystemConfig {
    /// Which provider to use.
    pub provider: ProviderKind,
    /// Model identifier to send.
    pub model: String,
    /// Base URL (relevant for `Openai` and `Ollama` — overrides the default endpoint).
    pub base_url: Option<String>,
    /// Resolved API key (read from the env var named by `<SUBSYSTEM>_API_KEY_ENV`,
    /// or from the provider's default env var).
    pub api_key: Option<String>,
}

/// Source for environment variable lookup. Production uses `EnvSource::Real`;
/// tests use `EnvSource::Map` to inject values without mutating real env.
pub enum EnvSource<'a> {
    /// Read from the process environment.
    Real,
    /// Read from this in-memory map (for tests).
    Map(&'a HashMap<String, String>),
}

impl<'a> EnvSource<'a> {
    fn get(&self, key: &str) -> Option<String> {
        match self {
            EnvSource::Real => env::var(key).ok(),
            EnvSource::Map(m) => m.get(key).cloned(),
        }
    }
}

/// Resolve a single subsystem's config from env vars.
///
/// `subsystem` is the upper-snake-case name (e.g., `"AGENT"`, `"NL_COMPILER"`).
pub fn resolve(subsystem: &str, env: &EnvSource<'_>) -> Result<SubsystemConfig> {
    let provider_str = env
        .get(&format!("CELLAR_{}_PROVIDER", subsystem))
        .or_else(|| env.get("CELLAR_DEFAULT_PROVIDER"))
        .ok_or_else(|| {
            LlmError::MissingConfig(format!(
                "no provider configured: set CELLAR_{}_PROVIDER or CELLAR_DEFAULT_PROVIDER",
                subsystem
            ))
        })?;
    let provider = ProviderKind::from_str(&provider_str)?;

    let model = env
        .get(&format!("CELLAR_{}_MODEL", subsystem))
        .or_else(|| env.get("CELLAR_DEFAULT_MODEL"))
        .ok_or_else(|| {
            LlmError::MissingConfig(format!(
                "no model configured: set CELLAR_{}_MODEL or CELLAR_DEFAULT_MODEL",
                subsystem
            ))
        })?;

    let base_url = env
        .get(&format!("CELLAR_{}_BASE_URL", subsystem))
        .or_else(|| env.get("CELLAR_DEFAULT_BASE_URL"));

    let api_key_env_name = env
        .get(&format!("CELLAR_{}_API_KEY_ENV", subsystem))
        .or_else(|| env.get("CELLAR_DEFAULT_API_KEY_ENV"));

    // Resolve the API key. If a per-subsystem or default name was given, use it;
    // otherwise fall back to the provider's canonical env var.
    let api_key = match api_key_env_name {
        Some(name) => env.get(&name),
        None => match provider {
            ProviderKind::Anthropic => env.get("ANTHROPIC_API_KEY"),
            ProviderKind::Openai => env.get("OPENAI_API_KEY"),
            ProviderKind::Ollama => None,
            ProviderKind::Gemini => env
                .get("GEMINI_API_KEY")
                .or_else(|| env.get("GOOGLE_API_KEY")),
            ProviderKind::Xai => env.get("XAI_API_KEY"),
        },
    };

    Ok(SubsystemConfig {
        provider,
        model,
        base_url,
        api_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_only() {
        let m = map_env(&[
            ("CELLAR_DEFAULT_PROVIDER", "anthropic"),
            ("CELLAR_DEFAULT_MODEL", "claude-opus-4-7"),
            ("ANTHROPIC_API_KEY", "sk-ant-xxx"),
        ]);
        let env = EnvSource::Map(&m);
        let cfg = resolve("AGENT", &env).unwrap();
        assert_eq!(cfg.provider, ProviderKind::Anthropic);
        assert_eq!(cfg.model, "claude-opus-4-7");
        assert_eq!(cfg.api_key.as_deref(), Some("sk-ant-xxx"));
        assert!(cfg.base_url.is_none());
    }

    #[test]
    fn subsystem_override() {
        let m = map_env(&[
            ("CELLAR_DEFAULT_PROVIDER", "anthropic"),
            ("CELLAR_DEFAULT_MODEL", "claude-opus-4-7"),
            ("ANTHROPIC_API_KEY", "sk-ant-xxx"),
            ("CELLAR_NL_COMPILER_PROVIDER", "openai"),
            ("CELLAR_NL_COMPILER_MODEL", "gpt-4o-mini"),
            (
                "CELLAR_NL_COMPILER_BASE_URL",
                "https://openrouter.ai/api/v1",
            ),
            ("CELLAR_NL_COMPILER_API_KEY_ENV", "OPENROUTER_API_KEY"),
            ("OPENROUTER_API_KEY", "or-xxx"),
        ]);
        let env = EnvSource::Map(&m);

        let agent = resolve("AGENT", &env).unwrap();
        assert_eq!(agent.provider, ProviderKind::Anthropic);
        assert_eq!(agent.model, "claude-opus-4-7");

        let nlc = resolve("NL_COMPILER", &env).unwrap();
        assert_eq!(nlc.provider, ProviderKind::Openai);
        assert_eq!(nlc.model, "gpt-4o-mini");
        assert_eq!(
            nlc.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(nlc.api_key.as_deref(), Some("or-xxx"));
    }

    #[test]
    fn ollama_no_auth() {
        let m = map_env(&[
            ("CELLAR_DEFAULT_PROVIDER", "ollama"),
            ("CELLAR_DEFAULT_MODEL", "llama3.1"),
            ("CELLAR_DEFAULT_BASE_URL", "http://localhost:11434"),
        ]);
        let env = EnvSource::Map(&m);
        let cfg = resolve("MEMORY", &env).unwrap();
        assert_eq!(cfg.provider, ProviderKind::Ollama);
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.base_url.as_deref(), Some("http://localhost:11434"));
    }

    #[test]
    fn missing_provider_errors() {
        let m = map_env(&[("CELLAR_DEFAULT_MODEL", "claude-opus-4-7")]);
        let env = EnvSource::Map(&m);
        let err = resolve("AGENT", &env).unwrap_err();
        assert!(matches!(err, LlmError::MissingConfig(_)));
    }

    #[test]
    fn unknown_provider_errors() {
        let m = map_env(&[
            ("CELLAR_DEFAULT_PROVIDER", "cohere"),
            ("CELLAR_DEFAULT_MODEL", "command-r"),
        ]);
        let env = EnvSource::Map(&m);
        let err = resolve("AGENT", &env).unwrap_err();
        assert!(matches!(err, LlmError::UnknownProvider(_)));
    }
}
