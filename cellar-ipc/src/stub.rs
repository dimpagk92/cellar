//! `StubHandler` — the v1 Phase 1 daemon's IPC handler.
//!
//! Implements the methods whose backing subsystems exist (`system.hello`,
//! `system.shutdown` flag, `daemon.status` minimal fields) and relies on
//! the [`crate::Handler`] trait's default `NotImplemented` for everything
//! else. Real subsystems land incrementally and override the corresponding
//! methods.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::{IpcError, IpcResult};
use crate::handler::Handler;
use crate::params::system::{SystemHelloParams, SystemShutdownParams};
use crate::results::daemon::{DaemonStatusResult, RuleStats, WatchlistStats};
use crate::results::system::{SystemHelloResult, SystemShutdownResult};

/// Protocol versions this stub supports. The RFC pins us at `"1"` for v1.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["1"];

/// Capabilities the v1 Phase 1 daemon advertises via `system.hello`.
pub const V1_PHASE1_CAPABILITIES: &[&str] = &[
    // The locked memory trait is wired with the basic in-memory provider.
    "memory.basic",
    // The gateway exists and intercepts every cel_act call.
    "gateway",
];

/// A minimal handler with just enough real behaviour to support
/// connection negotiation and a `daemon.status` query. Everything else
/// returns `IpcError::NotImplemented`.
pub struct StubHandler {
    daemon_version: String,
    started_at: Instant,
    shutting_down: AtomicBool,
    open_subscriptions: AtomicU64,
}

impl StubHandler {
    /// Build a stub handler. Caller supplies the daemon's own version string.
    pub fn new(daemon_version: impl Into<String>) -> Self {
        Self {
            daemon_version: daemon_version.into(),
            started_at: Instant::now(),
            shutting_down: AtomicBool::new(false),
            open_subscriptions: AtomicU64::new(0),
        }
    }

    fn uptime_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

impl Default for StubHandler {
    fn default() -> Self {
        Self::new(env!("CARGO_PKG_VERSION"))
    }
}

#[async_trait]
impl Handler for StubHandler {
    async fn system_hello(&self, params: SystemHelloParams) -> IpcResult<SystemHelloResult> {
        // Pick the highest mutually-supported protocol version.
        let chosen = SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|v| params.supported_protocol_versions.iter().any(|c| c == *v))
            .copied();
        let Some(version) = chosen else {
            return Err(IpcError::UnsupportedProtocolVersion(
                params.supported_protocol_versions,
            ));
        };
        Ok(SystemHelloResult {
            protocol_version: version.to_string(),
            daemon_version: self.daemon_version.clone(),
            daemon_uptime_s: self.uptime_s(),
            session_id: format!("ses_{}", Uuid::now_v7()),
            capabilities: V1_PHASE1_CAPABILITIES
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
    }

    async fn system_shutdown(
        &self,
        _params: SystemShutdownParams,
    ) -> IpcResult<SystemShutdownResult> {
        self.shutting_down.store(true, Ordering::SeqCst);
        Ok(SystemShutdownResult {
            shutting_down: true,
        })
    }

    async fn daemon_status(&self) -> IpcResult<DaemonStatusResult> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(IpcError::ShuttingDown);
        }
        Ok(DaemonStatusResult {
            healthy: true,
            uptime_s: self.uptime_s(),
            rules: RuleStats {
                total: 0,
                enabled: 0,
            },
            watchlists: WatchlistStats { total: 0 },
            recent_fires_24h: 0,
            pending_confirmations: 0,
            agent_sessions_active: 0,
            daemon_version: self.daemon_version.clone(),
            memory_mb: 0.0,
            cpu_pct: 0.0,
        })
    }

    async fn on_stream(
        &self,
        _stream: crate::subscription::StreamName,
        _id: &crate::subscription::SubscriptionId,
        attaching: bool,
    ) {
        if attaching {
            self.open_subscriptions.fetch_add(1, Ordering::Relaxed);
        } else {
            self.open_subscriptions
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(1))
                })
                .ok();
        }
    }
}
