//! The [`OffdeviceCallHook`] trait — the off-device call governance seam.
//!
//! Cloud-bound LLM calls made by the memory subsystem (summarizers,
//! embedders, anything that ships a chunk's content over the network) are
//! themselves governable events — shipping memory content off-device is a
//! privacy footgun. Before such a call dispatches, the producer calls
//! `OffdeviceCallHook::before_call(descriptor)`. The hook can:
//!
//! - return `Ok(OffdeviceDecision::Allow)` — the call proceeds.
//! - return `Ok(OffdeviceDecision::Veto { reason })` — the producer skips
//!   the network round-trip and surfaces a structured error to the caller.
//!   Used to honor rules like "never call Anthropic during work hours" or
//!   "require confirmation on first off-device memory call per session."
//! - return `Err(_)` — the call surfaces as a hard error to the caller.
//!
//! The daemon wires a hook backed by the rule matcher: it synthesises a
//! `MemoryOffdeviceCallAttempted` event from the descriptor, runs the rule
//! matcher over it, and returns the appropriate decision.
//!
//! Without a hook, producers always proceed — the test default and the
//! correct behavior for daemons that haven't wired the rule matcher path
//! yet.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Describes the off-device call a producer is about to make. Passed to
/// [`OffdeviceCallHook::before_call`] so rules can match on
/// kind / provider / model / subsystem.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OffdeviceCallDescriptor {
    /// What kind of call this is: `"summarizer"`, `"embedder"`,
    /// `"agent"`, etc. Drives the synthetic event's `data.kind` field.
    pub kind: String,
    /// Provider name as understood by the LLM router (`"anthropic"`,
    /// `"openai"`, `"voyage"`, …). Drives `data.provider`.
    pub provider: String,
    /// Model id (`"claude-haiku-4-5"`, `"voyage-3-large"`, …). Drives
    /// `data.model`.
    pub model: String,
    /// Which daemon subsystem initiated the call (`"memory_summarizer"`,
    /// `"memory_embedder"`, etc.). Drives `data.subsystem`.
    pub subsystem: String,
    /// Free-form metadata the producer wants to expose to the rule
    /// matcher. Inserted under `data.metadata.*`. Keep small — the
    /// matcher serialises every key/value pair on every match.
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl OffdeviceCallDescriptor {
    /// Convenience constructor. The most common shape: caller knows the
    /// kind, provider, model, and subsystem up front and has nothing
    /// extra to add.
    pub fn new(
        kind: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        subsystem: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            provider: provider.into(),
            model: model.into(),
            subsystem: subsystem.into(),
            metadata: BTreeMap::new(),
        }
    }

    /// Add a metadata field via builder-style chaining.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// What an [`OffdeviceCallHook`] tells the producer to do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OffdeviceDecision {
    /// Proceed with the network call. The default outcome.
    Allow,
    /// Skip the network call. The producer surfaces a structured error
    /// to its caller — for summarizers, this becomes
    /// `SummarizerError::Provider("off-device call vetoed: <reason>")`.
    Veto {
        /// Human-readable cause (typically the matched rule's name).
        reason: String,
    },
}

/// The hook producers consult before dispatching an off-device call.
///
/// Implementations must be cheap: this fires on every cloud call made
/// by the memory subsystem. The daemon's matcher-backed hook is
/// `O(rules)` — that's the v1 budget.
#[async_trait]
pub trait OffdeviceCallHook: Send + Sync {
    /// Decide what to do with the call. Default impl returns `Allow`,
    /// which makes wiring a hook optional — most consumers can ignore
    /// this trait entirely.
    async fn before_call(&self, _descriptor: &OffdeviceCallDescriptor) -> OffdeviceDecision {
        OffdeviceDecision::Allow
    }
}

/// Convenience wrapper for callers that want to plug a closure in
/// without declaring a struct. Useful for tests and for the daemon's
/// adapter that wraps the rule matcher.
pub struct ClosureOffdeviceHook<F>(pub F)
where
    F: Fn(&OffdeviceCallDescriptor) -> OffdeviceDecision + Send + Sync + 'static;

#[async_trait]
impl<F> OffdeviceCallHook for ClosureOffdeviceHook<F>
where
    F: Fn(&OffdeviceCallDescriptor) -> OffdeviceDecision + Send + Sync + 'static,
{
    async fn before_call(&self, descriptor: &OffdeviceCallDescriptor) -> OffdeviceDecision {
        (self.0)(descriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closure_hook_vetoes_anthropic_calls() {
        let hook = ClosureOffdeviceHook(|d: &OffdeviceCallDescriptor| {
            if d.provider == "anthropic" {
                OffdeviceDecision::Veto {
                    reason: "no anthropic calls".into(),
                }
            } else {
                OffdeviceDecision::Allow
            }
        });
        let d = OffdeviceCallDescriptor::new(
            "summarizer",
            "anthropic",
            "claude-haiku-4-5",
            "memory_summarizer",
        );
        match hook.before_call(&d).await {
            OffdeviceDecision::Veto { reason } => assert_eq!(reason, "no anthropic calls"),
            other => panic!("expected Veto, got {other:?}"),
        }
        let d2 =
            OffdeviceCallDescriptor::new("summarizer", "ollama", "llama3.2", "memory_summarizer");
        assert_eq!(hook.before_call(&d2).await, OffdeviceDecision::Allow);
    }

    #[tokio::test]
    async fn default_hook_allows_everything() {
        struct Default;
        #[async_trait]
        impl OffdeviceCallHook for Default {}

        let d = OffdeviceCallDescriptor::new("summarizer", "anthropic", "haiku", "memory");
        assert_eq!(Default.before_call(&d).await, OffdeviceDecision::Allow);
    }

    #[tokio::test]
    async fn descriptor_metadata_builder() {
        let d = OffdeviceCallDescriptor::new("summarizer", "anthropic", "haiku", "memory")
            .with_metadata("session_id", "s1")
            .with_metadata("chunk_count", 5);
        assert_eq!(d.metadata.len(), 2);
        assert_eq!(
            d.metadata.get("session_id").and_then(|v| v.as_str()),
            Some("s1")
        );
        assert_eq!(
            d.metadata.get("chunk_count").and_then(|v| v.as_i64()),
            Some(5)
        );
    }
}
