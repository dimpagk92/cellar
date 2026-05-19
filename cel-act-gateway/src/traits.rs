//! Pluggable trait surfaces around the gateway.
//!
//! The gateway owns the matcher and the decision logic; everything else
//! (the actuator that actually runs cel_act, the broker that resolves
//! confirmation requests, the rule source the matcher reads from) is
//! pluggable so the daemon, integration tests, and future replacements can
//! supply their own implementations.

use async_trait::async_trait;
use cellar_types::{Rule, WatchlistLookup};
use serde_json::Value;

use crate::action::{ConfirmationDecision, ConfirmationRequest, ProposedAction};

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

/// Source of the current rule set for the matcher.
///
/// In the daemon this reads from SQLite via a hot-reloaded snapshot; in
/// tests it returns a fixed `Vec<Rule>`.
pub trait RuleSource: Send + Sync {
    /// Return a snapshot of all enabled-or-disabled rules. The matcher
    /// itself filters by `enabled` — this trait just lists.
    fn snapshot(&self) -> Vec<Rule>;
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
