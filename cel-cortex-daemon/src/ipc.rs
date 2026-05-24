//! The daemon's IPC handler — implements the locked
//! [`cellar_ipc::Handler`] trait against real subsystems.
//!
//! Slice 2b lights up `rules.*` and `watchlists.*` via the
//! [`SqliteRulesStore`]. The other method groups stay at
//! [`cellar_ipc::IpcError::NotImplemented`] — Slice 2c wires
//! `rules.compile` through the NL compiler, and later phases bring
//! the agent, fires, events, confirmation, and webhook surfaces online.
//!
//! `system.*` and `daemon.status` keep the same shape they had on
//! [`cellar_ipc::StubHandler`]; `daemon.status` now reports real rule
//! and watchlist counts read from the store.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use cellar_ipc::error::{IpcError, IpcResult};
use cellar_ipc::handler::FrameSink;
use cellar_ipc::params::{
    agent as agent_params, confirmation as cf_params, events as ev_params, fires as fi_params,
    rules as rules_params, system, watchlists as wl_params, webhooks as wh_params,
};
use cellar_ipc::results::agent::{
    AgentMessage, AgentMessageResult, AgentSessionMetadata, AgentSessionsCreateResult,
    AgentSessionsGetResult, AgentSessionsListResult,
};
use cellar_ipc::results::confirmation::{ConfirmationListPendingResult, ConfirmationResolveResult};
use cellar_ipc::results::daemon::{DaemonStatusResult, RuleStats, WatchlistStats};
use cellar_ipc::results::rules::{
    RulesAddResult, RulesCompileResult, RulesGetResult, RulesListResult,
};
use cellar_ipc::results::system::{SystemHelloResult, SystemShutdownResult};
use cellar_ipc::results::watchlists::{WatchlistsGetResult, WatchlistsListResult};
use cellar_ipc::results::webhooks::{WebhooksListResult, WebhooksTestResult};
use cellar_ipc::results::{OkResult, SubscribeResult};
use cellar_ipc::stub::{SUPPORTED_PROTOCOL_VERSIONS, V1_PHASE1_CAPABILITIES};
use cellar_ipc::subscription::{StreamName, SubscriptionId};
use cellar_ipc::Handler;
use cellar_rule_compiler::{CompileError, CompileRequest, Compiler};
use cellar_rules_store::{RulesStoreError, SqliteRulesStore};
use cellar_types::{Event, EventKind, EventSource};
use cellar_webhook::{AttemptOutcome, ReqwestSender, Sender, WebhookRegistry, WebhookSecret};
use uuid::Uuid;

use crate::agent_action_bus::{AgentActionBus, AgentActionRing};
use crate::agent_runtime::{AgentRuntime, AGENT_CALLER_ID};
use crate::bus::EventBus;
use crate::chat_bus::ChatBus;
use crate::confirmation::{ConfirmationBus, IpcConfirmationBroker};
use crate::fire_bus::{FireBus, FireFrame};
use crate::recent::{event_matches, event_to_value, fire_matches, fire_to_value, Ring};
use crate::subscriptions::{
    spawn_agent_actions_forwarder, spawn_agent_chat_forwarder, spawn_confirmation_forwarder,
    spawn_events_forwarder, spawn_fires_forwarder, SubscriptionRegistry,
};

/// IPC handler the daemon installs on its `Server`. Holds the shared
/// `Arc<SqliteRulesStore>` so writes through `rules.*` / `watchlists.*`
/// are visible to the gateway and the matcher consumer task on their
/// next snapshot — see `cellar-rules-store/tests/hot_reload.rs`.
///
/// The optional `compiler` field is the NL → typed `Rule` compiler used
/// by `rules.compile`. The daemon tries to build one from env vars at
/// startup (`CELLAR_DEFAULT_PROVIDER` + `CELLAR_DEFAULT_MODEL`, or
/// `CELLAR_NL_COMPILER_*` for an override). If no provider is configured
/// the field is `None` and `rules.compile` returns `LlmProviderError`
/// with a clear "compiler not configured" message — every other
/// rules.* / watchlists.* method continues to work.
pub struct DaemonIpcHandler {
    daemon_version: String,
    started_at: Instant,
    shutting_down: AtomicBool,
    open_subscriptions: AtomicU64,
    rules_store: Arc<SqliteRulesStore>,
    compiler: Option<Arc<Compiler>>,
    /// Activity-tab plumbing. `None` (default) means the streaming
    /// surface is disabled — `.subscribe` / `.recent` methods return
    /// `NotImplemented` until the daemon installs streams via
    /// [`Self::with_streams`].
    streams: Option<HandlerStreams>,
    /// Confirmation flow plumbing. `None` (default) means
    /// `confirmation.*` methods return `NotImplemented`.
    confirmation: Option<HandlerConfirmation>,
    /// Agent runtime + shared memory + chat bus. `None` (default) means
    /// `agent.*` methods return `NotImplemented`. When configured but
    /// `agent_runtime` itself is `None`, `agent.sessions.*` works (via
    /// the memory provider) and `agent.message` returns `LlmProviderError`.
    agent: Option<HandlerAgent>,
    /// Webhook registry for hot-reload. When set, `webhooks.add` and
    /// `webhooks.remove` call through to the live service so deliveries
    /// take effect immediately without a daemon restart.
    webhook_registry: Option<Arc<dyn WebhookRegistry>>,
}

/// Bundle of agent-related plumbing.
struct HandlerAgent {
    memory: Arc<dyn cel_memory::MemoryProvider>,
    runtime: Option<Arc<AgentRuntime>>,
    chat_bus: ChatBus,
}

/// Bundle of confirmation-flow plumbing the handler holds. The same
/// broker the gateway holds (`Arc` clone) plus the bus for the
/// `confirmation.subscribe` forwarder.
struct HandlerConfirmation {
    broker: Arc<IpcConfirmationBroker>,
    bus: ConfirmationBus,
}

/// Bundle of references the handler needs to serve `events.*` / `fires.*` /
/// `agent_actions.*`. Constructed once by the daemon and handed to the handler
/// via [`DaemonIpcHandler::with_streams`]; the IPC server keeps a single
/// `Arc<DaemonIpcHandler>` across all connections.
struct HandlerStreams {
    event_bus: EventBus,
    fire_bus: FireBus,
    event_ring: Arc<Ring<Event>>,
    fire_ring: Arc<Ring<FireFrame>>,
    agent_action_bus: AgentActionBus,
    agent_action_ring: Arc<AgentActionRing>,
    registry: Arc<SubscriptionRegistry>,
}

impl DaemonIpcHandler {
    /// Build a handler without an NL compiler. `rules.compile` will return
    /// `IpcError::LlmProviderError`. Used by tests and by daemons running
    /// without an LLM provider configured.
    pub fn new(daemon_version: impl Into<String>, rules_store: Arc<SqliteRulesStore>) -> Self {
        Self::with_compiler(daemon_version, rules_store, None)
    }

    /// Build a handler with an optional NL compiler. Pass `Some(compiler)`
    /// to enable `rules.compile`. The `Compiler` is itself `Arc`-wrapped so
    /// callers can keep their own reference if they need to invoke the
    /// compiler outside the handler.
    pub fn with_compiler(
        daemon_version: impl Into<String>,
        rules_store: Arc<SqliteRulesStore>,
        compiler: Option<Arc<Compiler>>,
    ) -> Self {
        Self {
            daemon_version: daemon_version.into(),
            started_at: Instant::now(),
            shutting_down: AtomicBool::new(false),
            open_subscriptions: AtomicU64::new(0),
            rules_store,
            compiler,
            streams: None,
            confirmation: None,
            agent: None,
            webhook_registry: None,
        }
    }

    /// Plug in the agent runtime + shared memory + chat bus. The
    /// agent runtime itself can be `None` when no LLM provider is
    /// configured; `agent.sessions.*` still works via memory.
    pub fn with_agent(
        mut self,
        memory: Arc<dyn cel_memory::MemoryProvider>,
        runtime: Option<Arc<AgentRuntime>>,
        chat_bus: ChatBus,
    ) -> Self {
        self.agent = Some(HandlerAgent {
            memory,
            runtime,
            chat_bus,
        });
        self
    }

    /// Plug in the confirmation-flow plumbing. Without it, `confirmation.*`
    /// methods return `NotImplemented`. Wired by the daemon in
    /// `wire_with_store_compiler_and_memory`.
    pub fn with_confirmation(
        mut self,
        broker: Arc<IpcConfirmationBroker>,
        bus: ConfirmationBus,
    ) -> Self {
        self.confirmation = Some(HandlerConfirmation { broker, bus });
        self
    }

    /// Plug in the webhook registry for hot-reload support. Without it,
    /// `webhooks.add` / `webhooks.remove` still persist to the store but
    /// won't activate the running service until daemon restart.
    pub fn with_webhook_registry(mut self, registry: Option<Arc<dyn WebhookRegistry>>) -> Self {
        self.webhook_registry = registry;
        self
    }

    /// Plug in the activity-tab streaming plumbing (buses + rings +
    /// subscription registry). Built once at daemon startup; without it,
    /// `events.*` / `fires.*` / `agent_actions.*` return `NotImplemented`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_streams(
        mut self,
        event_bus: EventBus,
        fire_bus: FireBus,
        event_ring: Arc<Ring<Event>>,
        fire_ring: Arc<Ring<FireFrame>>,
        agent_action_bus: AgentActionBus,
        agent_action_ring: Arc<AgentActionRing>,
        registry: Arc<SubscriptionRegistry>,
    ) -> Self {
        self.streams = Some(HandlerStreams {
            event_bus,
            fire_bus,
            event_ring,
            fire_ring,
            agent_action_bus,
            agent_action_ring,
            registry,
        });
        self
    }

    fn uptime_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

/// Map a `RulesStoreError` into an `IpcError` honouring the
/// daemon-specific JSON-RPC codes from [`cellar-ipc-protocol.md`] §3.2.
///
/// Validation errors (duplicate id, malformed JSON in payload) become
/// `ValidationFailed`; everything else becomes `Internal` with the
/// underlying detail in the message. The store's
/// [`RulesStoreError::is_unique_constraint_violation`] helper keeps the
/// daemon from having to know about SQLite internals.
fn store_err_to_ipc(err: RulesStoreError) -> IpcError {
    if err.is_unique_constraint_violation() {
        return IpcError::ValidationFailed(format!("constraint violation: {err}"));
    }
    if matches!(err, RulesStoreError::Json(_)) {
        return IpcError::ValidationFailed(format!("json error: {err}"));
    }
    IpcError::Internal(format!("rules store: {err}"))
}

/// Map a `CompileError` from `cellar-rule-compiler` into the typed
/// `IpcError` codes from the RFC. Bad-input failures (empty NL,
/// LLM-produced rule didn't validate) map to `ValidationFailed`
/// (-32010); upstream LLM failures map to `LlmProviderError` (-32011);
/// JSON parse blow-ups map to `Internal` because they shouldn't reach
/// a client (the compiler retries once before surfacing them).
/// Best-effort snapshot of the current process's resident memory (MB) and
/// CPU (%). Uses `sysinfo` — takes a new system snapshot each call, so this
/// is not free. Only called from `daemon.status`, which is a human-driven
/// path (low frequency). Returns `(0.0, 0.0)` on any failure.
fn process_resource_snapshot() -> (f64, f64) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory().with_cpu()),
    );
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let pid = Pid::from(std::process::id() as usize);
    if let Some(p) = sys.process(pid) {
        let memory_mb = p.memory() as f64 / (1024.0 * 1024.0);
        let cpu_pct = f64::from(p.cpu_usage());
        (memory_mb, cpu_pct)
    } else {
        (0.0, 0.0)
    }
}

fn compile_err_to_ipc(err: CompileError) -> IpcError {
    match err {
        CompileError::EmptyInput => IpcError::ValidationFailed("nl_string is empty".into()),
        CompileError::Validation(msg) => {
            IpcError::ValidationFailed(format!("compile validation: {msg}"))
        }
        CompileError::NoJsonInResponse => {
            IpcError::LlmProviderError("LLM did not produce valid JSON after retry".into())
        }
        CompileError::JsonParse(e) => {
            IpcError::Internal(format!("compile JSON parse (should have retried): {e}"))
        }
        CompileError::Provider(e) => IpcError::LlmProviderError(format!("{e}")),
    }
}

#[async_trait]
impl Handler for DaemonIpcHandler {
    // ───── system.* ─────

    async fn system_hello(
        &self,
        params: system::SystemHelloParams,
    ) -> IpcResult<SystemHelloResult> {
        let chosen = SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|v| params.supported_protocol_versions.iter().any(|c| c == *v))
            .copied();
        let Some(version) = chosen else {
            return Err(IpcError::UnsupportedProtocolVersion(
                params.supported_protocol_versions,
            ));
        };
        // The set of caps gets `rules.crud` added now that we serve those,
        // plus `rules.compile` when the NL compiler is wired (the client
        // can detect "this daemon has no LLM provider configured" without
        // calling `rules.compile` and getting an error).
        let mut capabilities: Vec<String> = V1_PHASE1_CAPABILITIES
            .iter()
            .map(|s| s.to_string())
            .collect();
        capabilities.push("rules.crud".into());
        capabilities.push("watchlists.crud".into());
        capabilities.push("webhooks.crud".into());
        if self.compiler.is_some() {
            capabilities.push("rules.compile".into());
        }
        if self.streams.is_some() {
            capabilities.push("events.subscribe".into());
            capabilities.push("fires.subscribe".into());
        }
        if self.confirmation.is_some() {
            capabilities.push("confirmation".into());
        }
        if let Some(a) = self.agent.as_ref() {
            capabilities.push("agent.sessions".into());
            if a.runtime.is_some() {
                capabilities.push("agent.message".into());
            }
        }

        Ok(SystemHelloResult {
            protocol_version: version.to_string(),
            daemon_version: self.daemon_version.clone(),
            daemon_uptime_s: self.uptime_s(),
            session_id: format!("ses_{}", Uuid::now_v7()),
            capabilities,
        })
    }

    async fn system_shutdown(
        &self,
        _params: system::SystemShutdownParams,
    ) -> IpcResult<SystemShutdownResult> {
        self.shutting_down.store(true, Ordering::SeqCst);
        Ok(SystemShutdownResult {
            shutting_down: true,
        })
    }

    // ───── daemon.* ─────

    async fn daemon_status(&self) -> IpcResult<DaemonStatusResult> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(IpcError::ShuttingDown);
        }
        let rules = self.rules_store.list_rules();
        let enabled = rules.iter().filter(|r| r.enabled).count();
        let watchlists = self
            .rules_store
            .list_watchlists()
            .map_err(store_err_to_ipc)?
            .len();

        // Pending confirmation count from the broker's live registry.
        let pending_confirmations = self
            .confirmation
            .as_ref()
            .map(|c| c.broker.list_pending().len() as u64)
            .unwrap_or(0);

        // Active agent sessions: open sessions in memory with caller_id=embedded.
        let agent_sessions_active = if let Some(a) = &self.agent {
            a.memory
                .list_sessions(cel_memory::SessionFilter {
                    caller_id: Some(AGENT_CALLER_ID.into()),
                    open_only: true,
                    ..Default::default()
                })
                .await
                .map(|v| v.len() as u64)
                .unwrap_or(0)
        } else {
            0
        };

        // Best-effort process memory + CPU via sysinfo.
        let (memory_mb, cpu_pct) = process_resource_snapshot();

        Ok(DaemonStatusResult {
            healthy: true,
            uptime_s: self.uptime_s(),
            rules: RuleStats {
                total: rules.len() as u64,
                enabled: enabled as u64,
            },
            watchlists: WatchlistStats {
                total: watchlists as u64,
            },
            recent_fires_24h: 0,
            pending_confirmations,
            agent_sessions_active,
            daemon_version: self.daemon_version.clone(),
            memory_mb,
            cpu_pct,
        })
    }

    // ───── rules.* ─────

    async fn rules_list(
        &self,
        _params: rules_params::RulesListParams,
    ) -> IpcResult<RulesListResult> {
        let mut rules = self.rules_store.list_rules();
        // Newest first (Cargo says "newest-first" in the result doc).
        rules.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(RulesListResult { rules })
    }

    async fn rules_get(&self, params: rules_params::RulesGetParams) -> IpcResult<RulesGetResult> {
        Ok(RulesGetResult {
            rule: self.rules_store.get_rule(&params.id),
        })
    }

    async fn rules_add(&self, params: rules_params::RulesAddParams) -> IpcResult<RulesAddResult> {
        let rule_id = params.rule.id.clone();
        if rule_id.trim().is_empty() {
            return Err(IpcError::ValidationFailed(
                "rule.id must not be empty".into(),
            ));
        }
        self.rules_store
            .create_rule(params.rule)
            .map_err(store_err_to_ipc)?;
        Ok(RulesAddResult { rule_id })
    }

    async fn rules_update(&self, params: rules_params::RulesUpdateParams) -> IpcResult<OkResult> {
        // Enforce that the URL-style id matches the body id — keeps the
        // protocol surface tight even though the underlying store only
        // looks at body.id.
        if params.id != params.rule.id {
            return Err(IpcError::ValidationFailed(format!(
                "params.id ({}) does not match rule.id ({})",
                params.id, params.rule.id
            )));
        }
        let updated = self
            .rules_store
            .update_rule(params.rule)
            .map_err(store_err_to_ipc)?;
        if !updated {
            return Err(IpcError::RuleNotFound(params.id));
        }
        Ok(OkResult::default())
    }

    async fn rules_remove(&self, params: rules_params::RuleIdParams) -> IpcResult<OkResult> {
        let deleted = self
            .rules_store
            .delete_rule(&params.id)
            .map_err(store_err_to_ipc)?;
        if !deleted {
            return Err(IpcError::RuleNotFound(params.id));
        }
        Ok(OkResult::default())
    }

    async fn rules_pause(&self, params: rules_params::RuleIdParams) -> IpcResult<OkResult> {
        let updated = self
            .rules_store
            .set_enabled(&params.id, false)
            .map_err(store_err_to_ipc)?;
        if !updated {
            return Err(IpcError::RuleNotFound(params.id));
        }
        Ok(OkResult::default())
    }

    async fn rules_resume(&self, params: rules_params::RuleIdParams) -> IpcResult<OkResult> {
        let updated = self
            .rules_store
            .set_enabled(&params.id, true)
            .map_err(store_err_to_ipc)?;
        if !updated {
            return Err(IpcError::RuleNotFound(params.id));
        }
        Ok(OkResult::default())
    }

    async fn rules_compile(
        &self,
        params: rules_params::RulesCompileParams,
    ) -> IpcResult<RulesCompileResult> {
        let Some(compiler) = self.compiler.as_ref() else {
            return Err(IpcError::LlmProviderError(
                "NL compiler not configured; set CELLAR_DEFAULT_PROVIDER + CELLAR_DEFAULT_MODEL \
                 (or CELLAR_NL_COMPILER_PROVIDER + CELLAR_NL_COMPILER_MODEL) and restart"
                    .into(),
            ));
        };

        // Surface the daemon's current watchlists to the compiler so it can
        // emit "rule references unknown watchlist X" warnings. This is a
        // snapshot — racing concurrent watchlist mutations would only
        // affect warning accuracy, not correctness.
        let watchlists: Vec<String> = self
            .rules_store
            .list_watchlists()
            .map_err(store_err_to_ipc)?
            .into_iter()
            .map(|w| w.name)
            .collect();

        let req = CompileRequest::new(params.nl_string).with_watchlists(watchlists);

        match compiler.compile(req).await {
            Ok(result) => Ok(RulesCompileResult {
                draft_rule: result.draft_rule,
                human_readable: result.human_readable,
                warnings: result.warnings,
            }),
            Err(e) => Err(compile_err_to_ipc(e)),
        }
    }

    async fn rules_test(
        &self,
        params: rules_params::RulesTestParams,
    ) -> IpcResult<cellar_ipc::results::rules::RulesTestResult> {
        let Some(rule) = self.rules_store.get_rule(&params.id) else {
            return Err(IpcError::RuleNotFound(params.id));
        };
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("rules.test"));
        };

        // Walk the recent-events ring with a `since` filter. The ring is
        // bounded (default 1024 entries / a few minutes deep on the
        // demo workload) so this is cheap.
        let candidates = streams
            .event_ring
            .filtered(usize::MAX, |e| e.ts >= params.since);

        let rules_vec = vec![rule];
        let mut matched = Vec::new();
        for event in &candidates {
            let fires =
                cellar_types::Matcher::evaluate(event, &rules_vec, self.rules_store.as_ref());
            if !fires.is_empty() {
                matched.push(event_to_value(event));
            }
        }

        Ok(cellar_ipc::results::rules::RulesTestResult {
            matched_events: matched,
        })
    }

    // ───── watchlists.* ─────

    async fn watchlists_list(
        &self,
        _params: wl_params::WatchlistsListParams,
    ) -> IpcResult<WatchlistsListResult> {
        Ok(WatchlistsListResult {
            watchlists: self
                .rules_store
                .list_watchlists()
                .map_err(store_err_to_ipc)?,
        })
    }

    async fn watchlists_get(
        &self,
        params: wl_params::WatchlistNameParams,
    ) -> IpcResult<WatchlistsGetResult> {
        Ok(WatchlistsGetResult {
            watchlist: self
                .rules_store
                .get_watchlist(&params.name)
                .map_err(store_err_to_ipc)?,
        })
    }

    async fn watchlists_set(&self, params: wl_params::WatchlistsSetParams) -> IpcResult<OkResult> {
        self.rules_store
            .set_watchlist_items(&params.name, &params.items)
            .map_err(store_err_to_ipc)?;
        Ok(OkResult::default())
    }

    async fn watchlists_add_item(
        &self,
        params: wl_params::WatchlistsItemParams,
    ) -> IpcResult<OkResult> {
        // Pre-check existence so the typical "no such list" case yields a
        // clean `WatchlistNotFound` instead of a generic constraint
        // violation. There's a small TOCTOU window if another concurrent
        // call deletes the list between this check and the insert, but
        // (a) the daemon is the only writer, and (b) the worst case is a
        // less-specific Internal error — no data corruption.
        if !self.rules_store.has_watchlist(&params.name) {
            return Err(IpcError::WatchlistNotFound(params.name));
        }
        self.rules_store
            .add_watchlist_item(&params.name, &params.item)
            .map_err(store_err_to_ipc)?;
        Ok(OkResult::default())
    }

    async fn watchlists_remove_item(
        &self,
        params: wl_params::WatchlistsItemParams,
    ) -> IpcResult<OkResult> {
        // `remove_watchlist_item` returns false if the (list, item) pair
        // doesn't exist — that's idempotent-friendly. We don't distinguish
        // "list missing" from "item missing"; both yield Ok.
        self.rules_store
            .remove_watchlist_item(&params.name, &params.item)
            .map_err(store_err_to_ipc)?;
        Ok(OkResult::default())
    }

    async fn watchlists_remove(
        &self,
        params: wl_params::WatchlistNameParams,
    ) -> IpcResult<OkResult> {
        let deleted = self
            .rules_store
            .delete_watchlist(&params.name)
            .map_err(store_err_to_ipc)?;
        if !deleted {
            return Err(IpcError::WatchlistNotFound(params.name));
        }
        Ok(OkResult::default())
    }

    // ───── webhooks.* ─────

    async fn webhooks_list(
        &self,
        _params: wh_params::WebhooksListParams,
    ) -> IpcResult<WebhooksListResult> {
        Ok(WebhooksListResult {
            webhooks: self.rules_store.list_webhooks(),
        })
    }

    async fn webhooks_add(&self, params: wh_params::WebhooksAddParams) -> IpcResult<OkResult> {
        let id = params.config.id.clone();
        if id.trim().is_empty() {
            return Err(IpcError::ValidationFailed(
                "webhook.id must not be empty".into(),
            ));
        }
        // Persist to store first so the config survives daemon restart.
        self.rules_store
            .create_webhook(params.config.clone())
            .map_err(store_err_to_ipc)?;
        // Hot-reload: register with the running service so delivery is
        // active immediately — no restart needed.
        if let Some(registry) = &self.webhook_registry {
            // Resolve the secret from the env at add time (same as startup).
            let secret = params
                .config
                .secret_header
                .as_ref()
                .zip(params.config.secret_value_env.as_ref())
                .and_then(|(header, env_name)| {
                    std::env::var(env_name).ok().map(|value| WebhookSecret {
                        header_name: header.clone(),
                        header_value: value,
                    })
                });
            registry.register_webhook(params.config, secret);
            tracing::info!(
                webhook_id = %id,
                "webhooks.add: webhook active (hot-reload)"
            );
        } else {
            tracing::warn!(
                webhook_id = %id,
                "webhooks.add: webhook recorded in store but webhook service not running"
            );
        }
        Ok(OkResult::default())
    }

    async fn webhooks_remove(&self, params: wh_params::WebhookIdParams) -> IpcResult<OkResult> {
        let deleted = self
            .rules_store
            .delete_webhook(&params.id)
            .map_err(store_err_to_ipc)?;
        if !deleted {
            return Err(IpcError::WebhookNotFound(params.id));
        }
        // Hot-reload: remove from the running service so future deliveries
        // for this id are dropped immediately.
        if let Some(registry) = &self.webhook_registry {
            registry.unregister_webhook(&params.id);
        }
        Ok(OkResult::default())
    }

    async fn webhooks_test(
        &self,
        params: wh_params::WebhookIdParams,
    ) -> IpcResult<WebhooksTestResult> {
        let Some(cfg) = self.rules_store.get_webhook(&params.id) else {
            return Err(IpcError::WebhookNotFound(params.id));
        };

        // Resolve the secret freshly (env vars may have changed since daemon
        // start). Best-effort: if the env var isn't set, send without a
        // secret header — the target's response surfaces the issue.
        let secret = cfg
            .secret_header
            .as_ref()
            .zip(cfg.secret_value_env.as_ref())
            .and_then(|(header, env_name)| {
                std::env::var(env_name).ok().map(|value| WebhookSecret {
                    header_name: header.clone(),
                    header_value: value,
                })
            });

        // Construct a synthetic test event so the target can see what a
        // real fire would look like. `tested_at` marks it as a test in the
        // event.data so receivers can route accordingly.
        let test_event = Event::now(
            EventSource::CelActGateway,
            EventKind::Other("webhook.test".into()),
        )
        .with_data("webhook_id", cfg.id.clone())
        .with_data("source", "cellar-daemon.webhooks.test");
        let payload = serde_json::json!({
            "fired_at": chrono::Utc::now().to_rfc3339(),
            "rule": {
                "id": "webhook-test",
                "name": "Cellar webhook test",
                "nl_original": "Test delivery initiated from cellar-daemon"
            },
            "event": test_event,
        });
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| IpcError::Internal(format!("serialize test payload: {e}")))?;

        let sender = ReqwestSender::new();
        let start = std::time::Instant::now();
        let outcome = sender.send(&cfg, secret.as_ref(), &bytes).await;
        let elapsed_ms = start.elapsed().as_millis() as u64;

        Ok(match outcome {
            AttemptOutcome::Success { status } => WebhooksTestResult {
                reachable: true,
                status_code: Some(status),
                elapsed_ms: Some(elapsed_ms),
                error: None,
            },
            AttemptOutcome::RetryableHttp { status, .. } => WebhooksTestResult {
                reachable: true,
                status_code: Some(status),
                elapsed_ms: Some(elapsed_ms),
                error: Some(format!("retryable HTTP {status}")),
            },
            AttemptOutcome::PermanentHttp { status } => WebhooksTestResult {
                reachable: true,
                status_code: Some(status),
                elapsed_ms: Some(elapsed_ms),
                error: Some(format!("permanent HTTP {status}")),
            },
            AttemptOutcome::RetryableNetwork { message } => WebhooksTestResult {
                reachable: false,
                status_code: None,
                elapsed_ms: Some(elapsed_ms),
                error: Some(format!("network: {message}")),
            },
            AttemptOutcome::PermanentOther { message } => WebhooksTestResult {
                reachable: false,
                status_code: None,
                elapsed_ms: Some(elapsed_ms),
                error: Some(message),
            },
        })
    }

    // ───── events.* ─────

    async fn events_recent(
        &self,
        params: ev_params::EventsRecentParams,
    ) -> IpcResult<Vec<serde_json::Value>> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("events.recent"));
        };
        let limit = params.filter.limit.unwrap_or(200).max(1);
        Ok(streams
            .event_ring
            .filtered(limit, |e| event_matches(e, &params.filter))
            .iter()
            .map(event_to_value)
            .collect())
    }

    async fn events_subscribe(
        &self,
        params: ev_params::EventsSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("events.subscribe"));
        };
        let id = spawn_events_forwarder(&streams.registry, &streams.event_bus, params.filter, sink);
        self.on_stream(StreamName::Events, &id, true).await;
        Ok(SubscribeResult {
            subscription_id: id,
        })
    }

    async fn events_unsubscribe(
        &self,
        params: ev_params::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("events.unsubscribe"));
        };
        let existed = streams.registry.unregister(&params.subscription_id);
        if existed {
            self.on_stream(StreamName::Events, &params.subscription_id, false)
                .await;
        }
        // Idempotent: unknown id is `Ok(())`. The locked protocol's
        // `OkResult` carries no failure variant for "no such subscription".
        Ok(OkResult::default())
    }

    // ───── fires.* ─────

    async fn fires_recent(
        &self,
        params: fi_params::FiresRecentParams,
    ) -> IpcResult<Vec<serde_json::Value>> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("fires.recent"));
        };
        let limit = params.filter.limit.unwrap_or(200).max(1);
        Ok(streams
            .fire_ring
            .filtered(limit, |f| fire_matches(f, &params.filter))
            .iter()
            .map(fire_to_value)
            .collect())
    }

    async fn fires_subscribe(
        &self,
        params: fi_params::FiresSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("fires.subscribe"));
        };
        let id = spawn_fires_forwarder(&streams.registry, &streams.fire_bus, params.filter, sink);
        self.on_stream(StreamName::Fires, &id, true).await;
        Ok(SubscribeResult {
            subscription_id: id,
        })
    }

    async fn fires_unsubscribe(&self, params: ev_params::UnsubscribeParams) -> IpcResult<OkResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("fires.unsubscribe"));
        };
        let existed = streams.registry.unregister(&params.subscription_id);
        if existed {
            self.on_stream(StreamName::Fires, &params.subscription_id, false)
                .await;
        }
        Ok(OkResult::default())
    }

    // ───── agent_actions.* ─────

    async fn agent_actions_recent(
        &self,
        params: cellar_ipc::params::agent_actions::AgentActionsRecentParams,
    ) -> IpcResult<Vec<serde_json::Value>> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("agent_actions.recent"));
        };
        let limit = params.filter.limit.unwrap_or(100).min(1000);
        let f = params.filter.clone();
        let frames = streams.agent_action_ring.filtered(limit, move |frame| {
            crate::recent::agent_action_matches(frame, &f)
        });
        Ok(frames.into_iter().map(|fr| fr.action).collect())
    }

    async fn agent_actions_subscribe(
        &self,
        params: cellar_ipc::params::agent_actions::AgentActionsSubscribeParams,
        sink: cellar_ipc::handler::FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("agent_actions.subscribe"));
        };
        let id = spawn_agent_actions_forwarder(
            &streams.registry,
            &streams.agent_action_bus,
            params.filter,
            sink,
        );
        self.on_stream(StreamName::AgentActions, &id, true).await;
        Ok(SubscribeResult {
            subscription_id: id,
        })
    }

    async fn agent_actions_unsubscribe(
        &self,
        params: ev_params::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("agent_actions.unsubscribe"));
        };
        let existed = streams.registry.unregister(&params.subscription_id);
        if existed {
            self.on_stream(StreamName::AgentActions, &params.subscription_id, false)
                .await;
        }
        Ok(OkResult::default())
    }

    // ───── confirmation.* ─────

    async fn confirmation_list_pending(
        &self,
        _params: cf_params::ConfirmationListPendingParams,
    ) -> IpcResult<ConfirmationListPendingResult> {
        let Some(c) = self.confirmation.as_ref() else {
            return Err(IpcError::NotImplemented("confirmation.list_pending"));
        };
        Ok(ConfirmationListPendingResult {
            pending: c.broker.list_pending(),
        })
    }

    async fn confirmation_resolve(
        &self,
        params: cf_params::ConfirmationResolveParams,
    ) -> IpcResult<ConfirmationResolveResult> {
        let Some(c) = self.confirmation.as_ref() else {
            return Err(IpcError::NotImplemented("confirmation.resolve"));
        };
        let outcome = c
            .broker
            .resolve(&params.id, params.decision, params.remember_kind);
        if !outcome.resolved {
            // Per the wire contract: `resolved: false` on already-resolved or
            // never-known IDs. Also surface the typed error for clients that
            // care about distinguishing this from a successful no-op.
            return Err(IpcError::ConfirmationNotFound(params.id));
        }
        // `action_outcome` is set by the broker once the gateway resumes.
        // For v1 we report it as "completed" — the gateway's
        // `ActionOutcome::Executed` path is the success case; failures
        // surface via `confirmation.list_pending` (the entry is gone) and
        // the gateway's own audit chunk in memory.
        Ok(ConfirmationResolveResult {
            resolved: true,
            action_outcome: "completed".into(),
            remembered_as: outcome.remembered_as,
        })
    }

    async fn confirmation_subscribe(
        &self,
        _params: cf_params::ConfirmationSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let (Some(c), Some(s)) = (self.confirmation.as_ref(), self.streams.as_ref()) else {
            return Err(IpcError::NotImplemented("confirmation.subscribe"));
        };
        let id = spawn_confirmation_forwarder(&s.registry, &c.bus, sink);
        self.on_stream(StreamName::Confirmation, &id, true).await;
        Ok(SubscribeResult {
            subscription_id: id,
        })
    }

    async fn confirmation_unsubscribe(
        &self,
        params: ev_params::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("confirmation.unsubscribe"));
        };
        let existed = streams.registry.unregister(&params.subscription_id);
        if existed {
            self.on_stream(StreamName::Confirmation, &params.subscription_id, false)
                .await;
        }
        Ok(OkResult::default())
    }

    // ───── agent.* ─────

    async fn agent_sessions_list(
        &self,
        _params: agent_params::AgentSessionsListParams,
    ) -> IpcResult<AgentSessionsListResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.sessions.list"));
        };
        let sessions = a
            .memory
            .list_sessions(cel_memory::SessionFilter::default())
            .await
            .map_err(|e| IpcError::Internal(format!("memory: {e}")))?;
        let mut metas: Vec<AgentSessionMetadata> = sessions
            .into_iter()
            .filter(|s| s.caller_id == AGENT_CALLER_ID)
            .map(|s| AgentSessionMetadata {
                id: s.id,
                title: s.title,
                created_at: s.started_at,
                updated_at: s.ended_at.unwrap_or(s.started_at),
                message_count: 0,
                outcome: match s.outcome {
                    cel_memory::SessionOutcome::Open => "open".into(),
                    cel_memory::SessionOutcome::Success => "success".into(),
                    cel_memory::SessionOutcome::Failure => "failure".into(),
                    cel_memory::SessionOutcome::Aborted => "aborted".into(),
                },
            })
            .collect();
        // Newest first.
        metas.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        Ok(AgentSessionsListResult { sessions: metas })
    }

    async fn agent_sessions_create(
        &self,
        params: agent_params::AgentSessionsCreateParams,
    ) -> IpcResult<AgentSessionsCreateResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.sessions.create"));
        };
        let session = a
            .memory
            .open_session(cel_memory::NewMemorySession {
                caller_id: AGENT_CALLER_ID.into(),
                title: params.title,
                metadata: serde_json::Value::Null,
            })
            .await
            .map_err(|e| IpcError::Internal(format!("memory: {e}")))?;
        Ok(AgentSessionsCreateResult {
            session_id: session.id,
        })
    }

    async fn agent_sessions_get(
        &self,
        params: agent_params::SessionIdParams,
    ) -> IpcResult<AgentSessionsGetResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.sessions.get"));
        };
        let Some(session) = a
            .memory
            .get_session(&params.session_id)
            .await
            .map_err(|e| IpcError::Internal(format!("memory: {e}")))?
        else {
            return Err(IpcError::SessionNotFound(params.session_id));
        };
        // Pull chat chunks for this session. Sentinel query — the
        // `session_id` filter in the query restricts the result set;
        // the lexical text isn't actually used to filter, just required
        // by BasicMemoryProvider's contract.
        let chunks = a
            .memory
            .retrieve(cel_memory::MemoryQuery {
                text: " ".into(),
                kinds: Some(vec![cel_memory::ChunkKind::Chat]),
                since: None,
                until: None,
                session_id: Some(params.session_id.clone()),
                caller_scope: cel_memory::CallerScope::Own,
                project_root_prefix: None,
                k: 200,
                include_rollups: false,
                min_importance: None,
                profile: cel_memory::RetrievalProfile::AgentChatTurn,
                caller_id: AGENT_CALLER_ID.into(),
            })
            .await
            .unwrap_or_default();
        let mut sorted: Vec<_> = chunks.into_iter().collect();
        sorted.sort_by_key(|c| c.created_at);
        let messages: Vec<AgentMessage> = sorted
            .into_iter()
            .map(|c| AgentMessage {
                id: c.id.clone(),
                role: c
                    .metadata
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string(),
                content: serde_json::Value::String(c.content.clone()),
                created_at: c.created_at,
            })
            .collect();
        Ok(AgentSessionsGetResult {
            session: AgentSessionMetadata {
                id: session.id,
                title: session.title,
                created_at: session.started_at,
                updated_at: session.ended_at.unwrap_or(session.started_at),
                message_count: messages.len() as u64,
                outcome: match session.outcome {
                    cel_memory::SessionOutcome::Open => "open".into(),
                    cel_memory::SessionOutcome::Success => "success".into(),
                    cel_memory::SessionOutcome::Failure => "failure".into(),
                    cel_memory::SessionOutcome::Aborted => "aborted".into(),
                },
            },
            messages,
        })
    }

    async fn agent_sessions_rename(
        &self,
        params: agent_params::AgentSessionsRenameParams,
    ) -> IpcResult<OkResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.sessions.rename"));
        };
        a.memory
            .rename_session(&params.session_id, &params.title)
            .await
            .map_err(|e| match e {
                cel_memory::MemoryError::NotFound(_) => {
                    IpcError::SessionNotFound(params.session_id.clone())
                }
                other => IpcError::Internal(format!("memory: {other}")),
            })?;
        Ok(OkResult::default())
    }

    async fn agent_sessions_delete(
        &self,
        params: agent_params::SessionIdParams,
    ) -> IpcResult<OkResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.sessions.delete"));
        };
        a.memory
            .close_session(&params.session_id, cel_memory::SessionOutcome::Aborted)
            .await
            .map_err(|e| match e {
                cel_memory::MemoryError::NotFound(_) => {
                    IpcError::SessionNotFound(params.session_id.clone())
                }
                other => IpcError::Internal(format!("memory: {other}")),
            })?;
        Ok(OkResult::default())
    }

    async fn agent_message(
        &self,
        params: agent_params::AgentMessageParams,
    ) -> IpcResult<AgentMessageResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.message"));
        };
        let Some(runtime) = a.runtime.as_ref() else {
            return Err(IpcError::LlmProviderError(
                "embedded agent not configured; set CELLAR_DEFAULT_PROVIDER + \
                 CELLAR_DEFAULT_MODEL and restart"
                    .into(),
            ));
        };
        // Validate session exists.
        let Some(_session) = a
            .memory
            .get_session(&params.session_id)
            .await
            .map_err(|e| IpcError::Internal(format!("memory: {e}")))?
        else {
            return Err(IpcError::SessionNotFound(params.session_id));
        };

        // Drive the turn in the background; return the request_id
        // immediately. The chat bus delivers MessageComplete +
        // RequestDone frames when the LLM call finishes.
        let runtime = runtime.clone();
        let session_id = params.session_id.clone();
        let content = params.content.clone();
        let request_id_placeholder = format!("req_{}", uuid::Uuid::now_v7());
        let user_message_placeholder = format!("msg_{}", uuid::Uuid::now_v7());
        let return_request_id = request_id_placeholder.clone();
        let return_message_id = user_message_placeholder.clone();

        tokio::spawn(async move {
            match runtime.run_turn(&session_id, &content).await {
                Ok(_result) => {
                    tracing::debug!(
                        session_id = %session_id,
                        "agent turn completed"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        session_id = %session_id,
                        "agent turn failed"
                    );
                }
            }
        });

        Ok(AgentMessageResult {
            request_id: return_request_id,
            message_id: return_message_id,
        })
    }

    async fn agent_chat_subscribe(
        &self,
        params: agent_params::AgentChatSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<cellar_ipc::results::SubscribeResult> {
        let (Some(a), Some(s)) = (self.agent.as_ref(), self.streams.as_ref()) else {
            return Err(IpcError::NotImplemented("agent.chat.subscribe"));
        };
        let id = spawn_agent_chat_forwarder(&s.registry, &a.chat_bus, params.session_id, sink);
        self.on_stream(StreamName::AgentChat, &id, true).await;
        Ok(cellar_ipc::results::SubscribeResult {
            subscription_id: id,
        })
    }

    async fn agent_chat_unsubscribe(
        &self,
        params: ev_params::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        let Some(streams) = self.streams.as_ref() else {
            return Err(IpcError::NotImplemented("agent.chat.unsubscribe"));
        };
        let existed = streams.registry.unregister(&params.subscription_id);
        if existed {
            self.on_stream(StreamName::AgentChat, &params.subscription_id, false)
                .await;
        }
        Ok(OkResult::default())
    }

    async fn agent_interrupt(
        &self,
        params: agent_params::AgentInterruptParams,
    ) -> IpcResult<OkResult> {
        let Some(a) = self.agent.as_ref() else {
            return Err(IpcError::NotImplemented("agent.interrupt"));
        };
        // Signal the agent runtime to abort the in-flight turn for this
        // session on its next tool-loop iteration. The interrupt flag is
        // latched in a HashSet so it is visible the next time run_turn's
        // per-iteration check fires; for turns that have already reached
        // the LLM call the interrupt takes effect before the next LLM
        // request (between tool-call iterations).
        if let Some(rt) = &a.runtime {
            rt.interrupt(&params.session_id);
        }
        Ok(OkResult::default())
    }

    // ───── Marker hooks ─────

    async fn on_stream(&self, _stream: StreamName, _id: &SubscriptionId, attaching: bool) {
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

    /// Called by the IPC server when a client connection ends. We use
    /// the opportunity to prune any forwarder tasks that have already
    /// exited (their per-connection FrameSink was dropped on close, so
    /// the next send failed and the task returned). This keeps the
    /// subscription registry size proportional to live subscriptions
    /// rather than total subscriptions ever created.
    async fn on_disconnect(&self) {
        let Some(streams) = self.streams.as_ref() else {
            return;
        };
        let pruned = streams.registry.prune_completed();
        if pruned > 0 {
            tracing::debug!(
                pruned,
                live = streams.registry.len(),
                "subscription registry pruned on disconnect"
            );
            // Reflect the prune in the open_subscriptions counter so
            // daemon.status doesn't report ghosts.
            let pruned_u64 = pruned as u64;
            self.open_subscriptions
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(pruned_u64))
                })
                .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cellar_ipc::params::{rules::*, watchlists::*};
    use cellar_types::expression::Operator;
    use cellar_types::rule::{Action, ActionType, RuleKind};
    use cellar_types::{Expression, Rule};
    use chrono::Utc;
    use serde_json::json;

    fn handler() -> DaemonIpcHandler {
        let store = SqliteRulesStore::in_memory().unwrap();
        DaemonIpcHandler::new("test", store)
    }

    fn sample(id: &str) -> Rule {
        Rule {
            id: id.into(),
            name: format!("rule {id}"),
            nl_original: "test".into(),
            kind: RuleKind::Watcher,
            enabled: true,
            match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
            action: Action {
                action_type: ActionType::LogOnly,
                webhook_id: None,
                timeout_s: None,
            },
            cooldown_seconds: 0,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn rules_add_then_list() {
        let h = handler();
        h.rules_add(RulesAddParams { rule: sample("r1") })
            .await
            .unwrap();
        let list = h.rules_list(RulesListParams::default()).await.unwrap();
        assert_eq!(list.rules.len(), 1);
        assert_eq!(list.rules[0].id, "r1");
    }

    #[tokio::test]
    async fn rules_get_returns_none_for_missing() {
        let h = handler();
        let r = h
            .rules_get(RulesGetParams { id: "ghost".into() })
            .await
            .unwrap();
        assert!(r.rule.is_none());
    }

    #[tokio::test]
    async fn rules_remove_unknown_yields_typed_error() {
        let h = handler();
        let err = h
            .rules_remove(RuleIdParams { id: "ghost".into() })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::RuleNotFound(ref id) if id == "ghost"));
        assert_eq!(err.code(), -32004);
    }

    #[tokio::test]
    async fn rules_update_mismatched_id_validation_fails() {
        let h = handler();
        let err = h
            .rules_update(RulesUpdateParams {
                id: "outer".into(),
                rule: sample("inner"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn rules_pause_resume_toggles_enabled() {
        let h = handler();
        h.rules_add(RulesAddParams { rule: sample("r1") })
            .await
            .unwrap();
        h.rules_pause(RuleIdParams { id: "r1".into() })
            .await
            .unwrap();
        let got = h
            .rules_get(RulesGetParams { id: "r1".into() })
            .await
            .unwrap()
            .rule
            .unwrap();
        assert!(!got.enabled);
        h.rules_resume(RuleIdParams { id: "r1".into() })
            .await
            .unwrap();
        let got = h
            .rules_get(RulesGetParams { id: "r1".into() })
            .await
            .unwrap()
            .rule
            .unwrap();
        assert!(got.enabled);
    }

    #[tokio::test]
    async fn rules_add_duplicate_id_validation_fails() {
        let h = handler();
        h.rules_add(RulesAddParams {
            rule: sample("dup"),
        })
        .await
        .unwrap();
        let err = h
            .rules_add(RulesAddParams {
                rule: sample("dup"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn rules_add_empty_id_validation_fails() {
        let h = handler();
        let err = h
            .rules_add(RulesAddParams { rule: sample("") })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn watchlists_set_and_lookup() {
        let h = handler();
        h.watchlists_set(WatchlistsSetParams {
            name: "approved".into(),
            items: vec!["com.apple.Safari".into(), "com.slack.Slack".into()],
        })
        .await
        .unwrap();
        let list = h
            .watchlists_list(WatchlistsListParams::default())
            .await
            .unwrap();
        assert_eq!(list.watchlists.len(), 1);
        assert_eq!(list.watchlists[0].name, "approved");
        assert_eq!(list.watchlists[0].items.len(), 2);
    }

    #[tokio::test]
    async fn watchlists_add_item_to_missing_yields_typed_error() {
        let h = handler();
        let err = h
            .watchlists_add_item(WatchlistsItemParams {
                name: "nonexistent".into(),
                item: "x".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::WatchlistNotFound(ref n) if n == "nonexistent"));
    }

    #[tokio::test]
    async fn watchlists_remove_unknown_yields_typed_error() {
        let h = handler();
        let err = h
            .watchlists_remove(WatchlistNameParams {
                name: "ghost".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::WatchlistNotFound(_)));
    }

    #[tokio::test]
    async fn daemon_status_reflects_rule_and_watchlist_counts() {
        let h = handler();
        h.rules_add(RulesAddParams { rule: sample("r1") })
            .await
            .unwrap();
        h.rules_add(RulesAddParams {
            rule: {
                let mut r = sample("r2");
                r.enabled = false;
                r
            },
        })
        .await
        .unwrap();
        h.watchlists_set(WatchlistsSetParams {
            name: "wl".into(),
            items: vec!["x".into()],
        })
        .await
        .unwrap();

        let status = h.daemon_status().await.unwrap();
        assert_eq!(status.rules.total, 2);
        assert_eq!(status.rules.enabled, 1);
        assert_eq!(status.watchlists.total, 1);
        assert!(status.healthy);
    }

    #[tokio::test]
    async fn system_hello_advertises_new_caps() {
        let h = handler();
        let r = h
            .system_hello(system::SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            })
            .await
            .unwrap();
        assert!(r.capabilities.contains(&"rules.crud".into()));
        assert!(r.capabilities.contains(&"watchlists.crud".into()));
    }

    // ───── rules.test ─────

    fn handler_with_streams() -> DaemonIpcHandler {
        let store = SqliteRulesStore::in_memory().unwrap();
        let event_bus = crate::bus::EventBus::with_capacity(32);
        let fire_bus = crate::fire_bus::FireBus::new();
        let event_ring: Arc<crate::recent::Ring<Event>> = Arc::new(crate::recent::Ring::new());
        let fire_ring: Arc<crate::recent::Ring<crate::fire_bus::FireFrame>> =
            Arc::new(crate::recent::Ring::new());
        let agent_action_bus = crate::agent_action_bus::AgentActionBus::new();
        let agent_action_ring: Arc<crate::agent_action_bus::AgentActionRing> =
            Arc::new(crate::agent_action_bus::AgentActionRing::new());
        let registry = Arc::new(crate::subscriptions::SubscriptionRegistry::new());
        DaemonIpcHandler::new("test", store).with_streams(
            event_bus,
            fire_bus,
            event_ring,
            fire_ring,
            agent_action_bus,
            agent_action_ring,
            registry,
        )
    }

    #[tokio::test]
    async fn rules_test_unknown_rule_returns_typed_error() {
        let h = handler_with_streams();
        let err = h
            .rules_test(RulesTestParams {
                id: "ghost".into(),
                since: chrono::Utc::now() - chrono::Duration::hours(1),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::RuleNotFound(_)));
    }

    #[tokio::test]
    async fn rules_test_replays_ring_and_returns_matches() {
        use cellar_types::{Event, EventKind, EventSource};

        // Set up: handler with streams + one rule.
        let store = SqliteRulesStore::in_memory().unwrap();
        let event_bus = crate::bus::EventBus::with_capacity(32);
        let fire_bus = crate::fire_bus::FireBus::new();
        let event_ring: Arc<crate::recent::Ring<Event>> = Arc::new(crate::recent::Ring::new());
        let fire_ring: Arc<crate::recent::Ring<crate::fire_bus::FireFrame>> =
            Arc::new(crate::recent::Ring::new());
        let agent_action_bus = crate::agent_action_bus::AgentActionBus::new();
        let agent_action_ring: Arc<crate::agent_action_bus::AgentActionRing> =
            Arc::new(crate::agent_action_bus::AgentActionRing::new());
        let registry = Arc::new(crate::subscriptions::SubscriptionRegistry::new());

        // Pre-populate the ring with one matching + one non-matching event.
        event_ring.push(
            Event::now(EventSource::Fsevents, EventKind::FileDeleted).with_data("path", "/tmp/x"),
        );
        event_ring.push(Event::now(EventSource::Fsevents, EventKind::FileCreated));

        // Rule that matches file_deleted only.
        store
            .create_rule(Rule {
                id: "r_delete".into(),
                name: "Delete rule".into(),
                nl_original: "fire on file deletion".into(),
                kind: RuleKind::Watcher,
                enabled: true,
                match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
                action: Action {
                    action_type: ActionType::LogOnly,
                    webhook_id: None,
                    timeout_s: None,
                },
                cooldown_seconds: 0,
                created_at: chrono::Utc::now(),
            })
            .unwrap();

        let h = DaemonIpcHandler::new("test", store).with_streams(
            event_bus,
            fire_bus,
            event_ring,
            fire_ring,
            agent_action_bus,
            agent_action_ring,
            registry,
        );

        let r = h
            .rules_test(RulesTestParams {
                id: "r_delete".into(),
                since: chrono::Utc::now() - chrono::Duration::hours(1),
            })
            .await
            .unwrap();
        assert_eq!(r.matched_events.len(), 1);
        assert_eq!(r.matched_events[0]["kind"], "file_deleted");
    }

    #[tokio::test]
    async fn rules_test_since_filter_excludes_older_events() {
        use cellar_types::{Event, EventKind, EventSource};

        let store = SqliteRulesStore::in_memory().unwrap();
        let event_bus = crate::bus::EventBus::with_capacity(32);
        let fire_bus = crate::fire_bus::FireBus::new();
        let event_ring: Arc<crate::recent::Ring<Event>> = Arc::new(crate::recent::Ring::new());
        let fire_ring: Arc<crate::recent::Ring<crate::fire_bus::FireFrame>> =
            Arc::new(crate::recent::Ring::new());
        let agent_action_bus = crate::agent_action_bus::AgentActionBus::new();
        let agent_action_ring: Arc<crate::agent_action_bus::AgentActionRing> =
            Arc::new(crate::agent_action_bus::AgentActionRing::new());
        let registry = Arc::new(crate::subscriptions::SubscriptionRegistry::new());

        // One event from an hour ago, one from now.
        let mut old = Event::now(EventSource::Fsevents, EventKind::FileDeleted);
        old.ts = chrono::Utc::now() - chrono::Duration::hours(1);
        event_ring.push(old);
        event_ring.push(Event::now(EventSource::Fsevents, EventKind::FileDeleted));

        store
            .create_rule(Rule {
                id: "r1".into(),
                name: "r1".into(),
                nl_original: "test".into(),
                kind: RuleKind::Watcher,
                enabled: true,
                match_expr: Expression::leaf("kind", Operator::Eq, json!("file_deleted")),
                action: Action {
                    action_type: ActionType::LogOnly,
                    webhook_id: None,
                    timeout_s: None,
                },
                cooldown_seconds: 0,
                created_at: chrono::Utc::now(),
            })
            .unwrap();

        let h = DaemonIpcHandler::new("test", store).with_streams(
            event_bus,
            fire_bus,
            event_ring,
            fire_ring,
            agent_action_bus,
            agent_action_ring,
            registry,
        );

        // Since = 5 min ago — only the "now" event counts.
        let r = h
            .rules_test(RulesTestParams {
                id: "r1".into(),
                since: chrono::Utc::now() - chrono::Duration::minutes(5),
            })
            .await
            .unwrap();
        assert_eq!(r.matched_events.len(), 1);
    }

    #[tokio::test]
    async fn rules_compile_without_provider_returns_llm_provider_error() {
        // Default handler (built via `new`) has no compiler — `rules.compile`
        // must return a clear LlmProviderError, not panic, not pretend to
        // succeed.
        let h = handler();
        let err = h
            .rules_compile(RulesCompileParams {
                nl_string: "anything".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::LlmProviderError(ref m) if m.contains("not configured")));
        assert_eq!(err.code(), -32011);
    }

    // ───── rules.compile with a mock LLM provider ─────

    fn handler_with_compiler(responses: &[&str]) -> DaemonIpcHandler {
        use cellar_llm_router::provider::MockProvider;
        use cellar_llm_router::types::{CompletionResponse, ContentBlock, StopReason, Usage};
        let responses: Vec<CompletionResponse> = responses
            .iter()
            .map(|t| CompletionResponse {
                content: vec![ContentBlock::Text {
                    text: t.to_string(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                model: None,
            })
            .collect();
        let provider = MockProvider::new(responses);
        let compiler = Arc::new(Compiler::new(provider, "mock-model"));
        let store = SqliteRulesStore::in_memory().unwrap();
        DaemonIpcHandler::with_compiler("test", store, Some(compiler))
    }

    const GOOD_JSON: &str = r#"{
        "id": "draft",
        "name": "Big delete",
        "nl_original": "notify when files >1GB are deleted from Documents",
        "kind": "watcher",
        "enabled": true,
        "created_at": "1970-01-01T00:00:00Z",
        "match": {
            "all": [
                {"leaf": {"field": "kind", "op": "eq", "value": "file_deleted"}},
                {"leaf": {"field": "data.size_bytes", "op": "gte", "value": 1073741824}}
            ]
        },
        "action": {"type": "webhook", "webhook_id": "default"},
        "cooldown_seconds": 60
    }"#;

    #[tokio::test]
    async fn rules_compile_happy_path_returns_draft_and_summary() {
        let h = handler_with_compiler(&[GOOD_JSON]);
        let r = h
            .rules_compile(RulesCompileParams {
                nl_string: "notify when files >1GB are deleted from Documents".into(),
            })
            .await
            .unwrap();
        assert_eq!(r.draft_rule.name, "Big delete");
        assert_eq!(r.draft_rule.kind, RuleKind::Watcher);
        assert!(!r.human_readable.is_empty());
        assert!(r.warnings.is_empty());
    }

    #[tokio::test]
    async fn rules_compile_empty_input_validation_fails() {
        let h = handler_with_compiler(&[GOOD_JSON]);
        let err = h
            .rules_compile(RulesCompileParams {
                nl_string: "   ".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::ValidationFailed(_)));
        assert_eq!(err.code(), -32010);
    }

    #[tokio::test]
    async fn rules_compile_no_json_after_retry_is_llm_provider_error() {
        // MockProvider replays the same response on retry, so two "no JSON"
        // responses → final error from the compiler is NoJsonInResponse →
        // we map to LlmProviderError (the LLM failed us, not the user).
        let h = handler_with_compiler(&["I cannot help with that.", "Sorry."]);
        let err = h
            .rules_compile(RulesCompileParams {
                nl_string: "something".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::LlmProviderError(_)));
        assert_eq!(err.code(), -32011);
    }

    #[tokio::test]
    async fn rules_compile_does_not_persist_draft() {
        // The handler must not auto-save the draft. `rules.list` should
        // still be empty after a successful compile.
        let h = handler_with_compiler(&[GOOD_JSON]);
        let _ = h
            .rules_compile(RulesCompileParams {
                nl_string: "notify when files >1GB are deleted from Documents".into(),
            })
            .await
            .unwrap();
        let list = h.rules_list(RulesListParams::default()).await.unwrap();
        assert!(list.rules.is_empty(), "compile must not persist");
    }

    #[tokio::test]
    async fn rules_compile_passes_known_watchlists_to_compiler() {
        // Set up a watchlist that the rule will reference — compiler
        // should NOT produce a warning since the watchlist exists.
        let json_with_watchlist = r#"{
            "id": "draft",
            "name": "App allowlist",
            "nl_original": "alert me when an app outside approved_apps launches",
            "kind": "watcher",
            "enabled": true,
            "created_at": "1970-01-01T00:00:00Z",
            "match": {
                "all": [
                    {"leaf": {"field": "kind", "op": "eq", "value": "process_started"}},
                    {"leaf": {"field": "data.bundle_id", "op": "not_in_watchlist", "value": "approved_apps"}}
                ]
            },
            "action": {"type": "webhook", "webhook_id": "default"},
            "cooldown_seconds": 60
        }"#;

        let h = handler_with_compiler(&[json_with_watchlist]);
        // Pre-create the watchlist so the rule reference is valid.
        h.watchlists_set(WatchlistsSetParams {
            name: "approved_apps".into(),
            items: vec!["com.apple.Safari".into()],
        })
        .await
        .unwrap();

        let r = h
            .rules_compile(RulesCompileParams {
                nl_string: "alert me when an app outside approved_apps launches".into(),
            })
            .await
            .unwrap();
        assert!(
            r.warnings.is_empty(),
            "no warnings expected when watchlist exists, got {:?}",
            r.warnings
        );
    }

    // ───── webhooks.* ─────

    fn sample_webhook(id: &str) -> cellar_types::WebhookConfig {
        cellar_types::WebhookConfig {
            id: id.into(),
            url: "https://example.com/hook".into(),
            headers: Default::default(),
            secret_header: None,
            secret_value_env: None,
            timeout_ms: 5000,
        }
    }

    #[tokio::test]
    async fn webhooks_add_then_list() {
        let h = handler();
        h.webhooks_add(wh_params::WebhooksAddParams {
            config: sample_webhook("default"),
        })
        .await
        .unwrap();
        let list = h
            .webhooks_list(wh_params::WebhooksListParams::default())
            .await
            .unwrap();
        assert_eq!(list.webhooks.len(), 1);
        assert_eq!(list.webhooks[0].id, "default");
    }

    #[tokio::test]
    async fn webhooks_add_duplicate_validation_fails() {
        let h = handler();
        h.webhooks_add(wh_params::WebhooksAddParams {
            config: sample_webhook("dup"),
        })
        .await
        .unwrap();
        let err = h
            .webhooks_add(wh_params::WebhooksAddParams {
                config: sample_webhook("dup"),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn webhooks_add_empty_id_validation_fails() {
        let h = handler();
        let err = h
            .webhooks_add(wh_params::WebhooksAddParams {
                config: sample_webhook(""),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::ValidationFailed(_)));
    }

    #[tokio::test]
    async fn webhooks_remove_unknown_returns_typed_error() {
        let h = handler();
        let err = h
            .webhooks_remove(wh_params::WebhookIdParams { id: "ghost".into() })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::WebhookNotFound(_)));
        assert_eq!(err.code(), -32006);
    }

    #[tokio::test]
    async fn webhooks_test_unknown_id_returns_typed_error() {
        let h = handler();
        let err = h
            .webhooks_test(wh_params::WebhookIdParams {
                id: "missing".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, IpcError::WebhookNotFound(_)));
    }

    #[tokio::test]
    async fn webhooks_test_unreachable_returns_test_result_with_error() {
        // Use an unreachable port — the test result should come back with
        // reachable=false and an error message, never an Err return.
        let h = handler();
        let mut cfg = sample_webhook("local-unreachable");
        cfg.url = "http://127.0.0.1:1/never-reachable".into();
        cfg.timeout_ms = 500;
        h.webhooks_add(wh_params::WebhooksAddParams { config: cfg })
            .await
            .unwrap();

        let r = h
            .webhooks_test(wh_params::WebhookIdParams {
                id: "local-unreachable".into(),
            })
            .await
            .unwrap();
        assert!(!r.reachable);
        assert!(r.error.is_some());
    }

    #[tokio::test]
    async fn system_hello_advertises_webhooks_crud() {
        let h = handler();
        let r = h
            .system_hello(system::SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            })
            .await
            .unwrap();
        assert!(r.capabilities.contains(&"webhooks.crud".into()));
    }

    #[tokio::test]
    async fn system_hello_advertises_rules_compile_only_when_wired() {
        // Without compiler.
        let h = handler();
        let r = h
            .system_hello(system::SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            })
            .await
            .unwrap();
        assert!(!r.capabilities.contains(&"rules.compile".into()));

        // With compiler.
        let h = handler_with_compiler(&[GOOD_JSON]);
        let r = h
            .system_hello(system::SystemHelloParams {
                client_name: "t".into(),
                client_version: "0".into(),
                supported_protocol_versions: vec!["1".into()],
            })
            .await
            .unwrap();
        assert!(r.capabilities.contains(&"rules.compile".into()));
    }
}
