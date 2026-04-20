//! Common Adapter Interface
//!
//! Defines the trait that all app-specific adapters implement.
//! Adapters provide native API access to specific applications,
//! giving the highest confidence context and most reliable actions.
//!
//! License: MIT (adapters are MIT-licensed to encourage community contributions)

use async_trait::async_trait;
use cel_context::ContextElement;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("App not running or not found")]
    AppNotFound,
    #[error("Adapter not available: {0}")]
    Unavailable(String),
    #[error("Operation failed: {0}")]
    OperationFailed(String),
    #[error("Connection lost")]
    ConnectionLost,
}

/// Metadata about an adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterInfo {
    /// Adapter name (e.g., "excel", "sap-gui").
    pub name: String,
    /// Display name (e.g., "Microsoft Excel").
    pub display_name: String,
    /// Supported app version range.
    pub supported_versions: String,
    /// Platform support.
    pub platforms: Vec<String>,
}

/// The core adapter trait that all app-specific adapters implement.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Get adapter metadata.
    fn info(&self) -> AdapterInfo;

    /// Check if the target app is running and accessible.
    async fn is_available(&self) -> bool;

    /// Connect to the target app's native API.
    async fn connect(&mut self) -> Result<(), AdapterError>;

    /// Disconnect from the target app.
    async fn disconnect(&mut self) -> Result<(), AdapterError>;

    /// Get context elements from the app's native API.
    /// These elements have the highest confidence (0.95+).
    async fn get_elements(&self) -> Result<Vec<ContextElement>, AdapterError>;

    /// Execute a named action on the app.
    async fn execute_action(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_info_serialization() {
        let info = AdapterInfo {
            name: "test".into(),
            display_name: "Test Adapter".into(),
            supported_versions: "1.0+".into(),
            platforms: vec!["windows".into(), "macos".into()],
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: AdapterInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "test");
        assert_eq!(back.display_name, "Test Adapter");
        assert_eq!(back.platforms.len(), 2);
    }

    #[test]
    fn test_adapter_error_display() {
        assert_eq!(AdapterError::AppNotFound.to_string(), "App not running or not found");
        assert_eq!(AdapterError::ConnectionLost.to_string(), "Connection lost");
        assert_eq!(
            AdapterError::Unavailable("no COM".into()).to_string(),
            "Adapter not available: no COM"
        );
        assert_eq!(
            AdapterError::OperationFailed("bad action".into()).to_string(),
            "Operation failed: bad action"
        );
    }

    #[test]
    fn test_adapter_info_clone() {
        let info = AdapterInfo {
            name: "clone-test".into(),
            display_name: "Clone Test".into(),
            supported_versions: "any".into(),
            platforms: vec!["linux".into()],
        };
        let cloned = info.clone();
        assert_eq!(info.name, cloned.name);
        assert_eq!(info.platforms, cloned.platforms);
    }
}
