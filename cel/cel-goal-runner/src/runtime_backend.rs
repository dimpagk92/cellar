//! Runtime backend — where goal execution actually happens.
//!
//! Orthogonal to [`GoalConfig`] (which describes *what* to execute).
//! `RuntimeBackend` describes *where*: the current process (`Local`) or a
//! remote `cellar-worker` reachable over HTTP (`Remote`).
//!
//! This module only exposes the configuration types. Actual dispatch lives
//! in the caller — `cel-napi`, CLI commands, MCP server tool handlers — each
//! branches on the resolved backend and picks the appropriate execution path.
//! See `cellar-worker` for the HTTP client used by the Remote path.

use serde::{Deserialize, Serialize};

/// Which execution backend a goal run should target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RuntimeBackend {
    /// Run in the current process (today's default).
    Local,
    /// Send goals to a remote `cellar-worker` over HTTP.
    Remote {
        /// Base URL, e.g. `http://my-server:7777`.
        url: String,
        /// Optional bearer token. `None` = unauthenticated (localhost / trusted network).
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
}

impl Default for RuntimeBackend {
    fn default() -> Self {
        Self::Local
    }
}

impl RuntimeBackend {
    /// True if this backend executes outside the current process.
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    /// Base URL for the Remote backend, or `None` for Local.
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Remote { url, .. } => Some(url),
            Self::Local => None,
        }
    }

    /// Bearer token for the Remote backend.
    pub fn token(&self) -> Option<&str> {
        match self {
            Self::Remote { token, .. } => token.as_deref(),
            Self::Local => None,
        }
    }
}

/// Resolve the runtime backend from env vars, falling back to `~/.cellar/config.toml`.
///
/// Resolution order (same pattern as `LlmProviderConfig`):
/// 1. `CEL_RUNTIME_BACKEND` — `local` or `remote`
/// 2. `CEL_RUNTIME_URL` — required when backend is `remote`
/// 3. `CEL_RUNTIME_TOKEN` — optional bearer token for `remote`
/// 4. `[runtime]` section of `~/.cellar/config.toml`
/// 5. Default: `Local`
pub fn resolve_runtime_backend() -> RuntimeBackend {
    if let Some(backend) = from_env() {
        return backend;
    }
    if let Some(backend) = from_config_file() {
        return backend;
    }
    RuntimeBackend::Local
}

fn from_env() -> Option<RuntimeBackend> {
    let kind = std::env::var("CEL_RUNTIME_BACKEND").ok()?;
    match kind.to_lowercase().as_str() {
        "local" => Some(RuntimeBackend::Local),
        "remote" => {
            let url = std::env::var("CEL_RUNTIME_URL").ok()?;
            let token = std::env::var("CEL_RUNTIME_TOKEN").ok();
            Some(RuntimeBackend::Remote { url, token })
        }
        other => {
            tracing::warn!("unknown CEL_RUNTIME_BACKEND={} — falling back", other);
            None
        }
    }
}

fn from_config_file() -> Option<RuntimeBackend> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::PathBuf::from(home).join(".cellar").join("config.toml");
    let content = std::fs::read_to_string(&path).ok()?;
    let file: ConfigFile = toml::from_str(&content).ok()?;
    let runtime = file.runtime?;
    match runtime.backend.to_lowercase().as_str() {
        "local" => Some(RuntimeBackend::Local),
        "remote" => {
            let url = runtime.url?;
            Some(RuntimeBackend::Remote {
                url,
                token: runtime.token,
            })
        }
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    runtime: Option<ConfigFileRuntime>,
}

#[derive(Debug, Deserialize)]
struct ConfigFileRuntime {
    backend: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests share global process state; serialize with a mutex.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_default_is_local() {
        assert_eq!(RuntimeBackend::default(), RuntimeBackend::Local);
        assert!(!RuntimeBackend::default().is_remote());
        assert!(RuntimeBackend::default().url().is_none());
    }

    #[test]
    fn test_remote_accessors() {
        let backend = RuntimeBackend::Remote {
            url: "http://localhost:7777".into(),
            token: Some("secret".into()),
        };
        assert!(backend.is_remote());
        assert_eq!(backend.url(), Some("http://localhost:7777"));
        assert_eq!(backend.token(), Some("secret"));
    }

    #[test]
    fn test_env_local() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CEL_RUNTIME_BACKEND", "local");
        std::env::remove_var("CEL_RUNTIME_URL");
        let backend = from_env().unwrap();
        assert_eq!(backend, RuntimeBackend::Local);
        std::env::remove_var("CEL_RUNTIME_BACKEND");
    }

    #[test]
    fn test_env_remote() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CEL_RUNTIME_BACKEND", "remote");
        std::env::set_var("CEL_RUNTIME_URL", "http://worker:7777");
        std::env::set_var("CEL_RUNTIME_TOKEN", "tok123");

        let backend = from_env().unwrap();
        assert_eq!(
            backend,
            RuntimeBackend::Remote {
                url: "http://worker:7777".into(),
                token: Some("tok123".into()),
            }
        );

        std::env::remove_var("CEL_RUNTIME_BACKEND");
        std::env::remove_var("CEL_RUNTIME_URL");
        std::env::remove_var("CEL_RUNTIME_TOKEN");
    }

    #[test]
    fn test_env_remote_missing_url() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CEL_RUNTIME_BACKEND", "remote");
        std::env::remove_var("CEL_RUNTIME_URL");
        // Missing URL → from_env returns None → caller falls back
        assert!(from_env().is_none());
        std::env::remove_var("CEL_RUNTIME_BACKEND");
    }

    #[test]
    fn test_config_file_parse_remote() {
        let content = r#"
[runtime]
backend = "remote"
url = "http://worker.local:7777"
token = "abc"
"#;
        let file: ConfigFile = toml::from_str(content).unwrap();
        let rt = file.runtime.unwrap();
        assert_eq!(rt.backend, "remote");
        assert_eq!(rt.url.as_deref(), Some("http://worker.local:7777"));
        assert_eq!(rt.token.as_deref(), Some("abc"));
    }

    #[test]
    fn test_config_file_parse_local() {
        let content = r#"
[runtime]
backend = "local"
"#;
        let file: ConfigFile = toml::from_str(content).unwrap();
        let rt = file.runtime.unwrap();
        assert_eq!(rt.backend, "local");
        assert!(rt.url.is_none());
    }
}
