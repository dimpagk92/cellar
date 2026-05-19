//! Memory session — a coherent unit of work.
//!
//! A chat conversation or a delegated job. Memory chunks tie back to a session
//! via [`MemoryChunk::session_id`]. The summarizer runs at end-of-session and
//! produces a `JobSummary` chunk per session.
//!
//! [`MemoryChunk::session_id`]: crate::MemoryChunk::session_id

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A session record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySession {
    /// uuid v7.
    pub id: String,
    /// When the session opened.
    pub started_at: DateTime<Utc>,
    /// When the session closed, if it has.
    pub ended_at: Option<DateTime<Utc>>,
    /// Normalised caller — `"embedded"`, `"mcp:cursor"`, etc.
    pub caller_id: String,
    /// Human-readable title (agent- or user-set).
    pub title: Option<String>,
    /// End-of-session synthesis. `None` until `close_session` has run and
    /// summarization (where available) has produced one.
    pub summary: Option<String>,
    /// Outcome of the session.
    pub outcome: SessionOutcome,
    /// Free-form metadata.
    #[serde(default)]
    pub metadata: Value,
}

/// Input to [`crate::MemoryProvider::open_session`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewMemorySession {
    /// Caller opening the session.
    pub caller_id: String,
    /// Optional title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional metadata.
    #[serde(default)]
    pub metadata: Value,
}

/// Outcome states for a session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    /// Still in progress. The session has been opened but not closed.
    Open,
    /// The session completed and the agent considers the goal achieved.
    Success,
    /// The session completed but the goal was not achieved.
    Failure,
    /// The session was aborted (user closed the window mid-job and didn't
    /// resume, agent self-terminated, etc.).
    Aborted,
}

/// Filter for [`crate::MemoryProvider::list_sessions`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionFilter {
    /// Restrict to sessions opened by a specific caller.
    #[serde(default)]
    pub caller_id: Option<String>,
    /// Restrict to sessions with a specific outcome.
    #[serde(default)]
    pub outcome: Option<SessionOutcome>,
    /// Lower bound on `started_at`.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// Upper bound on `started_at`.
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    /// If true, only return sessions still `Open`.
    #[serde(default)]
    pub open_only: bool,
    /// Maximum number of sessions to return. `None` = no limit.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outcome_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(SessionOutcome::Open).unwrap(),
            json!("open")
        );
        assert_eq!(
            serde_json::to_value(SessionOutcome::Aborted).unwrap(),
            json!("aborted")
        );
    }

    #[test]
    fn filter_defaults() {
        let f = SessionFilter::default();
        assert!(f.caller_id.is_none());
        assert!(f.outcome.is_none());
        assert!(!f.open_only);
        assert!(f.limit.is_none());
    }
}
