//! The [`Handler`] trait — what the daemon implements.
//!
//! Every RPC method has a typed method here. The default implementation of
//! each returns [`crate::IpcError::NotImplemented`] so a daemon can supply
//! only the methods whose backing subsystem is wired and let the rest fall
//! through cleanly.
//!
//! The [`crate::Server`] dispatches incoming [`JsonRpcRequest`] messages
//! to these typed methods after deserialising params, and serialises typed
//! results back into [`JsonRpcResponse`].

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::{IpcError, IpcResult};
use crate::params::{
    agent, agent_actions, confirmation, events, fires, rules, settings, system, watchlists,
    webhooks,
};
use crate::results;
use crate::results::{OkResult, SubscribeResult};
use crate::subscription::{StreamFrame, StreamName, SubscriptionId};

/// A handle the [`Handler`] uses to push subscription frames to the
/// connected client. The server gives one of these to the handler when a
/// `*.subscribe` call lands; the handler stashes it (e.g., in a map keyed
/// by `SubscriptionId`) and pushes frames whenever the underlying source
/// produces them.
pub type FrameSink = mpsc::Sender<StreamFrame>;

/// The daemon-side handler.
///
/// Every RPC method on the v1 protocol corresponds to one trait method here.
/// All methods return `Err(IpcError::NotImplemented)` by default; a daemon
/// overrides only the methods whose backing subsystems exist.
///
/// Subscribe methods receive a [`FrameSink`] they push frames through.
/// Unsubscribe methods receive the [`SubscriptionId`] previously returned.
#[async_trait]
#[allow(unused_variables)]
pub trait Handler: Send + Sync + 'static {
    // ───── system.* ─────

    /// `system.hello` — required first call. Real daemons override this to
    /// return their capabilities.
    async fn system_hello(
        &self,
        params: system::SystemHelloParams,
    ) -> IpcResult<results::system::SystemHelloResult> {
        Err(IpcError::NotImplemented("system.hello"))
    }

    /// `system.shutdown` — graceful daemon shutdown.
    async fn system_shutdown(
        &self,
        params: system::SystemShutdownParams,
    ) -> IpcResult<results::system::SystemShutdownResult> {
        Err(IpcError::NotImplemented("system.shutdown"))
    }

    /// `system.pong` — client → server heartbeat acknowledgement. Default
    /// no-op.
    async fn system_pong(&self) -> IpcResult<OkResult> {
        Ok(OkResult::default())
    }

    // ───── daemon.* ─────

    /// `daemon.status` — health, uptime, counts.
    async fn daemon_status(&self) -> IpcResult<results::daemon::DaemonStatusResult> {
        Err(IpcError::NotImplemented("daemon.status"))
    }

    // ───── rules.* ─────

    /// `rules.list` — all rules.
    async fn rules_list(
        &self,
        params: rules::RulesListParams,
    ) -> IpcResult<results::rules::RulesListResult> {
        Err(IpcError::NotImplemented("rules.list"))
    }
    /// `rules.get` — fetch one rule by ID.
    async fn rules_get(
        &self,
        params: rules::RulesGetParams,
    ) -> IpcResult<results::rules::RulesGetResult> {
        Err(IpcError::NotImplemented("rules.get"))
    }
    /// `rules.add` — persist a new rule.
    async fn rules_add(
        &self,
        params: rules::RulesAddParams,
    ) -> IpcResult<results::rules::RulesAddResult> {
        Err(IpcError::NotImplemented("rules.add"))
    }
    /// `rules.update` — update an existing rule.
    async fn rules_update(&self, params: rules::RulesUpdateParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("rules.update"))
    }
    /// `rules.remove` — delete a rule.
    async fn rules_remove(&self, params: rules::RuleIdParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("rules.remove"))
    }
    /// `rules.pause` — disable a rule without deleting it.
    async fn rules_pause(&self, params: rules::RuleIdParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("rules.pause"))
    }
    /// `rules.resume` — re-enable a paused rule.
    async fn rules_resume(&self, params: rules::RuleIdParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("rules.resume"))
    }
    /// `rules.compile` — NL → compiled rule (does not save).
    async fn rules_compile(
        &self,
        params: rules::RulesCompileParams,
    ) -> IpcResult<results::rules::RulesCompileResult> {
        Err(IpcError::NotImplemented("rules.compile"))
    }
    /// `rules.test` — replay recent events against this rule.
    async fn rules_test(
        &self,
        params: rules::RulesTestParams,
    ) -> IpcResult<results::rules::RulesTestResult> {
        Err(IpcError::NotImplemented("rules.test"))
    }

    // ───── watchlists.* ─────

    /// `watchlists.list` — all watchlists.
    async fn watchlists_list(
        &self,
        params: watchlists::WatchlistsListParams,
    ) -> IpcResult<results::watchlists::WatchlistsListResult> {
        Err(IpcError::NotImplemented("watchlists.list"))
    }
    /// `watchlists.get` — one watchlist by name.
    async fn watchlists_get(
        &self,
        params: watchlists::WatchlistNameParams,
    ) -> IpcResult<results::watchlists::WatchlistsGetResult> {
        Err(IpcError::NotImplemented("watchlists.get"))
    }
    /// `watchlists.set` — replace the items.
    async fn watchlists_set(&self, params: watchlists::WatchlistsSetParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("watchlists.set"))
    }
    /// `watchlists.add_item` — add one item.
    async fn watchlists_add_item(
        &self,
        params: watchlists::WatchlistsItemParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("watchlists.add_item"))
    }
    /// `watchlists.remove_item` — remove one item.
    async fn watchlists_remove_item(
        &self,
        params: watchlists::WatchlistsItemParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("watchlists.remove_item"))
    }
    /// `watchlists.remove` — delete a watchlist.
    async fn watchlists_remove(
        &self,
        params: watchlists::WatchlistNameParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("watchlists.remove"))
    }

    // ───── webhooks.* ─────

    /// `webhooks.list`.
    async fn webhooks_list(
        &self,
        params: webhooks::WebhooksListParams,
    ) -> IpcResult<results::webhooks::WebhooksListResult> {
        Err(IpcError::NotImplemented("webhooks.list"))
    }
    /// `webhooks.add`.
    async fn webhooks_add(&self, params: webhooks::WebhooksAddParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("webhooks.add"))
    }
    /// `webhooks.remove`.
    async fn webhooks_remove(&self, params: webhooks::WebhookIdParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("webhooks.remove"))
    }
    /// `webhooks.test`.
    async fn webhooks_test(
        &self,
        params: webhooks::WebhookIdParams,
    ) -> IpcResult<results::webhooks::WebhooksTestResult> {
        Err(IpcError::NotImplemented("webhooks.test"))
    }

    // ───── events.* ─────

    /// `events.recent`.
    async fn events_recent(
        &self,
        params: events::EventsRecentParams,
    ) -> IpcResult<Vec<serde_json::Value>> {
        Err(IpcError::NotImplemented("events.recent"))
    }
    /// `events.subscribe`.
    async fn events_subscribe(
        &self,
        params: events::EventsSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let _ = sink;
        Err(IpcError::NotImplemented("events.subscribe"))
    }
    /// `events.unsubscribe`.
    async fn events_unsubscribe(&self, params: events::UnsubscribeParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("events.unsubscribe"))
    }

    // ───── fires.* ─────

    /// `fires.recent`.
    async fn fires_recent(
        &self,
        params: fires::FiresRecentParams,
    ) -> IpcResult<Vec<serde_json::Value>> {
        Err(IpcError::NotImplemented("fires.recent"))
    }
    /// `fires.subscribe`.
    async fn fires_subscribe(
        &self,
        params: fires::FiresSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let _ = sink;
        Err(IpcError::NotImplemented("fires.subscribe"))
    }
    /// `fires.unsubscribe`.
    async fn fires_unsubscribe(&self, params: events::UnsubscribeParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("fires.unsubscribe"))
    }

    // ───── agent_actions.* ─────

    /// `agent_actions.recent`.
    async fn agent_actions_recent(
        &self,
        params: agent_actions::AgentActionsRecentParams,
    ) -> IpcResult<Vec<serde_json::Value>> {
        Err(IpcError::NotImplemented("agent_actions.recent"))
    }
    /// `agent_actions.subscribe`.
    async fn agent_actions_subscribe(
        &self,
        params: agent_actions::AgentActionsSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let _ = sink;
        Err(IpcError::NotImplemented("agent_actions.subscribe"))
    }
    /// `agent_actions.unsubscribe`.
    async fn agent_actions_unsubscribe(
        &self,
        params: events::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("agent_actions.unsubscribe"))
    }

    // ───── confirmation.* ─────

    /// `confirmation.list_pending`.
    async fn confirmation_list_pending(
        &self,
        params: confirmation::ConfirmationListPendingParams,
    ) -> IpcResult<results::confirmation::ConfirmationListPendingResult> {
        Err(IpcError::NotImplemented("confirmation.list_pending"))
    }
    /// `confirmation.subscribe`.
    async fn confirmation_subscribe(
        &self,
        params: confirmation::ConfirmationSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let _ = sink;
        Err(IpcError::NotImplemented("confirmation.subscribe"))
    }
    /// `confirmation.unsubscribe`.
    async fn confirmation_unsubscribe(
        &self,
        params: events::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("confirmation.unsubscribe"))
    }
    /// `confirmation.resolve`.
    async fn confirmation_resolve(
        &self,
        params: confirmation::ConfirmationResolveParams,
    ) -> IpcResult<results::confirmation::ConfirmationResolveResult> {
        Err(IpcError::NotImplemented("confirmation.resolve"))
    }

    // ───── agent.* ─────

    /// `agent.sessions.list`.
    async fn agent_sessions_list(
        &self,
        params: agent::AgentSessionsListParams,
    ) -> IpcResult<results::agent::AgentSessionsListResult> {
        Err(IpcError::NotImplemented("agent.sessions.list"))
    }
    /// `agent.sessions.create`.
    async fn agent_sessions_create(
        &self,
        params: agent::AgentSessionsCreateParams,
    ) -> IpcResult<results::agent::AgentSessionsCreateResult> {
        Err(IpcError::NotImplemented("agent.sessions.create"))
    }
    /// `agent.sessions.get`.
    async fn agent_sessions_get(
        &self,
        params: agent::SessionIdParams,
    ) -> IpcResult<results::agent::AgentSessionsGetResult> {
        Err(IpcError::NotImplemented("agent.sessions.get"))
    }
    /// `agent.sessions.rename`.
    async fn agent_sessions_rename(
        &self,
        params: agent::AgentSessionsRenameParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("agent.sessions.rename"))
    }
    /// `agent.sessions.delete`.
    async fn agent_sessions_delete(&self, params: agent::SessionIdParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("agent.sessions.delete"))
    }
    /// `agent.message`.
    async fn agent_message(
        &self,
        params: agent::AgentMessageParams,
    ) -> IpcResult<results::agent::AgentMessageResult> {
        Err(IpcError::NotImplemented("agent.message"))
    }
    /// `agent.chat.subscribe`.
    async fn agent_chat_subscribe(
        &self,
        params: agent::AgentChatSubscribeParams,
        sink: FrameSink,
    ) -> IpcResult<SubscribeResult> {
        let _ = sink;
        Err(IpcError::NotImplemented("agent.chat.subscribe"))
    }
    /// `agent.chat.unsubscribe`.
    async fn agent_chat_unsubscribe(
        &self,
        params: events::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("agent.chat.unsubscribe"))
    }
    /// `agent.interrupt`.
    async fn agent_interrupt(&self, params: agent::AgentInterruptParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("agent.interrupt"))
    }

    // ───── settings.* ─────

    /// `settings.get`.
    async fn settings_get(
        &self,
        params: settings::SettingsGetParams,
    ) -> IpcResult<serde_json::Value> {
        Err(IpcError::NotImplemented("settings.get"))
    }
    /// `settings.set`.
    async fn settings_set(&self, params: settings::SettingsSetParams) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("settings.set"))
    }

    // ───── daemon.health.* ─────

    /// `daemon.health.subscribe`.
    async fn daemon_health_subscribe(&self, sink: FrameSink) -> IpcResult<SubscribeResult> {
        let _ = sink;
        Err(IpcError::NotImplemented("daemon.health.subscribe"))
    }
    /// `daemon.health.unsubscribe`.
    async fn daemon_health_unsubscribe(
        &self,
        params: events::UnsubscribeParams,
    ) -> IpcResult<OkResult> {
        Err(IpcError::NotImplemented("daemon.health.unsubscribe"))
    }

    // ───── Marker hook ─────

    /// Optional hook the server calls when a client disconnects. Default
    /// no-op; daemons override to garbage-collect per-connection state.
    async fn on_disconnect(&self) {}

    /// Optional hook called once at the start of a connection (after the
    /// connect, before the first request). Default no-op.
    async fn on_connect(&self) {}

    /// Inform the handler about the named stream's lifecycle. Default no-op.
    /// Useful for handlers that want to consolidate connection / subscription
    /// state in one place.
    async fn on_stream(&self, stream: StreamName, id: &SubscriptionId, attaching: bool) {
        let _ = (stream, id, attaching);
    }
}
