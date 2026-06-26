//! Memory chunk — the primary persisted unit.
//!
//! One row in `memory_chunks`. Every category of memory (chat / action / fire /
//! observation / correction / job_summary / context / rollup) is a chunk, with
//! the [`kind`] discriminator and kind-specific structured fields living in
//! [`metadata`]. The single embedded/FTS-indexed text is [`content`].
//!
//! The corresponding SQL schema (the `memory_chunks` table and its indexes)
//! lives in the `cel-memory-sqlite` crate's migrations.
//!
//! [`kind`]: MemoryChunk::kind
//! [`metadata`]: MemoryChunk::metadata
//! [`content`]: MemoryChunk::content

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A persisted memory unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryChunk {
    /// Time-ordered ID. uuid v7 in v1.
    pub id: String,
    /// When the chunk was written.
    pub created_at: DateTime<Utc>,
    /// Discriminator for kind-specific handling and retrieval filtering.
    pub kind: ChunkKind,
    /// Tier this chunk lives in: working (never persisted, so never seen
    /// here), session (recent), or long_term (aged out / summarized).
    pub tier: MemoryTier,
    /// Where the chunk came from.
    pub source: ChunkSource,
    /// Conversation or delegated-job grouping. `None` for ambient chunks
    /// like observations and rule firings.
    pub session_id: Option<String>,
    /// Working directory or project this chunk is scoped to, if known.
    pub project_root: Option<String>,
    /// Normalised caller — for example `"agent"`, `"mcp:codex"`,
    /// `"gateway"`, `"policy"`, `"perception"`, `"system"`.
    pub caller_id: String,
    /// The human-readable text indexed by FTS and embedded.
    pub content: String,
    /// Kind-specific structured fields.
    pub metadata: Value,
    /// Importance score in [0, 1]. Drives eviction priority.
    pub importance: f32,
    /// If true, never auto-evicted regardless of importance or age.
    pub pinned: bool,
    /// Cross-caller visibility flag. When `true`, the chunk surfaces to
    /// every caller whose query uses [`crate::CallerScope::OwnPlusShared`]
    /// (in addition to the writer's own scope). When `false` (the default),
    /// the chunk is only visible to its writer under `Own`/`OwnPlusShared`
    /// and to privileged surfaces under `Global`. See [`crate::CallerScope`]
    /// for the full visibility model.
    #[serde(default)]
    pub shareable: bool,
    /// If non-`None`, the ID of a chunk that replaces this one (e.g. a
    /// correction supersedes the original mistake).
    pub superseded_by: Option<String>,
    /// Name of the embedding model used (e.g. `"bge-small-en-v1.5"`). The v1
    /// `BasicMemoryProvider` records `"none"` because it doesn't embed.
    pub embedding_model: String,
    /// Dimensionality of the embedding. `0` for the v1 stub.
    pub embedding_dim: u32,
}

/// Input to [`crate::MemoryProvider::write`]. The fields the provider fills in
/// (id, created_at, tier, embedding model+dim) are absent here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewMemoryChunk {
    /// See [`MemoryChunk::kind`].
    pub kind: ChunkKind,
    /// See [`MemoryChunk::source`].
    pub source: ChunkSource,
    /// See [`MemoryChunk::session_id`].
    pub session_id: Option<String>,
    /// See [`MemoryChunk::project_root`].
    pub project_root: Option<String>,
    /// See [`MemoryChunk::caller_id`].
    pub caller_id: String,
    /// See [`MemoryChunk::content`].
    pub content: String,
    /// See [`MemoryChunk::metadata`]. Defaults to JSON `null` if omitted.
    #[serde(default)]
    pub metadata: Value,
    /// Optional caller-supplied importance hint. The provider may clamp or
    /// override based on its own scorer; defaults to `0.5` when `None`.
    #[serde(default)]
    pub importance: Option<f32>,
    /// Mark this chunk as cross-caller shareable. Defaults to `false`.
    #[serde(default)]
    pub shareable: bool,
    /// Pin from creation (rare; usually set later via `pin`).
    #[serde(default)]
    pub pinned: bool,
}

/// Categories of memory. Drives kind-filtered retrieval and per-kind
/// retention horizons.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChunkKind {
    /// A chat message between the user and an agent.
    Chat,
    /// An action call: attempted, completed, or denied.
    Action,
    /// A rule firing.
    Fire,
    /// A runtime observation the importance scorer flagged as significant.
    Observation,
    /// A user correction or override (confirmation modal Deny, "don't do that
    /// again" chat message, etc.). Highest-signal kind. Never auto-evicted.
    Correction,
    /// End-of-session synthesis: goal, plan, actions, outcome, surprises.
    JobSummary,
    /// File / app / URL focus episode.
    Context,
    /// A rollup that covers many other chunks. Linked via the summary-members
    /// table in the storage layer.
    Rollup,
}

/// Where the chunk came from. Distinct from `caller_id`: `source` is the
/// producer category, while `caller_id` is the originator of the underlying
/// activity (for example an agent id, client id, or service name).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChunkSource {
    /// An embedded or in-process agent.
    Embedded,
    /// An external MCP client (Cursor, Codex, Claude Desktop, etc.). The
    /// specific client lives in `caller_id`.
    Mcp,
    /// An action gateway, recording an attempted/completed/denied action.
    Gateway,
    /// A policy or rule matcher post-fire hook.
    Matcher,
    /// A perception or observation runtime.
    Perception,
    /// System-level metadata (settings changes, lifecycle events, etc.).
    System,
}

/// Which tier a chunk currently lives in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    /// Recent (within the session horizon). Raw rows.
    Session,
    /// Older. Mostly summarized.
    LongTerm,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chunk_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(ChunkKind::JobSummary).unwrap(),
            json!("job_summary")
        );
        let back: ChunkKind = serde_json::from_value(json!("rollup")).unwrap();
        assert_eq!(back, ChunkKind::Rollup);
    }

    #[test]
    fn chunk_source_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(ChunkSource::Gateway).unwrap(),
            json!("gateway")
        );
    }

    #[test]
    fn new_chunk_defaults() {
        let raw = r#"{"kind":"chat","source":"embedded","caller_id":"embedded","content":"hi","session_id":null,"project_root":null}"#;
        let n: NewMemoryChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(n.kind, ChunkKind::Chat);
        assert_eq!(n.importance, None);
        assert!(!n.shareable);
        assert!(!n.pinned);
        assert_eq!(n.metadata, Value::Null);
    }

    #[test]
    fn chunk_round_trip() {
        let c = MemoryChunk {
            id: "01950000-0000-7000-8000-000000000001".into(),
            created_at: Utc::now(),
            kind: ChunkKind::Action,
            tier: MemoryTier::Session,
            source: ChunkSource::Gateway,
            session_id: Some("sess_abc".into()),
            project_root: Some("/Users/dim/Workspace".into()),
            caller_id: "embedded".into(),
            content: "Agent attempted fs.copy".into(),
            metadata: json!({"action_type":"fs.copy"}),
            importance: 0.7,
            pinned: false,
            shareable: false,
            superseded_by: None,
            embedding_model: "none".into(),
            embedding_dim: 0,
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: MemoryChunk = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn chunk_shareable_default_false_on_deserialize() {
        // Pre-Phase-4 callers that constructed JSON without the `shareable`
        // field must still round-trip — shareable defaults to false on the
        // wire. (Other fields are still required.)
        let raw = r#"{"id":"x","created_at":"2026-01-01T00:00:00Z","kind":"chat",
            "tier":"session","source":"embedded","session_id":null,
            "project_root":null,"caller_id":"embedded",
            "content":"hi","metadata":null,"importance":0.5,"pinned":false,
            "superseded_by":null,"embedding_model":"none","embedding_dim":0}"#;
        let c: MemoryChunk = serde_json::from_str(raw).unwrap();
        assert!(!c.shareable);
    }
}
