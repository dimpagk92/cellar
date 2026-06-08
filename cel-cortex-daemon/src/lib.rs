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

pub mod agent_action_bus;
pub mod agent_runtime;
pub mod bus;
pub mod chat_bus;
pub mod confirmation;
pub mod fire_bus;
pub mod fsevents;
pub mod ipc;
pub mod matcher_task;
pub mod memory_offdevice_governance;
pub mod memory_write_governance;
pub mod process_poller;
pub mod recent;
pub mod signals_poller;
pub mod subscriptions;
pub mod sweeper;

use std::path::Path;
use std::sync::Arc;

use cel_act_gateway::test_support::RecordingActuator;
use cel_act_gateway::{
    AgentActionHook, AgentGateway, CooldownPersistence, CooldownTracker, Gateway, WebhookHook,
};
use cel_memory::{BasicMemoryProvider, MemoryProvider};
use cellar_llm_router::Router;
use cellar_rule_compiler::Compiler;
use cellar_rules_store::{RulesStoreError, SqliteRulesStore};
use cellar_types::Event;
use cellar_webhook::{
    GatewayHook, ReqwestSender, Sender, WebhookRegistry, WebhookSecret, WebhookService,
    WebhookServiceConfig,
};

use crate::agent_action_bus::{AgentActionBus, AgentActionRing, DaemonAgentActionHook};
use crate::agent_runtime::AgentRuntime;
use crate::bus::EventBus;
use crate::chat_bus::ChatBus;
use crate::confirmation::{ConfirmationBus, IpcConfirmationBroker};
use crate::fire_bus::{FireBus, FireFrame};
use crate::ipc::DaemonIpcHandler;
use crate::recent::Ring;
use crate::subscriptions::SubscriptionRegistry;

/// Concrete `Gateway` type the v1 daemon holds. Both rule source and
/// watchlist lookup are served by the same `Arc<SqliteRulesStore>`; the
/// gateway and the matcher consumer task get their own independent
/// clones so writes through one are visible to the other (see
/// `cellar-rules-store/tests/hot_reload.rs`).
///
/// The broker slot is `Arc<IpcConfirmationBroker>` — the same broker the
/// IPC handler holds a clone of for `confirmation.resolve`.
pub type DaemonGateway = Gateway<
    RecordingActuator,
    Arc<IpcConfirmationBroker>,
    Arc<SqliteRulesStore>,
    Arc<SqliteRulesStore>,
>;

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

    /// SQLite-backed rules + watchlists store, shared with the gateway and
    /// the matcher consumer task. Mutations through this handle are
    /// immediately visible to both via the `RuleSource` / `WatchlistLookup`
    /// blanket impls on `Arc<T>`.
    pub rules_store: Arc<SqliteRulesStore>,

    /// Per-rule cooldown tracker, shared with the gateway and the matcher
    /// consumer task so a fire through either path counts against the
    /// same window. See [`CooldownTracker`].
    pub cooldown: Arc<CooldownTracker>,

    /// Webhook fan-out hook. Attached to the `cel_act` gateway and the
    /// matcher consumer task so a fired `watcher` rule with
    /// `action.type = Webhook` delivers via the same retry queue regardless
    /// of which path matched it. The service is always started (even with
    /// zero webhooks at startup); newly-added webhooks are registered live
    /// via [`Daemon::webhook_registry`] without a daemon restart.
    pub webhook_hook: Option<Arc<dyn WebhookHook>>,

    /// Live registry for the webhook service. `webhooks.add` and
    /// `webhooks.remove` IPC calls register/unregister via this handle so the
    /// running service picks up changes without a daemon restart. Always `Some`
    /// when the webhook service is running.
    pub webhook_registry: Option<Arc<dyn WebhookRegistry>>,

    /// Broadcast bus for ambient events from process poller / fsevents /
    /// signals poller. Cloned into the matcher consumer task (read), the
    /// IPC `events.subscribe` forwarders (read), and a ring-filler task
    /// (read into `event_ring`). main.rs spawns the ambient sources to
    /// publish into it.
    pub event_bus: EventBus,

    /// Broadcast bus for fires emitted by the matcher consumer task.
    /// IPC `fires.subscribe` forwarders subscribe to this; the
    /// ring-filler task also writes into `fire_ring`.
    pub fire_bus: FireBus,

    /// Bounded ring of recent events for `events.recent` backfill.
    /// Populated by a small daemon-spawned task that drains the bus.
    pub event_ring: Arc<Ring<Event>>,

    /// Bounded ring of recent fires for `fires.recent` backfill.
    pub fire_ring: Arc<Ring<FireFrame>>,

    /// Registry of live `events.subscribe` / `fires.subscribe` forwarder
    /// tasks. The IPC handler registers each subscribe call here so
    /// unsubscribe / disconnect can abort the per-subscription task.
    pub subscription_registry: Arc<SubscriptionRegistry>,

    /// IPC-backed confirmation broker. Same Arc the gateway holds —
    /// `confirmation.resolve` IPC calls reach the gateway's pending
    /// `request_confirmation` await via this shared registry.
    pub confirmation_broker: Arc<IpcConfirmationBroker>,

    /// Broadcast bus for `PendingConfirmation` frames; the IPC handler's
    /// `confirmation.subscribe` forwarder subscribes to this.
    pub confirmation_bus: ConfirmationBus,

    /// Embedded agent runtime. `None` when no LLM provider is configured
    /// at startup (same gate as `compiler`); `agent.message` returns
    /// `LlmProviderError` then. `agent.sessions.*` works regardless.
    pub agent_runtime: Option<Arc<AgentRuntime>>,

    /// Broadcast bus for `agent.chat.*` frames. The IPC handler's
    /// `agent.chat.subscribe` forwarder filters by session_id.
    pub chat_bus: ChatBus,

    /// Broadcast bus for `agent_actions.*` frames. Published by the gateway
    /// via `DaemonAgentActionHook` after every `intercept()` call.
    pub agent_action_bus: AgentActionBus,

    /// Bounded ring of recent agent-action frames for `agent_actions.recent`.
    pub agent_action_ring: Arc<AgentActionRing>,

    /// IPC handler. Implements the locked protocol surface from
    /// [`cellar-ipc-protocol.md`]; v1 backs `system.*`, `daemon.status`,
    /// and the full `rules.*` / `watchlists.*` CRUD surface with real
    /// bodies. Everything else returns
    /// [`cellar_ipc::IpcError::NotImplemented`] via the trait's default
    /// methods. Subsystem owners override more methods on this handler
    /// as their phases ship.
    ///
    /// [`cellar-ipc-protocol.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-ipc-protocol.md
    pub ipc_handler: Arc<DaemonIpcHandler>,
}

impl Daemon {
    /// Wire every subsystem with an **in-memory** rules store. Infallible.
    /// Used by integration tests and any consumer that doesn't need
    /// rule persistence across process restarts.
    ///
    /// For the production daemon, use [`Self::wire_subsystems_with_db`].
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
    /// confirmations to the Tauri app in Phase 3.
    ///
    /// **IPC handler swap point:** Phase 2 onward replaces [`StubHandler`]
    /// with a richer handler that holds references to the rules store,
    /// gateway, agent runtime, etc. and overrides the trait methods to
    /// dispatch into those subsystems. The IPC protocol surface itself
    /// (every method name, every typed param/result) is locked.
    pub fn wire_subsystems() -> Self {
        let rules_store = SqliteRulesStore::in_memory().expect("in-memory SQLite open cannot fail");
        Self::wire_with_store(rules_store)
    }

    /// Wire every subsystem with a **file-backed** rules store at `path`.
    /// Creates any missing parent directories so a fresh install with no
    /// `~/.cellar` directory just works.
    ///
    /// Attempts to build the NL rule compiler from environment variables
    /// (`CELLAR_DEFAULT_PROVIDER` + `CELLAR_DEFAULT_MODEL`, or the
    /// `CELLAR_NL_COMPILER_*` override set). On failure (no provider
    /// configured, missing API key, unsupported provider name) the daemon
    /// still boots — `rules.compile` returns `LlmProviderError` and the
    /// `system.hello` capabilities omit `rules.compile`.
    pub fn wire_subsystems_with_db(path: impl AsRef<Path>) -> Result<Self, RulesStoreError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let rules_store = SqliteRulesStore::open(path)?;
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        let compiler = try_build_nl_compiler(Some(&memory));
        Ok(Self::wire_with_store_compiler_and_memory(
            rules_store,
            compiler,
            memory,
        ))
    }

    /// As [`Self::wire_subsystems_with_db`], but accepts a pre-built memory
    /// provider. The daemon binary uses this to plug in
    /// [`SqliteMemoryProvider`](cel_memory_sqlite::SqliteMemoryProvider)
    /// (asynchronously constructed because the SQLite open + migration
    /// run blocks). Tests that don't care about persistence keep using
    /// [`Self::wire_subsystems_with_db`].
    pub fn wire_subsystems_with_db_and_memory(
        path: impl AsRef<Path>,
        memory: Arc<dyn MemoryProvider>,
    ) -> Result<Self, RulesStoreError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let rules_store = SqliteRulesStore::open(path)?;
        let compiler = try_build_nl_compiler(Some(&memory));
        Ok(Self::wire_with_store_compiler_and_memory(
            rules_store,
            compiler,
            memory,
        ))
    }

    /// As [`Self::wire_subsystems_with_db_and_memory`], but accepts a
    /// **pre-opened** rules store. The daemon binary uses this when the
    /// memory provider needs the rules store *before* the rest of the
    /// daemon wires (e.g., to attach a `MatcherWriteHook` that runs
    /// rule-matcher governance on every memory write).
    pub fn wire_subsystems_with_store_and_memory(
        rules_store: Arc<SqliteRulesStore>,
        memory: Arc<dyn MemoryProvider>,
    ) -> Self {
        let compiler = try_build_nl_compiler(Some(&memory));
        Self::wire_with_store_compiler_and_memory(rules_store, compiler, memory)
    }

    /// Wire every subsystem with the given (pre-built) NL compiler.
    /// Used by integration tests that need to inject a `MockProvider`-backed
    /// compiler without going through env vars.
    pub fn wire_subsystems_with_compiler(compiler: Arc<Compiler>) -> Self {
        let rules_store = SqliteRulesStore::in_memory().expect("in-memory SQLite open cannot fail");
        Self::wire_with_store_and_compiler(rules_store, Some(compiler))
    }

    fn wire_with_store(rules_store: Arc<SqliteRulesStore>) -> Self {
        Self::wire_with_store_and_compiler(rules_store, None)
    }

    fn wire_with_store_and_compiler(
        rules_store: Arc<SqliteRulesStore>,
        compiler: Option<Arc<Compiler>>,
    ) -> Self {
        let memory: Arc<dyn MemoryProvider> = Arc::new(BasicMemoryProvider::new());
        Self::wire_with_store_compiler_and_memory(rules_store, compiler, memory)
    }

    fn wire_with_store_compiler_and_memory(
        rules_store: Arc<SqliteRulesStore>,
        compiler: Option<Arc<Compiler>>,
        memory: Arc<dyn MemoryProvider>,
    ) -> Self {
        // One `Arc<CooldownTracker>` shared by the gateway and the matcher
        // consumer task so per-rule cooldown windows count fires from
        // either path (see `cel_act_gateway::CooldownTracker` docs).
        //
        // Backed by the rules store (`rule_cooldowns` table) so a quick
        // crash-restart can't bypass a long cooldown window — the
        // tracker rehydrates from SQLite on construction and writes
        // through on every successful fire.
        let cooldown_store: Arc<dyn CooldownPersistence> = Arc::clone(&rules_store) as _;
        let cooldown = Arc::new(CooldownTracker::with_store(cooldown_store));

        // Build the webhook service. Always spawns — even with zero webhooks
        // at startup — so hot-reload via `webhooks.add` IPC works without
        // restarting the daemon.
        let (webhook_hook, webhook_registry) = build_webhook_service(&rules_store);
        let webhook_hook = Some(webhook_hook);
        let webhook_registry = Some(webhook_registry);

        // Activity-tab plumbing. The buses are broadcast channels; the
        // rings are bounded backfill buffers populated by a filler task
        // (spawned in main.rs after construction).
        let event_bus = EventBus::with_capacity(4096);
        let fire_bus = FireBus::new();
        let event_ring: Arc<Ring<Event>> = Arc::new(Ring::new());
        let fire_ring: Arc<Ring<FireFrame>> = Arc::new(Ring::new());
        let subscription_registry = Arc::new(SubscriptionRegistry::new());

        // Confirmation broker — the gateway holds an `Arc` clone; the
        // IPC handler's `confirmation.resolve` path holds the same
        // `Arc` so a resolve through IPC unblocks the gateway's pending
        // `request_confirmation` await.
        let confirmation_bus = ConfirmationBus::new();
        let confirmation_broker = Arc::new(IpcConfirmationBroker::new(
            rules_store.clone(),
            confirmation_bus.clone(),
        ));

        // Agent-action bus + ring. Published by the gateway via
        // `DaemonAgentActionHook` after every `intercept()`. The ring is
        // bounded (1024 entries by default) for `agent_actions.recent`
        // backfill. The bus is broadcast for `agent_actions.subscribe`.
        let agent_action_bus = AgentActionBus::new();
        let agent_action_ring: Arc<AgentActionRing> = Arc::new(AgentActionRing::new());
        let action_hook: Arc<dyn AgentActionHook> = Arc::new(DaemonAgentActionHook::new(
            agent_action_bus.clone(),
            agent_action_ring.clone(),
        ));

        // Two independent clones of the same `Arc<SqliteRulesStore>` —
        // one for the gateway's `RuleSource` slot, one for its
        // `WatchlistLookup` slot. The matcher consumer task takes two
        // more clones (`spawn()` call in main.rs).
        let mut gateway_builder = Gateway::new(
            RecordingActuator::new(),
            confirmation_broker.clone(),
            rules_store.clone(),
            rules_store.clone(),
            memory.clone(),
        )
        .with_cooldown(cooldown.clone())
        .with_action_hook(action_hook);
        if let Some(hook) = &webhook_hook {
            gateway_builder = gateway_builder.with_webhook_hook(hook.clone());
        }
        let gateway = Arc::new(gateway_builder);

        // Embedded agent runtime. Built *after* the gateway so we can
        // pass `Arc<dyn AgentGateway>` in directly and enable `cel_act`
        // tool dispatch from the agent loop.  Same env-var resolution as
        // the NL compiler but for the `agent` subsystem. `None` when no
        // provider is configured — `agent.message` returns a clear
        // `LlmProviderError` then.
        let chat_bus = ChatBus::new();
        let agent_runtime = try_build_agent_runtime(
            memory.clone(),
            chat_bus.clone(),
            Some(gateway.clone() as Arc<dyn AgentGateway>),
        );

        let ipc_handler = Arc::new(
            DaemonIpcHandler::with_compiler(
                env!("CARGO_PKG_VERSION"),
                rules_store.clone(),
                compiler,
            )
            .with_streams(
                event_bus.clone(),
                fire_bus.clone(),
                event_ring.clone(),
                fire_ring.clone(),
                agent_action_bus.clone(),
                agent_action_ring.clone(),
                subscription_registry.clone(),
            )
            .with_confirmation(confirmation_broker.clone(), confirmation_bus.clone())
            .with_agent(memory.clone(), agent_runtime.clone(), chat_bus.clone())
            .with_webhook_registry(webhook_registry.clone())
            .with_gateway(gateway.clone() as Arc<dyn AgentGateway>),
        );

        Self {
            memory,
            gateway,
            rules_store,
            cooldown,
            webhook_hook,
            webhook_registry,
            event_bus,
            fire_bus,
            event_ring,
            fire_ring,
            subscription_registry,
            confirmation_broker,
            confirmation_bus,
            agent_runtime,
            chat_bus,
            agent_action_bus,
            agent_action_ring,
            ipc_handler,
        }
    }
}

/// Build the webhook service from the rules store's startup snapshot.
///
/// Always spawns the service worker — even with zero configured webhooks —
/// so hot-reload via [`WebhookRegistry::register_webhook`] works from the
/// first IPC `webhooks.add` call without restarting the daemon.
///
/// Returns `(hook, registry)` where both point at the same underlying service.
/// The hook is attached to the gateway and matcher consumer task; the registry
/// is stored in the IPC handler so `webhooks.add` / `webhooks.remove` can
/// register/unregister live.
///
/// Secret resolution: each webhook's `secret_value_env` is read once from
/// the process env at startup. Missing env vars yield "no secret" — the
/// worker sends without the secret header. Newly added webhooks via IPC
/// also have their secret resolved from the env at add time.
fn build_webhook_service(
    rules_store: &SqliteRulesStore,
) -> (Arc<dyn WebhookHook>, Arc<dyn WebhookRegistry>) {
    let webhooks = rules_store.webhooks_snapshot();
    let mut secrets = std::collections::HashMap::new();
    for (id, cfg) in &webhooks {
        if let (Some(header), Some(env_name)) = (
            cfg.secret_header.as_deref(),
            cfg.secret_value_env.as_deref(),
        ) {
            match std::env::var(env_name) {
                Ok(value) => {
                    secrets.insert(
                        id.clone(),
                        WebhookSecret {
                            header_name: header.to_string(),
                            header_value: value,
                        },
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        webhook_id = %id,
                        env_name = %env_name,
                        "webhook secret env var not set; webhook will deliver without secret header"
                    );
                }
            }
        }
    }
    let service: Arc<WebhookService<ReqwestSender>> = Arc::new(WebhookService::spawn(
        WebhookServiceConfig::default(),
        webhooks.clone(),
        secrets,
        ReqwestSender::new(),
        |result| {
            tracing::debug!(?result, "webhook dispatch result");
        },
    ));
    tracing::info!(
        webhook_count = webhooks.len(),
        "webhook service: started (hot-reload enabled via WebhookRegistry)"
    );
    let hook: Arc<dyn WebhookHook> = Arc::new(GatewayHook::new(service.clone()));
    let registry: Arc<dyn WebhookRegistry> = service;
    (hook, registry)
}

#[allow(dead_code)]
fn _assert_sender_sized<S: Sender + 'static>(_: &S) {}

/// Try to construct the embedded agent runtime from env vars. Mirrors
/// [`try_build_nl_compiler`] but for the `agent` subsystem.
///
/// When `gateway` is `Some`, tool dispatch via `cel_act` is enabled and the
/// system prompt is updated accordingly.
fn try_build_agent_runtime(
    memory: Arc<dyn MemoryProvider>,
    chat_bus: ChatBus,
    gateway: Option<Arc<dyn AgentGateway>>,
) -> Option<Arc<AgentRuntime>> {
    let router = match Router::from_env(&["agent"]) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "embedded agent: no provider configured \
                 (set CELLAR_DEFAULT_PROVIDER + CELLAR_DEFAULT_MODEL); \
                 agent.message will return LlmProviderError"
            );
            return None;
        }
    };
    let handle = match router.get("agent") {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "embedded agent: subsystem not resolved");
            return None;
        }
    };
    let tool_dispatch_enabled = gateway.is_some();
    let mut runtime = AgentRuntime::new(
        memory,
        handle.provider.clone(),
        handle.model.clone(),
        chat_bus,
    );
    if let Some(gw) = gateway {
        runtime = runtime.with_gateway(gw);
    }
    tracing::info!(
        model = %handle.model,
        tool_dispatch = tool_dispatch_enabled,
        "embedded agent runtime ready (agent.message enabled)"
    );
    Some(Arc::new(runtime))
}

/// Try to construct an NL rule compiler from environment variables.
///
/// Returns `None` (with a `tracing::warn`) on any failure — the daemon
/// still boots and serves every method except `rules.compile`. Returns
/// `Some(Arc<Compiler>)` on success, after logging which provider and
/// model were resolved.
///
/// Env-var resolution rules live in `cellar_llm_router::config` — set
/// `CELLAR_DEFAULT_PROVIDER` + `CELLAR_DEFAULT_MODEL` for the simple case,
/// or `CELLAR_NL_COMPILER_PROVIDER` + `CELLAR_NL_COMPILER_MODEL` for a
/// per-subsystem override (e.g., cheap small model for compiles, full
/// model for the embedded agent).
fn try_build_nl_compiler(memory: Option<&Arc<dyn MemoryProvider>>) -> Option<Arc<Compiler>> {
    let router = match Router::from_env(&["nl_compiler"]) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "NL rule compiler: no provider configured \
                 (set CELLAR_DEFAULT_PROVIDER + CELLAR_DEFAULT_MODEL); \
                 rules.compile will return LlmProviderError"
            );
            return None;
        }
    };
    let handle = match router.get("nl_compiler") {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "NL rule compiler: subsystem not resolved");
            return None;
        }
    };
    let mut compiler = Compiler::new(handle.provider.clone(), handle.model.clone());
    if let Some(memory) = memory {
        compiler = compiler.with_memory(Arc::clone(memory));
        tracing::info!(
            model = %handle.model,
            "NL rule compiler ready (rules.compile enabled, precedent retrieval wired)"
        );
    } else {
        tracing::info!(
            model = %handle.model,
            "NL rule compiler ready (rules.compile enabled, no memory — precedent retrieval disabled)"
        );
    }
    Some(Arc::new(compiler))
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
