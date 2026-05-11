use serde::{Deserialize, Serialize};

/// Path to the user-level config file written by `cellar init`.
fn config_file_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".cellar")
            .join("config.toml"),
    )
}

/// On-disk config file schema. Only the `[llm]` section is consulted today.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    llm: Option<ConfigFileLlm>,
}

#[derive(Debug, Deserialize)]
struct ConfigFileLlm {
    provider: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    escalation_model: Option<String>,
}

/// Known LLM provider kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    OpenAI,
    Gemini,
    Anthropic,
    HuggingFace,
    Ollama,
    Custom,
}

impl ProviderKind {
    /// Default API endpoint for this provider. All endpoints are
    /// OpenAI-compatible chat completions URLs — Anthropic uses its compat
    /// shim (`/v1/chat/completions` instead of native `/v1/messages`) so the
    /// LlmClient can speak one protocol everywhere.
    pub fn default_endpoint(&self) -> &str {
        match self {
            Self::OpenAI => "https://api.openai.com/v1/chat/completions",
            Self::Gemini => {
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
            }
            // OpenAI-compat shim, NOT native /v1/messages.
            // See https://docs.anthropic.com/en/api/openai-sdk
            Self::Anthropic => "https://api.anthropic.com/v1/chat/completions",
            Self::Ollama => "http://localhost:11434/v1/chat/completions",
            Self::HuggingFace | Self::Custom => "",
        }
    }

    /// Auto-detect provider from a base URL (or full chat completions URL)
    /// when `CEL_LLM_PROVIDER` is unset. Lets users switch backends just by
    /// changing `CEL_LLM_BASE_URL` without flipping a separate provider flag.
    /// Falls back to `Custom` for unknown hosts (still works — we send
    /// OpenAI-shaped requests with Bearer auth, the de facto standard).
    pub fn from_base_url(url: &str) -> Self {
        let url = url.to_lowercase();
        if url.contains("api.anthropic.com") {
            Self::Anthropic
        } else if url.contains("api.openai.com") {
            Self::OpenAI
        } else if url.contains("generativelanguage.googleapis.com") {
            Self::Gemini
        } else if url.contains("localhost") || url.contains("127.0.0.1") {
            Self::Ollama
        } else {
            Self::Custom
        }
    }

    /// Default model for this provider.
    pub fn default_model(&self) -> &str {
        match self {
            Self::OpenAI => "gpt-4o",
            Self::Gemini => "gemini-2.5-flash",
            Self::Anthropic => "claude-sonnet-4-20250514",
            Self::Ollama => "gemma4:e4b",
            Self::HuggingFace | Self::Custom => "",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAI => write!(f, "openai"),
            Self::Gemini => write!(f, "gemini"),
            Self::Anthropic => write!(f, "anthropic"),
            Self::HuggingFace => write!(f, "huggingface"),
            Self::Ollama => write!(f, "ollama"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

/// Parse a provider string into a ProviderKind.
impl From<&str> for ProviderKind {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "openai" => Self::OpenAI,
            "gemini" => Self::Gemini,
            "anthropic" | "claude" => Self::Anthropic,
            "huggingface" | "hf" => Self::HuggingFace,
            "ollama" => Self::Ollama,
            _ => Self::Custom,
        }
    }
}

/// Model capability tier — determines prompt complexity and context budget.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Small/fast models (gemini-flash, gpt-4o-mini, haiku). Short prompts, aggressive filtering.
    Flash,
    /// Standard models (gpt-4o, claude-sonnet, gemini-pro). Default behavior.
    #[default]
    Standard,
    /// Premium models (claude-opus, o3, gpt-5). Extended prompts, more context.
    Premium,
}

/// Profile describing a model's capabilities.
#[derive(Debug, Clone)]
pub struct ModelProfile {
    pub provider: ProviderKind,
    pub model_id: String,
    pub tier: ModelTier,
}

impl ModelProfile {
    /// Infer a model profile from a model ID string.
    pub fn from_model_id(model_id: &str) -> Self {
        let lower = model_id.to_lowercase();
        let provider = if lower.contains("claude") || lower.contains("anthropic") {
            ProviderKind::Anthropic
        } else if lower.contains("gemini") {
            ProviderKind::Gemini
        } else if lower.contains("gpt") || lower.contains("o1") || lower.contains("o3") {
            ProviderKind::OpenAI
        } else {
            ProviderKind::Custom
        };

        let tier = if lower.contains("flash")
            || lower.contains("mini")
            || lower.contains("haiku")
            || lower.contains("nano")
        {
            ModelTier::Flash
        } else if lower.contains("opus")
            || lower.contains("o3")
            || lower.contains("gpt-5")
            || lower.contains("pro")
        {
            ModelTier::Premium
        } else {
            ModelTier::Standard
        };

        ModelProfile {
            provider,
            model_id: model_id.to_string(),
            tier,
        }
    }
}

/// Role-based LLM routing — each role can use a different provider/model.
/// Configured via env vars: `CEL_LLM_{ROLE}_PROVIDER`, `CEL_LLM_{ROLE}_MODEL`, etc.
/// Falls back to base `CEL_LLM_*` vars when role-specific vars are not set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmRole {
    /// Reasoning, step planning, self-healing. Best with Claude/GPT-4o.
    Planner,
    /// Quick verification, context analysis. Best with Gemini Flash.
    Observer,
    /// Screenshot interpretation. Best with GPT-4o/Claude.
    Vision,
    /// Base fallback for llm_complete and other general calls.
    General,
    /// Independent action success/failure judgment. Best with Gemini Flash.
    Validator,
    /// Visual element grounding/localization. Best with Gemini Flash.
    Localizer,
    /// Goal decomposition and replanning. Best with Gemini Flash.
    Orchestrator,
}

impl LlmRole {
    /// Env var prefix for this role (e.g., "CEL_LLM_PLANNER").
    fn env_prefix(&self) -> &str {
        match self {
            Self::Planner => "CEL_LLM_PLANNER",
            Self::Observer => "CEL_LLM_OBSERVER",
            Self::Vision => "CEL_LLM_VISION",
            Self::General => "CEL_LLM",
            Self::Validator => "CEL_LLM_VALIDATOR",
            Self::Localizer => "CEL_LLM_LOCALIZER",
            Self::Orchestrator => "CEL_LLM_ORCHESTRATOR",
        }
    }
}

/// Configuration for an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProviderConfig {
    /// Provider kind.
    pub provider: ProviderKind,
    /// API endpoint URL. Falls back to the provider's default if unset.
    pub endpoint: Option<String>,
    /// API key.
    pub api_key: Option<String>,
    /// Model name/ID. Falls back to the provider's default if unset.
    pub model: Option<String>,
    /// Sampling temperature (0.0–2.0). `None` uses the provider's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Stronger model to escalate to after consecutive failures.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_model: Option<String>,
}

impl LlmProviderConfig {
    /// Build configuration from environment variables.
    ///
    /// Minimal config for the common case — three env vars, one provider:
    ///
    /// - `CEL_LLM_API_KEY` — your API key (single key for the whole runtime)
    /// - `CEL_LLM_BASE_URL` — full chat completions URL (OpenAI-compatible).
    ///   Switch providers just by changing this. Examples:
    ///   - Anthropic: `https://api.anthropic.com/v1/chat/completions`
    ///   - Gemini:    `https://generativelanguage.googleapis.com/v1beta/openai/chat/completions`
    ///   - OpenAI:    `https://api.openai.com/v1/chat/completions`
    ///   - Ollama:    `http://localhost:11434/v1/chat/completions`
    /// - `CEL_LLM_MODEL` — model id (e.g. `claude-sonnet-4-20250514`,
    ///   `gemini-2.5-flash`, `gpt-4o`)
    ///
    /// `CEL_LLM_PROVIDER` is optional; auto-detected from `CEL_LLM_BASE_URL`
    /// when unset. `CEL_LLM_ENDPOINT` is kept as an alias for `CEL_LLM_BASE_URL`
    /// for backwards compatibility.
    ///
    /// Provider-specific API key fallbacks (when `CEL_LLM_API_KEY` is unset):
    /// `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`,
    /// `HUGGINGFACE_API_KEY` / `HF_API_KEY`. Useful when you already have
    /// a provider key in your shell env.
    ///
    /// Returns `None` if neither `CEL_LLM_PROVIDER` nor `CEL_LLM_BASE_URL`
    /// is set.
    pub fn from_env() -> Option<Self> {
        Self::from_env_with_role(LlmRole::General)
    }

    /// Build configuration from environment variables with role-based override.
    ///
    /// Resolution order for each field:
    /// 1. `CEL_LLM_{ROLE}_X` (e.g., `CEL_LLM_PLANNER_PROVIDER`) — power-user
    ///    knob for mixed-provider setups (cheap Gemini for vision + premium
    ///    Sonnet for planner).
    /// 2. `CEL_LLM_X` (base fallback — the common case)
    /// 3. Provider-specific keys (e.g., `ANTHROPIC_API_KEY`) for the API key
    ///    field only.
    ///
    /// Returns `None` if no provider can be resolved (neither
    /// `CEL_LLM_PROVIDER` nor `CEL_LLM_BASE_URL` is set).
    pub fn from_env_with_role(role: LlmRole) -> Option<Self> {
        let prefix = role.env_prefix();

        // Endpoint / base URL: read first, since provider can be auto-detected
        // from it when `CEL_LLM_PROVIDER` is unset. Accept both
        // `CEL_LLM_BASE_URL` (the documented name) and `CEL_LLM_ENDPOINT`
        // (legacy alias). Role-specific overrides take precedence.
        let endpoint = if role != LlmRole::General {
            std::env::var(format!("{prefix}_BASE_URL"))
                .or_else(|_| std::env::var(format!("{prefix}_ENDPOINT")))
                .or_else(|_| std::env::var("CEL_LLM_BASE_URL"))
                .or_else(|_| std::env::var("CEL_LLM_ENDPOINT"))
                .ok()
        } else {
            std::env::var("CEL_LLM_BASE_URL")
                .or_else(|_| std::env::var("CEL_LLM_ENDPOINT"))
                .ok()
        };

        // Provider: explicit `CEL_LLM_PROVIDER` wins; else auto-detect from
        // the base URL; else bail. Lets users get away with just BASE_URL +
        // API_KEY + MODEL, no provider flag.
        let explicit_provider = if role != LlmRole::General {
            std::env::var(format!("{prefix}_PROVIDER"))
                .or_else(|_| std::env::var("CEL_LLM_PROVIDER"))
                .ok()
        } else {
            std::env::var("CEL_LLM_PROVIDER").ok()
        };
        let provider = match (explicit_provider.as_deref(), endpoint.as_deref()) {
            (Some(p), _) => ProviderKind::from(p),
            (None, Some(url)) if !url.is_empty() => ProviderKind::from_base_url(url),
            (None, _) => return None,
        };

        // API key: role-specific → base → provider-specific
        let api_key = if role != LlmRole::General {
            std::env::var(format!("{prefix}_API_KEY"))
                .or_else(|_| std::env::var("CEL_LLM_API_KEY"))
                .ok()
                .or_else(|| Self::provider_specific_key(&provider))
        } else {
            std::env::var("CEL_LLM_API_KEY")
                .ok()
                .or_else(|| Self::provider_specific_key(&provider))
        };

        // Model: role-specific → base
        let model = if role != LlmRole::General {
            std::env::var(format!("{prefix}_MODEL"))
                .or_else(|_| std::env::var("CEL_LLM_MODEL"))
                .ok()
        } else {
            std::env::var("CEL_LLM_MODEL").ok()
        };

        // Escalation model: role-specific → base
        let escalation_model = if role != LlmRole::General {
            std::env::var(format!("{prefix}_ESCALATION_MODEL"))
                .or_else(|_| std::env::var("CEL_LLM_ESCALATION_MODEL"))
                .ok()
        } else {
            std::env::var("CEL_LLM_ESCALATION_MODEL").ok()
        };

        // Temperature: role-specific → base
        let temperature = if role != LlmRole::General {
            std::env::var(format!("{prefix}_TEMPERATURE"))
                .or_else(|_| std::env::var("CEL_LLM_TEMPERATURE"))
                .ok()
        } else {
            std::env::var("CEL_LLM_TEMPERATURE").ok()
        }
        .and_then(|s| s.parse::<f64>().ok());

        tracing::debug!(
            "LLM config for {:?}: provider={}, model={:?}",
            role,
            provider,
            model.as_deref().unwrap_or("(default)"),
        );

        Some(Self {
            provider,
            endpoint,
            api_key,
            model,
            temperature,
            escalation_model,
        })
    }

    /// Look up provider-specific API key env vars. Empty values count as
    /// "not set" so an exported-but-empty `ANTHROPIC_API_KEY` doesn't shadow
    /// `CLAUDE_CODE_OAUTH_TOKEN`.
    fn provider_specific_key(provider: &ProviderKind) -> Option<String> {
        fn nonempty(name: &str) -> Option<String> {
            std::env::var(name).ok().filter(|v| !v.is_empty())
        }
        match provider {
            ProviderKind::OpenAI => nonempty("OPENAI_API_KEY"),
            ProviderKind::Anthropic => nonempty("ANTHROPIC_API_KEY")
                // Claude Code installs an OAuth token under this name. Use it
                // as a fallback so cellar runs out of the box on dev machines
                // with Claude Code already authenticated.
                .or_else(|| nonempty("CLAUDE_CODE_OAUTH_TOKEN")),
            // Google publishes two common env-var spellings for the same
            // key — AI Studio / Google AI snippets use `GOOGLE_GEMINI_API_KEY`
            // while most SDKs default to `GEMINI_API_KEY`. Accept both so
            // existing `.env` files keep working without renaming.
            ProviderKind::Gemini => nonempty("GEMINI_API_KEY")
                .or_else(|| nonempty("GOOGLE_GEMINI_API_KEY"))
                .or_else(|| nonempty("GOOGLE_API_KEY")),
            ProviderKind::HuggingFace => {
                nonempty("HUGGINGFACE_API_KEY").or_else(|| nonempty("HF_API_KEY"))
            }
            ProviderKind::Ollama | ProviderKind::Custom => None,
        }
    }

    /// Load configuration from `~/.cellar/config.toml` (written by `cellar init`).
    ///
    /// Returns `None` if the file is missing, unreadable, or has no `[llm]` section.
    /// Env vars always take precedence — this is only consulted when env is unset.
    pub fn from_config_file() -> Option<Self> {
        let path = config_file_path()?;
        let content = std::fs::read_to_string(&path).ok()?;
        let file: ConfigFile = toml::from_str(&content).ok()?;
        let llm = file.llm?;
        let provider = ProviderKind::from(llm.provider.as_str());
        // `cellar init` writes a config.toml without an `api_key` (the key
        // lives in the environment). Fall back to the provider-specific env
        // var so we don't ship an empty `Authorization: Bearer` header.
        let api_key = llm
            .api_key
            .or_else(|| Self::provider_specific_key(&provider));
        Some(Self {
            provider,
            endpoint: llm.endpoint,
            api_key,
            model: llm.model,
            temperature: llm.temperature,
            escalation_model: llm.escalation_model,
        })
    }

    /// Resolve the endpoint, falling back to provider default.
    pub fn resolved_endpoint(&self) -> &str {
        self.endpoint
            .as_deref()
            .unwrap_or_else(|| self.provider.default_endpoint())
    }

    /// Resolve the model, falling back to provider default.
    pub fn resolved_model(&self) -> &str {
        self.model
            .as_deref()
            .unwrap_or_else(|| self.provider.default_model())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_kind_from_str() {
        assert_eq!(ProviderKind::from("openai"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from("OpenAI"), ProviderKind::OpenAI);
        assert_eq!(ProviderKind::from("gemini"), ProviderKind::Gemini);
        assert_eq!(ProviderKind::from("anthropic"), ProviderKind::Anthropic);
        assert_eq!(ProviderKind::from("claude"), ProviderKind::Anthropic);
        assert_eq!(ProviderKind::from("huggingface"), ProviderKind::HuggingFace);
        assert_eq!(ProviderKind::from("hf"), ProviderKind::HuggingFace);
        assert_eq!(ProviderKind::from("ollama"), ProviderKind::Ollama);
    }

    #[test]
    fn test_provider_defaults() {
        assert!(!ProviderKind::OpenAI.default_endpoint().is_empty());
        assert!(!ProviderKind::OpenAI.default_model().is_empty());
        assert!(ProviderKind::Custom.default_endpoint().is_empty());
    }

    #[test]
    fn test_ollama_defaults() {
        assert_eq!(
            ProviderKind::Ollama.default_endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(ProviderKind::Ollama.default_model(), "gemma4:e4b");
        assert_eq!(ProviderKind::Ollama.to_string(), "ollama");
    }

    #[test]
    fn test_config_file_parse() {
        let content = r#"
[llm]
provider = "ollama"
model = "gemma4:e4b"
"#;
        let file: ConfigFile = toml::from_str(content).unwrap();
        let llm = file.llm.unwrap();
        assert_eq!(llm.provider, "ollama");
        assert_eq!(llm.model.as_deref(), Some("gemma4:e4b"));
        assert!(llm.api_key.is_none());
    }

    #[test]
    fn test_config_file_with_api_key() {
        let content = r#"
[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-xxx"
temperature = 0.2
"#;
        let file: ConfigFile = toml::from_str(content).unwrap();
        let llm = file.llm.unwrap();
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.api_key.as_deref(), Some("sk-ant-xxx"));
        assert_eq!(llm.temperature, Some(0.2));
    }

    #[test]
    fn test_from_config_file_ollama_roundtrip() {
        // Real file round-trip: set HOME to a tempdir, write a config.toml with
        // `provider = "ollama"`, call from_config_file(), assert we get back an
        // Ollama config with the expected defaults.
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_home = std::env::var("HOME").ok();

        let tmp = std::env::temp_dir().join(format!("cel-llm-config-test-{}", std::process::id()));
        let cellar_dir = tmp.join(".cellar");
        std::fs::create_dir_all(&cellar_dir).unwrap();
        std::fs::write(
            cellar_dir.join("config.toml"),
            r#"[llm]
provider = "ollama"
model = "gemma4:e4b"
"#,
        )
        .unwrap();

        std::env::set_var("HOME", &tmp);
        let config = LlmProviderConfig::from_config_file().expect("should load config");

        // Restore env before asserting so a panic doesn't poison other tests.
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(config.provider, ProviderKind::Ollama);
        assert_eq!(config.model.as_deref(), Some("gemma4:e4b"));
        assert_eq!(
            config.resolved_endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
        assert!(config.api_key.is_none());
    }

    #[test]
    fn test_from_config_file_falls_back_to_provider_specific_env() {
        // Regression: with the default `cellar init` config.toml (provider
        // only, no api_key) and `ANTHROPIC_API_KEY` exported, the loaded
        // config must carry the env-var key — otherwise requests go out with
        // an empty bearer token and the server returns HTTP 401.
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_home = std::env::var("HOME").ok();
        let prev_anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
        let prev_oauth = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok();

        let tmp = std::env::temp_dir().join(format!(
            "cel-llm-config-fallback-test-{}",
            std::process::id()
        ));
        let cellar_dir = tmp.join(".cellar");
        std::fs::create_dir_all(&cellar_dir).unwrap();
        std::fs::write(
            cellar_dir.join("config.toml"),
            r#"[llm]
provider = "anthropic"
"#,
        )
        .unwrap();

        std::env::set_var("HOME", &tmp);
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-fallback");
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        let config = LlmProviderConfig::from_config_file().expect("should load config");

        // Restore env before asserting so a panic doesn't poison other tests.
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match prev_anthropic {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match prev_oauth {
            Some(v) => std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", v),
            None => std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN"),
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(config.provider, ProviderKind::Anthropic);
        assert_eq!(config.api_key.as_deref(), Some("sk-ant-test-fallback"));
    }

    #[test]
    fn test_from_config_file_missing() {
        // No config file → None, does not panic.
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_home = std::env::var("HOME").ok();

        let tmp = std::env::temp_dir().join(format!(
            "cel-llm-missing-config-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("HOME", &tmp);

        let result = LlmProviderConfig::from_config_file();

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(result.is_none());
    }

    // Env-var tests must be serialized — they share global process state.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_from_env_not_set() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CEL_LLM_PROVIDER");
        assert!(LlmProviderConfig::from_env().is_none());
    }

    #[test]
    fn test_from_env_basic() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CEL_LLM_PROVIDER", "openai");
        std::env::set_var("CEL_LLM_API_KEY", "sk-env-test");
        std::env::set_var("CEL_LLM_MODEL", "gpt-4o-mini");
        std::env::remove_var("CEL_LLM_ENDPOINT");

        let config = LlmProviderConfig::from_env().unwrap();
        assert_eq!(config.provider, ProviderKind::OpenAI);
        assert_eq!(config.api_key.as_deref(), Some("sk-env-test"));
        assert_eq!(config.model.as_deref(), Some("gpt-4o-mini"));
        assert!(config.endpoint.is_none());

        std::env::remove_var("CEL_LLM_PROVIDER");
        std::env::remove_var("CEL_LLM_API_KEY");
        std::env::remove_var("CEL_LLM_MODEL");
    }

    #[test]
    fn test_from_env_provider_specific_key() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CEL_LLM_PROVIDER", "anthropic");
        std::env::remove_var("CEL_LLM_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-fallback");

        let config = LlmProviderConfig::from_env().unwrap();
        assert_eq!(config.provider, ProviderKind::Anthropic);
        assert_eq!(config.api_key.as_deref(), Some("sk-ant-fallback"));

        std::env::remove_var("CEL_LLM_PROVIDER");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn test_model_tier_flash() {
        assert_eq!(
            ModelProfile::from_model_id("gemini-2.0-flash").tier,
            ModelTier::Flash
        );
        assert_eq!(
            ModelProfile::from_model_id("gpt-4o-mini").tier,
            ModelTier::Flash
        );
        assert_eq!(
            ModelProfile::from_model_id("claude-haiku-4-5").tier,
            ModelTier::Flash
        );
    }

    #[test]
    fn test_model_tier_standard() {
        assert_eq!(
            ModelProfile::from_model_id("gpt-4o").tier,
            ModelTier::Standard
        );
        assert_eq!(
            ModelProfile::from_model_id("claude-sonnet-4-20250514").tier,
            ModelTier::Standard
        );
    }

    #[test]
    fn test_model_tier_premium() {
        assert_eq!(
            ModelProfile::from_model_id("claude-opus-4-6").tier,
            ModelTier::Premium
        );
        assert_eq!(ModelProfile::from_model_id("o3").tier, ModelTier::Premium);
        assert_eq!(
            ModelProfile::from_model_id("gpt-5").tier,
            ModelTier::Premium
        );
    }

    #[test]
    fn test_model_profile_provider_detection() {
        assert_eq!(
            ModelProfile::from_model_id("claude-sonnet-4").provider,
            ProviderKind::Anthropic
        );
        assert_eq!(
            ModelProfile::from_model_id("gpt-4o").provider,
            ProviderKind::OpenAI
        );
        assert_eq!(
            ModelProfile::from_model_id("gemini-2.0-flash").provider,
            ProviderKind::Gemini
        );
        assert_eq!(
            ModelProfile::from_model_id("llama-3").provider,
            ProviderKind::Custom
        );
    }

    #[test]
    fn test_config_resolved() {
        let config = LlmProviderConfig {
            provider: ProviderKind::OpenAI,
            endpoint: None,
            api_key: Some("sk-test".into()),
            model: None,
            temperature: None,
            escalation_model: None,
        };
        assert_eq!(
            config.resolved_endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(config.resolved_model(), "gpt-4o");

        let custom = LlmProviderConfig {
            provider: ProviderKind::OpenAI,
            endpoint: Some("http://localhost:8080/v1/chat/completions".into()),
            api_key: None,
            model: Some("local-model".into()),
            temperature: None,
            escalation_model: None,
        };
        assert_eq!(
            custom.resolved_endpoint(),
            "http://localhost:8080/v1/chat/completions"
        );
        assert_eq!(custom.resolved_model(), "local-model");
    }
}
