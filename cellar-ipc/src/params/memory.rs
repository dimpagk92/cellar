//! `memory.*` request parameters — the Phase 4 surface that backs the
//! `cel_remember` / `cel_recall` / `cel_forget` MCP tools and any IPC
//! consumer that needs to drive the daemon's [`MemoryProvider`].
//!
//! Wire encoding mirrors the in-process trait surface from
//! [`cel_memory`] so the daemon-side handler can shape param structs into
//! [`cel_memory::NewMemoryChunk`] / [`cel_memory::MemoryQuery`] /
//! [`cel_memory::MemoryPredicate`] with negligible glue.
//!
//! [`cel_memory`]: https://docs.rs/cel-memory

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Params for `memory.remember`.
///
/// `caller_id` is **not** carried on the wire — the daemon resolves the
/// caller from the connection identity and stamps it on the persisted
/// chunk. This keeps the contract honest: an MCP client cannot impersonate
/// the embedded agent or another tool by sending a forged `caller_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryRememberParams {
    /// Free-text content. Required; empty/whitespace-only content is
    /// rejected with `ValidationFailed`.
    pub content: String,
    /// Optional caller-supplied identity. The daemon prefers a server-side
    /// caller_id (resolved from the connection) when one is available; this
    /// field is a hint used only when the IPC connection itself doesn't
    /// carry an identity (e.g. one-shot CLI calls). Always prefixed with
    /// `"mcp:"` before storage when supplied by an MCP client to make the
    /// origin auditable in the Memory tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<String>,
    /// Chunk kind. Defaults to `chat` when omitted — matches the most
    /// common MCP-client use case (durable assistant context).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional session this chunk belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional project / workspace scope. The Memory tab filters by
    /// `project_root` prefix; setting it here makes the chunk show up
    /// under the right project view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Tags propagated into chunk metadata under the `tags` key. The
    /// provider keeps them addressable for predicate-based forget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Caller-supplied importance hint in `[0.0, 1.0]`. Out-of-range values
    /// are clamped before storage. Omit to let the provider's heuristic score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<f32>,
    /// When `true`, the chunk surfaces to every caller whose query uses
    /// `OwnPlusShared` (in addition to the writer's own scope). Defaults to
    /// `false` — chunks are caller-private unless explicitly shared. See
    /// [`cellar-memory-manager.md`] §13.1.
    ///
    /// [`cellar-memory-manager.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md
    #[serde(default)]
    pub shareable: bool,
    /// Pin the chunk from creation. Pinned chunks are never auto-evicted.
    #[serde(default)]
    pub pinned: bool,
}

/// Params for `memory.recall`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryRecallParams {
    /// Free-text query. Embedded for vector search and tokenized for FTS.
    pub query: String,
    /// Optional caller-supplied identity. As in [`MemoryRememberParams`],
    /// the server prefers the connection-resolved caller_id when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<String>,
    /// Top-k results. Defaults to 8 when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Optional kind filter. Each entry must be a known chunk kind
    /// (`chat`, `action`, `fire`, `observation`, `correction`,
    /// `job_summary`, `context`, `rollup`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Multi-agent visibility scope. Defaults to `own`. Allowed values:
    /// `own`, `own_plus_shared`, `global`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Minimum importance to include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_importance: Option<f32>,
    /// Optional lower bound on `created_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Restrict to chunks belonging to this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Restrict to chunks whose `project_root` begins with this prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root_prefix: Option<String>,
}

/// Params for `memory.forget`.
///
/// Exactly one of `chunk_ids` or `predicate` should be supplied — the
/// daemon's handler errors with `ValidationFailed` otherwise. The
/// predicate is intentionally limited to a small set of cheap-to-evaluate
/// criteria (`kind`, `older_than`, `tag`) to keep this surface from
/// becoming a back-door for arbitrary SQL.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryForgetParams {
    /// Optional caller-supplied identity. Server-resolved identity wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<String>,
    /// Exact chunk IDs to delete. Each chunk must be owned by the caller
    /// (or the call must come from a privileged surface — Memory tab,
    /// Audit timeline) or the daemon returns `NotAuthorized`. This check
    /// lives in the daemon-side handler, not the trait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_ids: Option<Vec<String>>,
    /// Predicate-based delete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<MemoryForgetPredicate>,
}

/// Predicate for [`MemoryForgetParams::predicate`]. Empty predicate is a
/// no-op (matches the [`cel_memory::MemoryPredicate::is_empty`] guard).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryForgetPredicate {
    /// Delete chunks of these kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Vec<String>>,
    /// Delete chunks created strictly before this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<DateTime<Utc>>,
    /// Delete chunks whose `metadata.tags` contains this tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

impl MemoryForgetPredicate {
    /// True when no field is set — the daemon short-circuits to a no-op
    /// in that case (mirrors the in-process predicate guard).
    pub fn is_empty(&self) -> bool {
        self.kind.is_none() && self.older_than.is_none() && self.tag.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn remember_defaults_apply_on_deserialize() {
        let p: MemoryRememberParams = serde_json::from_value(json!({"content": "hi"})).unwrap();
        assert_eq!(p.content, "hi");
        assert!(p.kind.is_none());
        assert!(!p.shareable);
        assert!(!p.pinned);
        assert!(p.caller_id.is_none());
    }

    #[test]
    fn recall_defaults_apply_on_deserialize() {
        let p: MemoryRecallParams = serde_json::from_value(json!({"query": "q4"})).unwrap();
        assert_eq!(p.query, "q4");
        assert!(p.limit.is_none());
        assert!(p.scope.is_none());
    }

    #[test]
    fn forget_predicate_is_empty_default() {
        assert!(MemoryForgetPredicate::default().is_empty());
        let p = MemoryForgetPredicate {
            tag: Some("foo".into()),
            ..Default::default()
        };
        assert!(!p.is_empty());
    }

    #[test]
    fn forget_params_skip_none() {
        let p = MemoryForgetParams::default();
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, "{}");
    }
}
