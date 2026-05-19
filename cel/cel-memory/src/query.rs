//! Query, profile, scope, and predicate types.
//!
//! [`MemoryQuery`] is the input to [`crate::MemoryProvider::retrieve`].
//! [`RetrievalProfile`] selects a tuned set of hybrid weights and kind filters
//! per caller path; [`CallerScope`] enforces multi-agent isolation.
//! [`MemoryPredicate`] describes the criteria for
//! [`crate::MemoryProvider::delete_matching`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::chunk::ChunkKind;

/// A retrieval query. See [`cellar-memory-manager.md`] §8.1.
///
/// [`cellar-memory-manager.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryQuery {
    /// Free-text query. Embedded for vector search and tokenized for FTS.
    pub text: String,
    /// Optional kind filter. `None` means all kinds.
    #[serde(default)]
    pub kinds: Option<Vec<ChunkKind>>,
    /// Optional lower bound on `created_at`.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// Optional upper bound on `created_at`.
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    /// Restrict to chunks in this session.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Multi-agent visibility scope.
    #[serde(default)]
    pub caller_scope: CallerScope,
    /// Restrict to chunks with a `project_root` prefix matching this string.
    #[serde(default)]
    pub project_root_prefix: Option<String>,
    /// Top-k results. Default 8.
    #[serde(default = "default_k")]
    pub k: usize,
    /// Include rollup chunks (`ChunkKind::Rollup`). Default true.
    #[serde(default = "default_true")]
    pub include_rollups: bool,
    /// Minimum importance to include. `None` means no floor.
    #[serde(default)]
    pub min_importance: Option<f32>,
    /// Retrieval profile selecting hybrid weights and per-caller defaults.
    #[serde(default)]
    pub profile: RetrievalProfile,
    /// Identifier of the caller performing the retrieval — used to enforce
    /// [`CallerScope::Own`] and to log access for relevance feedback.
    pub caller_id: String,
}

fn default_k() -> usize {
    8
}

fn default_true() -> bool {
    true
}

/// Multi-agent visibility scope for a query.
///
/// See [`cellar-memory-manager.md`] §13. Default scope for every external MCP
/// client is [`Own`]; the embedded agent gets [`OwnPlusShared`]; the Memory
/// tab UI and the audit timeline get [`Global`].
///
/// [`Own`]: CallerScope::Own
/// [`OwnPlusShared`]: CallerScope::OwnPlusShared
/// [`Global`]: CallerScope::Global
/// [`cellar-memory-manager.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CallerScope {
    /// Caller sees only chunks where `caller_id == self`.
    #[default]
    Own,
    /// Caller sees own chunks plus any chunk tagged shareable.
    OwnPlusShared,
    /// Caller sees everything. Privileged — granted to user surfaces only.
    Global,
}

/// Named retrieval profiles. Each profile sets a tuned set of hybrid weights
/// (vector / FTS / recency) and kind filters appropriate to the caller path.
///
/// The v1 [`crate::BasicMemoryProvider`] accepts the profile but does not
/// honor it (it always performs a single lexical match). The full Memory &
/// Context Manager implements per-profile tuning per
/// [`cellar-memory-manager.md`] §8.3.
///
/// [`cellar-memory-manager.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalProfile {
    /// Embedded agent's per-turn retrieval. Semantic-heavy, short recency
    /// half-life. The default for any call without a profile.
    #[default]
    AgentChatTurn,
    /// Embedded agent's delegated-job context. Heavier on long-term tier and
    /// job-summary chunks.
    AgentDelegatedJob,
    /// NL rule compiler authoring a new rule — wants similar prior rules.
    NLCompilerSimilarRules,
    /// NL rule compiler authoring a new rule — wants similar prior fires
    /// to a draft rule's match.
    NLCompilerSimilarFires,
    /// Audit / Activity tab — wide window, keyword-dominant.
    AuditTimeline,
    /// User free-text search in the Memory tab.
    UserSearch,
}

/// Criteria for [`crate::MemoryProvider::delete_matching`] (and reused as the
/// filter inside [`crate::ExportFilter`]).
///
/// Empty predicate matches nothing — `delete_matching(MemoryPredicate::default())`
/// is a safe no-op rather than a "delete everything" footgun. Use
/// [`crate::MemoryProvider::purge_all`] for that.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryPredicate {
    /// Match chunks whose kind is in this set.
    #[serde(default)]
    pub kinds: Option<Vec<ChunkKind>>,
    /// Match chunks written by callers in this set.
    #[serde(default)]
    pub callers: Option<Vec<String>>,
    /// Match chunks belonging to these sessions.
    #[serde(default)]
    pub session_ids: Option<Vec<String>>,
    /// Match chunks whose `project_root` begins with this prefix.
    #[serde(default)]
    pub project_root_prefix: Option<String>,
    /// Match chunks created before this instant.
    #[serde(default)]
    pub before: Option<DateTime<Utc>>,
    /// Match chunks created after this instant.
    #[serde(default)]
    pub after: Option<DateTime<Utc>>,
    /// If `Some(true)`, only pinned chunks; if `Some(false)`, only unpinned;
    /// `None` ignores pinning.
    #[serde(default)]
    pub pinned: Option<bool>,
    /// Match chunks with importance strictly less than this.
    #[serde(default)]
    pub importance_below: Option<f32>,
    /// Match chunks whose content contains this substring (case-insensitive
    /// in v1).
    #[serde(default)]
    pub content_contains: Option<String>,
}

impl MemoryPredicate {
    /// True if the predicate has no constraints — i.e. would match every
    /// chunk. [`crate::MemoryProvider::delete_matching`] short-circuits to a
    /// no-op when this returns true.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_none()
            && self.callers.is_none()
            && self.session_ids.is_none()
            && self.project_root_prefix.is_none()
            && self.before.is_none()
            && self.after.is_none()
            && self.pinned.is_none()
            && self.importance_below.is_none()
            && self.content_contains.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn caller_scope_default_is_own() {
        assert_eq!(CallerScope::default(), CallerScope::Own);
    }

    #[test]
    fn retrieval_profile_default_is_agent_chat_turn() {
        assert_eq!(RetrievalProfile::default(), RetrievalProfile::AgentChatTurn);
    }

    #[test]
    fn empty_predicate_is_empty() {
        assert!(MemoryPredicate::default().is_empty());
        let p = MemoryPredicate {
            content_contains: Some("hi".into()),
            ..Default::default()
        };
        assert!(!p.is_empty());
    }

    #[test]
    fn query_round_trip() {
        let q = MemoryQuery {
            text: "Q4 report".into(),
            kinds: Some(vec![ChunkKind::Chat, ChunkKind::Action]),
            since: None,
            until: None,
            session_id: Some("sess_abc".into()),
            caller_scope: CallerScope::OwnPlusShared,
            project_root_prefix: Some("/Users/dim/Workspace".into()),
            k: 8,
            include_rollups: true,
            min_importance: Some(0.3),
            profile: RetrievalProfile::AgentChatTurn,
            caller_id: "embedded".into(),
        };
        let s = serde_json::to_string(&q).unwrap();
        let back: MemoryQuery = serde_json::from_str(&s).unwrap();
        assert_eq!(q, back);
    }

    #[test]
    fn query_defaults_apply_on_deserialize() {
        let raw = json!({
            "text": "Q4 report",
            "caller_id": "embedded"
        })
        .to_string();
        let q: MemoryQuery = serde_json::from_str(&raw).unwrap();
        assert_eq!(q.k, 8);
        assert!(q.include_rollups);
        assert_eq!(q.profile, RetrievalProfile::AgentChatTurn);
        assert_eq!(q.caller_scope, CallerScope::Own);
    }
}
