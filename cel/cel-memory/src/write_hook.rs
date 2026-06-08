//! The [`MemoryWriteHook`] trait — the per-write governance seam.
//!
//! Memory writes are themselves governable events. Before a provider
//! persists a chunk, it calls `MemoryWriteHook::before_write(chunk)`. The
//! hook can:
//!
//! - return `Ok(WriteDecision::Allow)` — the provider persists the chunk
//!   verbatim.
//! - return `Ok(WriteDecision::Redact { reason })` — the provider drops
//!   the chunk silently (the `before_write` event still gets matcher
//!   coverage for audit; the chunk just doesn't land in `memory_chunks`).
//!   Used to honor `redact_memory` rules.
//! - return `Err(_)` — the write surfaces as a hard error to the caller.
//!
//! The daemon wires a hook backed by the rule matcher: it synthesises a
//! `MemoryWriteAttempted` event from the chunk's kind, source, caller, and
//! content prefix, runs the rule matcher over it, and returns the
//! appropriate decision based on what fired (Veto → Redact; LogOnly → Allow
//! but record the fire).
//!
//! Without a hook, providers persist every write — the v1 default for
//! tests and for daemons that haven't wired the rule matcher path yet.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::chunk::NewMemoryChunk;
use crate::error::Result;

/// What a [`MemoryWriteHook`] tells the provider to do with an incoming
/// chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteDecision {
    /// Persist the chunk verbatim. The default outcome.
    Allow,
    /// Drop the chunk before persistence. The provider returns success
    /// to the caller (so write paths don't need to special-case redaction)
    /// but the chunk never lands in `memory_chunks`. The reason string
    /// surfaces in logs and in any `record_access`-style audit trail.
    Redact {
        /// Human-readable cause (typically the matched rule's name).
        reason: String,
    },
}

/// The hook providers consult before persisting a chunk.
///
/// Implementations must be cheap: this fires on every write through the
/// memory subsystem. The daemon's matcher-backed hook is `O(rules)` — that's
/// the v1 budget.
#[async_trait]
pub trait MemoryWriteHook: Send + Sync {
    /// Decide what to do with the chunk. Default impl returns `Allow`,
    /// which makes wiring a hook optional — most consumers can ignore
    /// this trait entirely.
    async fn before_write(&self, _chunk: &NewMemoryChunk) -> Result<WriteDecision> {
        Ok(WriteDecision::Allow)
    }
}

/// Convenience wrapper for callers that want to plug a closure in without
/// declaring a struct. Useful for tests and for the daemon's adapter that
/// wraps the rule matcher.
pub struct ClosureHook<F>(pub F)
where
    F: Fn(&NewMemoryChunk) -> WriteDecision + Send + Sync + 'static;

#[async_trait]
impl<F> MemoryWriteHook for ClosureHook<F>
where
    F: Fn(&NewMemoryChunk) -> WriteDecision + Send + Sync + 'static,
{
    async fn before_write(&self, chunk: &NewMemoryChunk) -> Result<WriteDecision> {
        Ok((self.0)(chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkKind, ChunkSource};

    fn nc(content: &str) -> NewMemoryChunk {
        NewMemoryChunk {
            kind: ChunkKind::Chat,
            source: ChunkSource::Embedded,
            session_id: None,
            project_root: None,
            caller_id: "test".into(),
            content: content.into(),
            metadata: serde_json::Value::Null,
            importance: None,
            shareable: false,
            pinned: false,
        }
    }

    #[tokio::test]
    async fn closure_hook_redacts_matching_content() {
        let hook = ClosureHook(|c: &NewMemoryChunk| {
            if c.content.contains("secret") {
                WriteDecision::Redact {
                    reason: "secret keyword".into(),
                }
            } else {
                WriteDecision::Allow
            }
        });
        let chunk = nc("a totally innocent chat");
        assert_eq!(
            hook.before_write(&chunk).await.unwrap(),
            WriteDecision::Allow
        );
        let chunk = nc("contains a secret");
        match hook.before_write(&chunk).await.unwrap() {
            WriteDecision::Redact { reason } => assert_eq!(reason, "secret keyword"),
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_hook_allows_everything() {
        struct Default;
        #[async_trait]
        impl MemoryWriteHook for Default {}

        let chunk = nc("anything");
        assert_eq!(
            Default.before_write(&chunk).await.unwrap(),
            WriteDecision::Allow
        );
    }
}
