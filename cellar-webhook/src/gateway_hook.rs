//! `GatewayHook` — adapter that satisfies [`cel_act_gateway::WebhookHook`].
//!
//! The gateway holds an `Option<Arc<dyn WebhookHook>>`. Wiring `cellar-webhook`
//! into the gateway is a one-liner:
//!
//! ```ignore
//! let svc = WebhookService::spawn(/* ... */);
//! let hook = Arc::new(GatewayHook::new(Arc::new(svc)));
//! let gw = Gateway::new(/* ... */).with_webhook_hook(hook);
//! ```
//!
//! On each fire whose `action_type == Webhook`, the gateway calls
//! `hook.deliver(snapshot, event)`. The adapter destructures the
//! `FiredRuleSnapshot` and delegates to
//! [`crate::WebhookService::enqueue_parts`]. Failures are logged via
//! `tracing::warn!` and swallowed — the gateway's contract is "best-effort
//! fan-out, never block the hot path".

use std::sync::Arc;

use async_trait::async_trait;
use cel_act_gateway::{FiredRuleSnapshot, WebhookHook};
use cellar_types::Event;

use crate::sender::Sender;
use crate::service::WebhookService;

/// Adapter that turns a [`WebhookService`] into a [`WebhookHook`] the
/// gateway can hold.
pub struct GatewayHook<S: Sender + 'static> {
    service: Arc<WebhookService<S>>,
}

impl<S: Sender + 'static> GatewayHook<S> {
    /// Build the adapter around an existing service.
    pub fn new(service: Arc<WebhookService<S>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<S: Sender + 'static> WebhookHook for GatewayHook<S> {
    async fn deliver(&self, fire: &FiredRuleSnapshot, event: &Event) {
        let webhook_id = fire.webhook_id.as_deref().unwrap_or("default");
        if let Err(e) = self.service.enqueue_parts(
            &fire.rule_id,
            &fire.rule_name,
            &fire.rule_nl_original,
            webhook_id,
            event,
        ) {
            tracing::warn!(
                error = %e,
                rule_id = %fire.rule_id,
                rule_name = %fire.rule_name,
                webhook_id,
                "webhook enqueue failed; dropping"
            );
        }
    }
}
