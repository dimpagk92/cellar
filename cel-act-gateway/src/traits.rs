//! Pluggable trait surfaces around the gateway.
//!
//! The gateway owns the matcher and the decision logic; everything else
//! (the actuator that actually runs cel_act, the broker that resolves
//! confirmation requests, the rule source the matcher reads from) is
//! pluggable so the daemon, integration tests, and future replacements can
//! supply their own implementations.

use async_trait::async_trait;
use cellar_types::{Event, Rule, WatchlistLookup};
use serde_json::Value;

use crate::action::{ConfirmationDecision, ConfirmationRequest, ProposedAction};
use crate::decision::FiredRuleSnapshot;

/// The underlying `cel_act` executor — the layer that actually performs the
/// action on the device.
#[async_trait]
pub trait Actuator: Send + Sync {
    /// Execute the proposed action. Returns whatever the action produces.
    /// Errors propagate to the caller via [`crate::GatewayError::Actuator`].
    async fn execute(&self, action: &ProposedAction) -> Result<Value, String>;
}

/// Resolves confirmation requests. In the production daemon this pushes to
/// the Tauri app over IPC and waits for the user's click; in tests it's a
/// scripted in-process broker.
#[async_trait]
pub trait ConfirmationBroker: Send + Sync {
    /// Pause the action, present the confirmation request to whoever can
    /// answer (user UI, scripted test, auto-allow harness), and return the
    /// resolution. Implementations are responsible for honouring the
    /// request's `expires_at`.
    async fn request_confirmation(
        &self,
        req: ConfirmationRequest,
    ) -> Result<ConfirmationDecision, String>;
}

/// Blanket impl so the daemon can hand an `Arc<B>` (typically
/// `Arc<IpcConfirmationBroker>`) to `Gateway::new` and share the same
/// broker with the IPC handler's `confirmation.resolve` path.
#[async_trait]
impl<T: ConfirmationBroker + ?Sized> ConfirmationBroker for std::sync::Arc<T> {
    async fn request_confirmation(
        &self,
        req: ConfirmationRequest,
    ) -> Result<ConfirmationDecision, String> {
        (**self).request_confirmation(req).await
    }
}

/// Source of the current rule set for the matcher.
///
/// In the daemon this reads from SQLite via a hot-reloaded snapshot; in
/// tests it returns a fixed `Vec<Rule>`.
pub trait RuleSource: Send + Sync {
    /// Return a snapshot of all enabled-or-disabled rules. The matcher
    /// itself filters by `enabled` — this trait just lists.
    fn snapshot(&self) -> Vec<Rule>;
}

/// Webhook fan-out hook the gateway invokes when a `watcher`-kind rule
/// (i.e., `action.type == Webhook`) fires. Implementations enqueue the
/// delivery asynchronously and return immediately — the gateway's hot
/// path must not block on the actual HTTP call.
///
/// The production implementation lives in the `cellar-webhook` crate and
/// wraps `WebhookService`. Tests can use a recording implementation to
/// assert the gateway called through for the right (rule, event) pairs.
///
/// Failure mode: implementations log + drop on enqueue failure. The
/// gateway treats webhook fan-out as best-effort; a backed-up queue must
/// not stall agent actuation. (Confirmation flow and `cel_act` execution
/// are separate paths.)
#[async_trait]
pub trait WebhookHook: Send + Sync {
    /// Called once per matched watcher rule. `fire` carries the
    /// rule identity (id, name, NL original, webhook_id) and `event`
    /// is the originating event (the synthesised `agent_action_attempted`
    /// for gateway-driven fires, or a bus event for future event-bus
    /// fires).
    async fn deliver(&self, fire: &FiredRuleSnapshot, event: &Event);
}

// ────────────────── Agent action hook (activity bus) ──────────────────

/// Hook the gateway calls after every `intercept()` with the final
/// `(action, outcome)` pair. Used by the daemon to fan out to the
/// `agent_actions.*` subscribe / recent IPC surface.
///
/// Implementations must be non-blocking: the gateway calls `on_action`
/// fire-and-forget on the hot path. Failures inside the hook are
/// silently swallowed (best-effort, same contract as `WebhookHook`).
#[async_trait]
pub trait AgentActionHook: Send + Sync {
    /// Called once per `intercept()` with the resolved outcome. The
    /// action write to memory has already happened; this is purely
    /// the fan-out point.
    async fn on_action(&self, action: &ProposedAction, outcome: &crate::action::ActionOutcome);
}

/// Blanket impl: `Arc<H>` where `H: AgentActionHook`.
#[async_trait]
impl<H: AgentActionHook + ?Sized> AgentActionHook for std::sync::Arc<H> {
    async fn on_action(&self, action: &ProposedAction, outcome: &crate::action::ActionOutcome) {
        (**self).on_action(action, outcome).await
    }
}

// ────────────────── Agent gateway (tool dispatch) ──────────────────

/// Thin trait the embedded agent uses to dispatch `cel_act` tool calls
/// through the governance gateway without knowing the gateway's concrete
/// generic parameters.
///
/// The blanket impl below delegates to [`crate::gateway::Gateway::intercept`]
/// so the daemon can hand an `Arc<dyn AgentGateway>` to the runtime and the
/// runtime stays decoupled from the four gateway type-params.
#[async_trait]
pub trait AgentGateway: Send + Sync {
    /// Intercept a proposed action. Delegated directly to the gateway's
    /// [`crate::gateway::Gateway::intercept`].
    async fn intercept_tool_call(
        &self,
        action: ProposedAction,
    ) -> Result<crate::action::ActionOutcome, crate::error::GatewayError>;
}

/// Blanket impl: `Arc<G>` where `G: AgentGateway`.
#[async_trait]
impl<G: AgentGateway + ?Sized> AgentGateway for std::sync::Arc<G> {
    async fn intercept_tool_call(
        &self,
        action: ProposedAction,
    ) -> Result<crate::action::ActionOutcome, crate::error::GatewayError> {
        (**self).intercept_tool_call(action).await
    }
}

/// Blanket impl: every `Gateway<A, B, R, W>` is an `AgentGateway`.
#[async_trait]
impl<A, B, R, W> AgentGateway for crate::gateway::Gateway<A, B, R, W>
where
    A: super::traits::Actuator + Send + Sync,
    B: super::traits::ConfirmationBroker + Send + Sync,
    R: super::traits::RuleSource + Send + Sync,
    W: cellar_types::WatchlistLookup + Send + Sync,
{
    async fn intercept_tool_call(
        &self,
        action: ProposedAction,
    ) -> Result<crate::action::ActionOutcome, crate::error::GatewayError> {
        self.intercept(action).await
    }
}

// ────────────────── Watchlist source ──────────────────

/// Marker trait combining [`WatchlistLookup`] with `Send + Sync`. The gateway
/// is generic over `W: WatchlistLookup + Send + Sync` directly; this trait
/// exists for consumers who want to abstract over the bound.
pub trait WatchlistSource: WatchlistLookup + Send + Sync {}

impl<T: WatchlistLookup + Send + Sync> WatchlistSource for T {}

// ────────────────── Reference implementations ──────────────────

/// In-memory [`RuleSource`]. Useful for tests and as the seed implementation
/// before the daemon's SQLite-backed source lands.
pub struct StaticRules(pub Vec<Rule>);

impl RuleSource for StaticRules {
    fn snapshot(&self) -> Vec<Rule> {
        self.0.clone()
    }
}

/// Blanket impl so callers can hand an `Arc<R>` to `Gateway::new` and to
/// the matcher consumer task. The daemon's SQLite-backed store is held in
/// an `Arc` and shared by both — this impl is what makes that wiring
/// compile.
impl<T: RuleSource + ?Sized> RuleSource for std::sync::Arc<T> {
    fn snapshot(&self) -> Vec<Rule> {
        (**self).snapshot()
    }
}
