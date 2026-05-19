//! Cellar v1 daemon — Phase 0 + Phase 1 (in progress) skeleton.
//!
//! The `Daemon` struct holds every subsystem the daemon owns. Currently wired:
//!
//! - [`MemoryProvider`] backed by [`BasicMemoryProvider`] (Phase 0)
//! - [`Gateway`] backed by the recording test-support implementations of
//!   actuator and broker, with an empty rule set (Phase 1 — real actuator
//!   and IPC-backed broker land alongside the event sources and MCP server).
//! - IPC [`StubHandler`] — answers `system.hello` / `system.shutdown` /
//!   `daemon.status` for real, returns `NotImplemented` for every other
//!   method. The locked protocol surface from
//!   [`cellar-ipc-protocol.md`] is honored from day one.
//!
//! Still to come in later Phase 1 work: event bus, Cortex goalless mode,
//! process poller, FSEvents adapter, webhook sender, MCP server.
//! Phase 2+ adds the rule matcher's storage layer, the NL compiler, and the
//! embedded agent runtime — each plugs into the locked IPC handler by
//! overriding the corresponding methods on a richer handler type.
//!
//! See `/Users/dimitriospagkratis/.claude/plans/cellar-app-v1.md` for the full
//! architecture. The locked memory trait surface is in
//! `/Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md` §12.
//!
//! [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md
//! [`StubHandler`]: cellar_ipc::StubHandler

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

use std::sync::Arc;

use cel_act_gateway::test_support::{AutoAllowBroker, RecordingActuator};
use cel_act_gateway::traits::StaticRules;
use cel_act_gateway::Gateway;
use cel_memory::{BasicMemoryProvider, MemoryProvider};
use cellar_ipc::StubHandler;
use cellar_types::InMemoryWatchlists;

/// Concrete `Gateway` type the v1 Phase 0/1 skeleton holds. The Memory & Context
/// Manager doesn't touch this; later phases swap the actuator and broker.
pub type DaemonGateway =
    Gateway<RecordingActuator, AutoAllowBroker, StaticRules, InMemoryWatchlists>;

/// The Cellar daemon. Owns all subsystems; constructed once at startup via
/// [`Daemon::wire_subsystems`].
///
/// The struct is intentionally a flat list of `Arc<...>` (or owned values
/// where lifetime allows). New subsystems land here as their phases ship.
pub struct Daemon {
    /// Memory subsystem. v1 backs this with [`BasicMemoryProvider`]; the full
    /// Memory & Context Manager drops in by changing the construction site
    /// below — no other callsite changes.
    pub memory: Arc<dyn MemoryProvider>,

    /// The `cel_act` gateway — every actuation call from every caller
    /// flows through this. Phase 1 wires the recording actuator and
    /// auto-allow broker; Phase 1.x replaces both with real implementations.
    pub gateway: Arc<DaemonGateway>,

    /// IPC handler. Implements the locked protocol surface from
    /// [`cellar-ipc-protocol.md`]; v1 backs only the system + status
    /// methods with real bodies. Subsystem owners override more methods
    /// on a richer handler type as their phases ship.
    ///
    /// [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md
    pub ipc_handler: Arc<StubHandler>,
}

impl Daemon {
    /// Wire every subsystem the daemon depends on and return a ready-to-use
    /// `Daemon`. Pure construction — no I/O, no spawned tasks. The caller
    /// (typically `main` or an integration test) decides what to do with the
    /// `Daemon` afterward.
    ///
    /// **Memory subsystem swap point:** in v1, `memory` is a
    /// [`BasicMemoryProvider`]. When the full Memory & Context Manager
    /// (`cel-memory-sqlite` crate, separate plan) ships, this is the single
    /// site where the swap happens. The rest of the daemon depends on
    /// `Arc<dyn MemoryProvider>` only; no callsite churn.
    ///
    /// **Gateway swap points:** the actuator (currently `RecordingActuator`)
    /// becomes the real `cel_act` executor in Phase 1.x; the broker
    /// (`AutoAllowBroker`) becomes an IPC-backed broker pushing
    /// confirmations to the Tauri app in Phase 3. The rule source
    /// (`StaticRules(vec![])`) becomes SQLite-backed with hot-reload in
    /// Phase 2.
    ///
    /// **IPC handler swap point:** Phase 2 onward replaces [`StubHandler`]
    /// with a richer handler that holds references to the rules store,
    /// gateway, agent runtime, etc. and overrides the trait methods to
    /// dispatch into those subsystems. The IPC protocol surface itself
    /// (every method name, every typed param/result) is locked.
    pub fn wire_subsystems() -> Self {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());

        let gateway = Arc::new(Gateway::new(
            RecordingActuator::new(),
            AutoAllowBroker,
            StaticRules(Vec::new()),
            InMemoryWatchlists::default(),
            memory.clone(),
        ));

        let ipc_handler = Arc::new(StubHandler::new(env!("CARGO_PKG_VERSION")));

        Self {
            memory,
            gateway,
            ipc_handler,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_memory::{
        CallerScope, ChunkKind, ChunkSource, MemoryQuery, NewMemoryChunk, RetrievalProfile,
    };

    fn nc(caller: &str, content: &str) -> NewMemoryChunk {
        NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: caller.into(),
            content: content.into(),
            metadata: serde_json::Value::Null,
            importance: None,
            shareable: false,
            pinned: false,
        }
    }

    #[tokio::test]
    async fn wire_subsystems_returns_usable_memory_provider() {
        let daemon = Daemon::wire_subsystems();
        // The Arc<dyn MemoryProvider> is the locked trait. Every v1 caller
        // (embedded agent, NL compiler, gateway, matcher hook) holds a clone
        // of this Arc; this test stands in for any of them.
        let stats = daemon.memory.stats().await.unwrap();
        assert_eq!(stats.total_chunks, 0);
    }

    #[tokio::test]
    async fn agent_callsite_pattern_compiles_against_locked_trait() {
        // This test exercises the exact callsite pattern the embedded agent
        // runtime (v1 Phase 3) will use: open a session, write a chat chunk,
        // retrieve via the AgentChatTurn profile. The point is to fail at
        // compile time if any of these shapes drift from the locked surface.
        let daemon = Daemon::wire_subsystems();

        let session = daemon
            .memory
            .open_session(cel_memory::NewMemorySession {
                caller_id: "embedded".into(),
                title: Some("smoke".into()),
                metadata: serde_json::Value::Null,
            })
            .await
            .unwrap();

        let mut chunk = nc("embedded", "user asked about the Q4 report");
        chunk.session_id = Some(session.id.clone());
        daemon.memory.write(chunk).await.unwrap();

        let hits = daemon
            .memory
            .retrieve(MemoryQuery {
                text: "q4 report".into(),
                kinds: None,
                since: None,
                until: None,
                session_id: None,
                caller_scope: CallerScope::Own,
                project_root_prefix: None,
                k: 8,
                include_rollups: true,
                min_importance: None,
                profile: RetrievalProfile::AgentChatTurn,
                caller_id: "embedded".into(),
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        daemon
            .memory
            .close_session(&session.id, cel_memory::SessionOutcome::Success)
            .await
            .unwrap();
    }
}
