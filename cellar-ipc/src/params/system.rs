//! `system.*` request parameters.

use serde::{Deserialize, Serialize};

/// Params for `system.hello`. The required first call after connecting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemHelloParams {
    /// Client name, e.g. `"cellar-tauri"`, `"cellar-cli"`.
    pub client_name: String,
    /// Client version (semver string).
    pub client_version: String,
    /// Protocol versions the client knows how to speak. The daemon picks
    /// the highest overlap or returns `-32001 unsupported_protocol_version`.
    pub supported_protocol_versions: Vec<String>,
}

/// Params for `system.shutdown`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemShutdownParams {
    /// Maximum seconds the daemon waits for in-flight work (agent loops,
    /// webhook retries) before exiting. Default 30.
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout_s: u64,
}

fn default_drain_timeout() -> u64 {
    30
}
