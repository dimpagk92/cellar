//! IPC-backed confirmation broker — the Phase 3 confirmation flow.
//!
//! Replaces `cel_act_gateway::test_support::AutoAllowBroker` as the
//! production [`ConfirmationBroker`]. When the gateway matches a
//! `require_confirmation` rule it calls `request_confirmation`, which:
//!
//! 1. Mints a [`PendingConfirmation`] from the request.
//! 2. Records `(confirmation_id → oneshot::Sender)` in the registry.
//! 3. Publishes the pending entry on the [`ConfirmationBus`] so any open
//!    `confirmation.subscribe` stream picks it up.
//! 4. `await`s the oneshot with a deadline derived from `expires_at`.
//!
//! The corresponding `confirmation.resolve` IPC method (in
//! [`crate::ipc::DaemonIpcHandler`]) removes the entry from the registry
//! and fires the oneshot with the user's [`ConfirmationDecision`].
//!
//! Side effects of `always_allow`:
//!
//! - [`RememberKind::WatchlistAdd`] is applied in-line — the broker
//!   resolves the override via the `Arc<SqliteRulesStore>` it holds.
//! - [`RememberKind::ExceptionRule`] creates a new rule with
//!   `ActionType::Allow` that matches the same `data.action_type` as the
//!   originating action. `Allow` has the highest decision precedence in
//!   `Decision::from_matches`, so it will short-circuit any blocking rule
//!   that would otherwise match the same action in the future.
//!
//! **Timeout behaviour:** the broker's `await` is wrapped in
//! `tokio::time::timeout` so the gateway is guaranteed to make progress
//! even if the IPC client never resolves the confirmation. Timed-out
//! entries are removed from the registry so a late resolve is a no-op.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cel_act_gateway::{ConfirmationBroker, ConfirmationDecision, ConfirmationRequest};
use cellar_ipc::params::confirmation::{
    ConfirmationDecisionWire, PendingConfirmation, PendingRule, RememberKind,
};
use cellar_rules_store::SqliteRulesStore;
use cellar_types::{Action, ActionType, Expression, Operator, Rule, RuleKind};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast::{self, Receiver, Sender};
use tokio::sync::oneshot;
use uuid::Uuid;

/// Broadcast bus for [`PendingConfirmation`] frames. Cloned into the
/// IPC handler's `confirmation.subscribe` forwarder.
#[derive(Clone)]
pub struct ConfirmationBus {
    tx: Sender<PendingConfirmation>,
}

impl ConfirmationBus {
    /// New bus with the default capacity. Confirmations are rare (and
    /// _critical_ — per the IPC RFC the channel must not drop them).
    /// Default cap is conservatively generous; backpressure manifests as
    /// `Lagged` and the IPC layer surfaces a `Gap` frame.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx }
    }

    /// Publish one pending confirmation. Same lenient semantics as the
    /// other buses — no subscribers = silent drop.
    pub fn publish(&self, frame: PendingConfirmation) {
        let _ = self.tx.send(frame);
    }

    /// Subscribe.
    pub fn subscribe(&self) -> Receiver<PendingConfirmation> {
        self.tx.subscribe()
    }
}

impl Default for ConfirmationBus {
    fn default() -> Self {
        Self::new()
    }
}

/// One slot in the broker's pending-confirmation registry.
struct PendingEntry {
    /// The pending payload (kept for `confirmation.list_pending`).
    confirmation: PendingConfirmation,
    /// Channel the broker awaits; `resolve` fires it.
    tx: oneshot::Sender<ConfirmationDecision>,
    /// Echo of any `RememberKind` the resolver applied — surfaced back
    /// in `ConfirmationResolveResult.remembered_as`.
    remembered_as: Option<RememberKind>,
}

/// Production [`ConfirmationBroker`] backed by IPC. Holds the registry
/// of pending confirmations, the broadcast bus for `confirmation.subscribe`
/// forwarders, and an `Arc<SqliteRulesStore>` for applying
/// `always_allow` side effects (watchlist additions and exception-rule
/// creation via [`RememberKind::ExceptionRule`]).
pub struct IpcConfirmationBroker {
    pending: Mutex<HashMap<String, PendingEntry>>,
    bus: ConfirmationBus,
    rules_store: Arc<SqliteRulesStore>,
}

/// Wire-level outcome of `resolve()` — surfaced back to
/// `ConfirmationResolveResult` by the IPC handler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolveOutcome {
    /// True iff the registry contained the id at resolve time.
    pub resolved: bool,
    /// What the daemon actually applied for `always_allow`. `None` for
    /// Allow / Deny, or when the requested override couldn't be applied.
    pub remembered_as: Option<RememberKind>,
}

impl IpcConfirmationBroker {
    /// Build a broker. The `rules_store` clone is for `always_allow`
    /// side effects; the `bus` is the same one cloned into the IPC
    /// handler's confirmation forwarder.
    pub fn new(rules_store: Arc<SqliteRulesStore>, bus: ConfirmationBus) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            bus,
            rules_store,
        }
    }

    /// Convenience constructor when callers don't need to share the bus.
    pub fn with_default_bus(rules_store: Arc<SqliteRulesStore>) -> (Self, ConfirmationBus) {
        let bus = ConfirmationBus::new();
        (Self::new(rules_store, bus.clone()), bus)
    }

    /// Resolve a pending confirmation. Returns
    /// `ResolveOutcome { resolved: false, .. }` when the id isn't in the
    /// registry (already resolved, timed out, or never existed).
    ///
    /// On `always_allow`:
    /// - `RememberKind::WatchlistAdd` — appends the item to the named
    ///   watchlist via the rules store. If the watchlist doesn't exist or
    ///   the SQLite op fails, the override is dropped and `remembered_as`
    ///   comes back `None` with a `tracing::warn`.
    /// - `RememberKind::ExceptionRule { name }` — creates a new Guard rule
    ///   with [`ActionType::Allow`] that matches on the originating action's
    ///   `data.action_type`, persisted via the rules store so future actions
    ///   of the same type are automatically allowed without a confirmation.
    pub fn resolve(
        &self,
        id: &str,
        decision: ConfirmationDecisionWire,
        remember: Option<RememberKind>,
    ) -> ResolveOutcome {
        let mut map = self.pending.lock().expect("confirmation registry poisoned");
        let Some(entry) = map.remove(id) else {
            return ResolveOutcome {
                resolved: false,
                remembered_as: None,
            };
        };
        drop(map);

        let mapped = match decision {
            ConfirmationDecisionWire::Allow | ConfirmationDecisionWire::AlwaysAllow => {
                ConfirmationDecision::Allow
            }
            ConfirmationDecisionWire::Deny => ConfirmationDecision::Deny,
        };

        // Apply the remember side effect only on AlwaysAllow.
        let remembered_as = if matches!(decision, ConfirmationDecisionWire::AlwaysAllow) {
            self.apply_remember(remember, &entry)
        } else {
            None
        };

        // Drop reference to entry's confirmation now that the side effect is done.
        let _ = entry.remembered_as; // unused — surfaced via resolve return

        if entry.tx.send(mapped).is_err() {
            // Broker side has dropped — gateway gave up waiting. Common
            // when the confirmation timed out between the registry
            // remove and the channel send. Not an error.
            tracing::debug!(id = %id, "resolve: broker oneshot already dropped");
        }

        ResolveOutcome {
            resolved: true,
            remembered_as,
        }
    }

    /// Snapshot of every pending confirmation. Used by
    /// `confirmation.list_pending` and by the IPC handler when a client
    /// (re)subscribes — the client uses `list_pending` to backfill the
    /// modal state before the subscription stream kicks in.
    pub fn list_pending(&self) -> Vec<PendingConfirmation> {
        self.pending
            .lock()
            .expect("confirmation registry poisoned")
            .values()
            .map(|e| e.confirmation.clone())
            .collect()
    }

    /// Enqueue a pending confirmation that was triggered by an event-bus rule
    /// match (not the gateway intercept path). Non-blocking: the confirmation
    /// is registered and published on the bus so `confirmation.list_pending`
    /// and `confirmation.subscribe` clients see it, but nothing awaits its
    /// resolution. When the user resolves it the decision is logged; there is
    /// no blocked action to unblock.
    ///
    /// Used by the matcher task when `ActionType::RequireConfirmation` fires
    /// on an ambient event (e.g. `url_changed` from the Tauri Cortex bridge).
    pub fn enqueue_confirmation(&self, pending: PendingConfirmation) {
        // The `_rx` side is intentionally dropped — nothing awaits on it.
        // When `resolve()` calls `entry.tx.send(…)` it gets
        // `is_err() == true` and logs a debug trace, which is correct
        // behaviour for this fire-and-forget path.
        let (tx, _rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().expect("confirmation registry poisoned");
            map.insert(
                pending.id.clone(),
                PendingEntry {
                    confirmation: pending.clone(),
                    tx,
                    remembered_as: None,
                },
            );
        }
        self.bus.publish(pending);
    }

    /// Bus reference (for forwarder spawn in the IPC handler).
    pub fn bus(&self) -> &ConfirmationBus {
        &self.bus
    }

    fn apply_remember(
        &self,
        remember: Option<RememberKind>,
        entry: &PendingEntry,
    ) -> Option<RememberKind> {
        let Some(kind) = remember else {
            tracing::debug!("always_allow with no remember kind — nothing to apply");
            return None;
        };
        match &kind {
            RememberKind::WatchlistAdd {
                watchlist_name,
                item,
            } => {
                // Pre-check that the watchlist exists; the store would
                // otherwise FK-violation on the insert and we'd lose
                // the descriptive error.
                if !self.rules_store.has_watchlist(watchlist_name) {
                    tracing::warn!(
                        watchlist = %watchlist_name,
                        "always_allow.watchlist_add: target watchlist does not exist; override dropped"
                    );
                    return None;
                }
                match self.rules_store.add_watchlist_item(watchlist_name, item) {
                    Ok(()) => {
                        tracing::info!(
                            watchlist = %watchlist_name,
                            item = %item,
                            "always_allow: applied watchlist_add"
                        );
                        Some(kind)
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            watchlist = %watchlist_name,
                            "always_allow.watchlist_add failed; override dropped"
                        );
                        None
                    }
                }
            }
            RememberKind::ExceptionRule { name } => {
                // Build an exception rule that explicitly allows the same
                // action_type as the originating action. The rule matches on
                // `kind = "agent_action_attempted"` AND
                // `data.action_type = <action_type from the originating action>`.
                //
                // The rule's `ActionType::Allow` gives it highest decision
                // precedence in `Decision::from_matches` — it will short-circuit
                // any `require_confirmation` or `veto` rules that might otherwise
                // match the same action in the future.
                let action_type_value: String = entry
                    .confirmation
                    .originating_action
                    .get("action_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let rule = Rule {
                    id: format!("exc_{}", Uuid::now_v7()),
                    name: name.clone(),
                    nl_original: format!("Always allow: {name}"),
                    kind: RuleKind::Guard,
                    enabled: true,
                    match_expr: Expression::all(vec![
                        Expression::leaf("kind", Operator::Eq, json!("agent_action_attempted")),
                        Expression::leaf(
                            "data.action_type",
                            Operator::Eq,
                            json!(action_type_value),
                        ),
                    ]),
                    action: Action {
                        action_type: ActionType::Allow,
                        webhook_id: None,
                        timeout_s: None,
                    },
                    cooldown_seconds: 0,
                    created_at: Utc::now(),
                };

                match self.rules_store.create_rule(rule) {
                    Ok(()) => {
                        tracing::info!(
                            rule_name = %name,
                            action_type = %action_type_value,
                            "always_allow.exception_rule: created exception rule"
                        );
                        Some(kind)
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            rule_name = %name,
                            "always_allow.exception_rule: store write failed; override dropped"
                        );
                        None
                    }
                }
            }
        }
    }
}

#[async_trait]
impl ConfirmationBroker for IpcConfirmationBroker {
    async fn request_confirmation(
        &self,
        req: ConfirmationRequest,
    ) -> Result<ConfirmationDecision, String> {
        let (tx, rx) = oneshot::channel();
        let pending = PendingConfirmation {
            id: req.id.clone(),
            created_at: req.created_at,
            expires_at: req.expires_at,
            rule: PendingRule {
                id: req.rule_id.clone(),
                name: req.rule_name.clone(),
                nl_original: req.rule_nl_original.clone(),
            },
            event: serde_json::to_value(&req.action)
                .ok()
                .map(|action| {
                    // Synthesise a minimal "event" view — the gateway
                    // already constructs an Event upstream; here we
                    // mirror the relevant shape for the UI modal.
                    serde_json::json!({
                        "ts": req.created_at.to_rfc3339(),
                        "source": "cel_act_gateway",
                        "kind": "agent_action_attempted",
                        "data": {
                            "action_type": req.action.action_type,
                            "action_args": req.action.action_args,
                            "caller": req.action.caller,
                        },
                        "_raw_action": action,
                    })
                })
                .unwrap_or_default(),
            originating_action: serde_json::to_value(&req.action).unwrap_or_default(),
            caller: req.action.caller.clone(),
            agent_session_id: req.action.agent_session_id.clone(),
        };

        // Register before publishing — otherwise a fast resolver could
        // race the registry insert.
        {
            let mut map = self.pending.lock().expect("confirmation registry poisoned");
            map.insert(
                req.id.clone(),
                PendingEntry {
                    confirmation: pending.clone(),
                    tx,
                    remembered_as: None,
                },
            );
        }
        self.bus.publish(pending);

        // Wait with a deadline. The expires_at is wall-clock; convert to
        // a monotonic timeout. If it's already past, await with zero
        // immediately yields a TimedOut.
        let wait = req
            .expires_at
            .signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or(Duration::from_secs(0));

        let decision = match tokio::time::timeout(wait, rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_)) => {
                // Sender dropped without sending — shouldn't happen but
                // surface as TimedOut so the gateway doesn't hang.
                tracing::warn!(
                    id = %req.id,
                    "confirmation broker oneshot dropped without resolve"
                );
                ConfirmationDecision::TimedOut
            }
            Err(_) => {
                // Real timeout. Sweep the registry.
                let mut map = self.pending.lock().expect("confirmation registry poisoned");
                map.remove(&req.id);
                tracing::info!(
                    id = %req.id,
                    "confirmation broker timed out before user resolved"
                );
                ConfirmationDecision::TimedOut
            }
        };

        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel_act_gateway::ProposedAction;
    use cellar_types::matcher::WatchlistLookup;
    use chrono::Duration as ChronoDuration;

    fn rules_store() -> Arc<SqliteRulesStore> {
        SqliteRulesStore::in_memory().unwrap()
    }

    fn sample_request(id: &str, expires_in_ms: i64) -> ConfirmationRequest {
        let now = Utc::now();
        ConfirmationRequest {
            id: id.into(),
            created_at: now,
            expires_at: now + ChronoDuration::milliseconds(expires_in_ms),
            rule_id: "rule_test".into(),
            rule_name: "Test guard".into(),
            rule_nl_original: "require confirmation for fs.copy outside workspace".into(),
            action: ProposedAction {
                caller: "embedded".into(),
                action_type: "fs.copy".into(),
                action_args: serde_json::json!({
                    "source_path": "/Users/x/Documents/secret.pdf",
                    "dest_path": "/Volumes/External/"
                }),
                agent_session_id: Some("sess_test".into()),
                project_root: None,
            },
        }
    }

    #[tokio::test]
    async fn resolve_allow_unblocks_broker() {
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store);
        let broker = Arc::new(broker);

        let b = broker.clone();
        let task =
            tokio::spawn(async move { b.request_confirmation(sample_request("c1", 5000)).await });

        // Give the broker a moment to register.
        tokio::task::yield_now().await;
        let outcome = broker.resolve("c1", ConfirmationDecisionWire::Allow, None);
        assert!(outcome.resolved);

        let decision = task.await.unwrap().unwrap();
        assert_eq!(decision, ConfirmationDecision::Allow);
    }

    #[tokio::test]
    async fn resolve_deny_unblocks_broker_with_deny() {
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store);
        let broker = Arc::new(broker);

        let b = broker.clone();
        let task =
            tokio::spawn(async move { b.request_confirmation(sample_request("c2", 5000)).await });
        tokio::task::yield_now().await;
        broker.resolve("c2", ConfirmationDecisionWire::Deny, None);

        let decision = task.await.unwrap().unwrap();
        assert_eq!(decision, ConfirmationDecision::Deny);
    }

    #[tokio::test]
    async fn timeout_returns_timed_out() {
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store);
        // 50 ms deadline — definitely expires before any resolve.
        let decision = broker
            .request_confirmation(sample_request("c3", 50))
            .await
            .unwrap();
        assert_eq!(decision, ConfirmationDecision::TimedOut);
        // Registry should be empty after the sweep.
        assert!(broker.list_pending().is_empty());
    }

    #[tokio::test]
    async fn resolve_unknown_id_returns_resolved_false() {
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store);
        let outcome = broker.resolve("ghost", ConfirmationDecisionWire::Allow, None);
        assert!(!outcome.resolved);
    }

    #[tokio::test]
    async fn list_pending_returns_active_confirmations() {
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store);
        let broker = Arc::new(broker);

        // Start two requests but don't resolve them.
        let b1 = broker.clone();
        let _t1 =
            tokio::spawn(async move { b1.request_confirmation(sample_request("c1", 5000)).await });
        let b2 = broker.clone();
        let _t2 =
            tokio::spawn(async move { b2.request_confirmation(sample_request("c2", 5000)).await });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        let pending = broker.list_pending();
        assert_eq!(pending.len(), 2);
        let ids: Vec<_> = pending.iter().map(|p| p.id.clone()).collect();
        assert!(ids.contains(&"c1".to_string()));
        assert!(ids.contains(&"c2".to_string()));
    }

    #[tokio::test]
    async fn always_allow_with_watchlist_add_applies_to_store() {
        let store = rules_store();
        // Pre-create the watchlist so the add succeeds.
        store.create_watchlist("approved", None).unwrap();

        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store.clone());
        let broker = Arc::new(broker);

        let b = broker.clone();
        let task =
            tokio::spawn(async move { b.request_confirmation(sample_request("cw", 5000)).await });
        tokio::task::yield_now().await;

        let outcome = broker.resolve(
            "cw",
            ConfirmationDecisionWire::AlwaysAllow,
            Some(RememberKind::WatchlistAdd {
                watchlist_name: "approved".into(),
                item: "fs.copy:secret.pdf".into(),
            }),
        );
        assert!(outcome.resolved);
        // Confirm the remember was applied.
        assert!(matches!(
            outcome.remembered_as,
            Some(RememberKind::WatchlistAdd { .. })
        ));
        assert!(store.contains("approved", "fs.copy:secret.pdf"));

        let decision = task.await.unwrap().unwrap();
        assert_eq!(decision, ConfirmationDecision::Allow);
    }

    #[tokio::test]
    async fn always_allow_with_missing_watchlist_logs_and_drops_override() {
        use cellar_types::matcher::WatchlistLookup;
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store.clone());
        let broker = Arc::new(broker);

        let b = broker.clone();
        let task =
            tokio::spawn(async move { b.request_confirmation(sample_request("cw2", 5000)).await });
        tokio::task::yield_now().await;

        let outcome = broker.resolve(
            "cw2",
            ConfirmationDecisionWire::AlwaysAllow,
            Some(RememberKind::WatchlistAdd {
                watchlist_name: "nonexistent".into(),
                item: "x".into(),
            }),
        );
        assert!(outcome.resolved);
        // Allow still went through even though remember failed.
        assert_eq!(task.await.unwrap().unwrap(), ConfirmationDecision::Allow);
        // Override dropped — no entry in the missing watchlist's cache.
        assert!(outcome.remembered_as.is_none());
        assert!(!store.contains("nonexistent", "x"));
    }

    #[tokio::test]
    async fn always_allow_with_exception_rule_creates_allow_rule() {
        // ExceptionRule path: broker creates a new rule with ActionType::Allow
        // that matches the same action_type as the originating action.
        let store = rules_store();
        let (broker, _bus) = IpcConfirmationBroker::with_default_bus(store.clone());
        let broker = Arc::new(broker);

        let b = broker.clone();
        let task =
            tokio::spawn(async move { b.request_confirmation(sample_request("cer", 5000)).await });
        tokio::task::yield_now().await;

        let outcome = broker.resolve(
            "cer",
            ConfirmationDecisionWire::AlwaysAllow,
            Some(RememberKind::ExceptionRule {
                name: "Always allow fs.copy".into(),
            }),
        );
        assert!(outcome.resolved);
        // The exception rule was created — remembered_as reflects what was applied.
        assert!(
            matches!(
                outcome.remembered_as,
                Some(RememberKind::ExceptionRule { .. })
            ),
            "expected ExceptionRule in remembered_as, got {:?}",
            outcome.remembered_as
        );

        // The exception rule now lives in the rules store.
        let rules = store.list_rules();
        let exc = rules.iter().find(|r| r.name == "Always allow fs.copy");
        assert!(exc.is_some(), "exception rule should be in rules store");
        let exc = exc.unwrap();
        assert_eq!(exc.action.action_type, ActionType::Allow);

        // Agent can continue.
        assert_eq!(task.await.unwrap().unwrap(), ConfirmationDecision::Allow);
    }
}
