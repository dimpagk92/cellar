//! [`HistoryStore`] trait + [`HistorySource`] — past-N-turns window source.
//!
//! Most agents keep a transcript of prior turns somewhere — in memory, on
//! disk, in a SQL database. [`HistoryStore`] is the small trait that lets
//! cel-brief read from any of them without caring how it's stored. The
//! companion [`HistorySource`] turns the last N entries into
//! [`Contribution`]s every turn at [`Priority::Normal`].
//!
//! The trait stays minimal on purpose: one async method that returns up to
//! `limit` recent entries in oldest-first order. Tool calls and tool results
//! are first-class so a model's prior decision-then-execution loop survives
//! the round trip.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source::{Contribution, ContributionContent, Source, SourceError};
use crate::types::{BriefContext, Priority, Role, SourceId};

/// A single entry in an agent's turn log.
///
/// One [`HistoryEntry`] becomes one [`crate::types::BriefMessage`] (modulo
/// budget pruning) in the assembled brief. The variants intentionally mirror
/// the subset of [`crate::source::ContributionContent`] that makes sense for
/// transcript replay; system prompts and tool *schemas* belong to other
/// sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryEntry {
    /// Plain text turn (user said X, assistant said Y).
    Text {
        /// Role this entry is attributed to.
        role: Role,
        /// Message body.
        content: String,
    },
    /// A tool invocation the model previously emitted.
    ToolCall {
        /// Provider-issued tool-call ID.
        id: String,
        /// Tool name.
        name: String,
        /// JSON arguments the model passed.
        args: Value,
    },
    /// The result returned for a prior tool call.
    ToolResult {
        /// Tool-call ID this result responds to.
        id: String,
        /// Serialised result content.
        content: String,
    },
}

impl HistoryEntry {
    /// Default token estimate for this entry — `length / 4`.
    pub fn estimated_tokens(&self) -> usize {
        match self {
            HistoryEntry::Text { content, .. } => content.len().div_ceil(4),
            HistoryEntry::ToolCall { name, args, .. } => {
                let args_len = serde_json::to_string(args).map(|s| s.len()).unwrap_or(0);
                (name.len() + args_len).div_ceil(4)
            }
            HistoryEntry::ToolResult { content, .. } => content.len().div_ceil(4),
        }
    }
}

/// Read-only access to an agent's turn log.
///
/// Implementations should:
/// - Return entries in **oldest-first** order so they slot into the brief in
///   chronological order.
/// - Cap the result at `limit` if their store has more than that many.
/// - Return an empty vec rather than an error when the store is empty.
///
/// The trait is generic over backend (in-memory `Vec`, SQLite table, Redis
/// list, …) and over what counts as a "turn". A computer-use agent might
/// store one entry per `ToolCall` + `ToolResult` pair; a chat agent stores
/// `Text` entries per message.
#[async_trait]
pub trait HistoryStore: Send + Sync {
    /// Return up to `limit` most-recent entries in oldest-first order.
    async fn recent(&self, limit: usize) -> Vec<HistoryEntry>;
}

#[async_trait]
impl HistoryStore for Vec<HistoryEntry> {
    /// Convenience impl — handy for tests and small fixed transcripts. The
    /// vec is treated as oldest-first; the tail (most recent) is taken.
    async fn recent(&self, limit: usize) -> Vec<HistoryEntry> {
        let start = self.len().saturating_sub(limit);
        self[start..].to_vec()
    }
}

/// A [`Source`] that injects the last N entries from a [`HistoryStore`].
///
/// Normal priority. The default importance is `0.4`, declining linearly
/// toward `0.2` for the oldest entry in the window — newer turns survive
/// pruning longer. Entries are emitted as redactable contributions so
/// governance can scrub PII before they reach the model.
#[derive(Debug, Clone)]
pub struct HistorySource<H: HistoryStore> {
    id: SourceId,
    store: H,
    limit: usize,
}

impl<H: HistoryStore> HistorySource<H> {
    /// Construct a history source with the default ID `"history"` and the
    /// given window size.
    ///
    /// A `limit` of `0` is allowed but the source will always emit zero
    /// contributions — register `HistorySource` only when you actually want
    /// transcript replay.
    pub fn new(store: H, limit: usize) -> Self {
        HistorySource {
            id: SourceId::new("history"),
            store,
            limit,
        }
    }

    /// Override the default [`SourceId`].
    pub fn with_id(mut self, id: impl Into<SourceId>) -> Self {
        self.id = id.into();
        self
    }

    /// The configured window size.
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[async_trait]
impl<H: HistoryStore> Source for HistorySource<H> {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::Normal
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        if self.limit == 0 {
            return Err(SourceError::Skipped("history limit is 0".into()));
        }
        let entries = self.store.recent(self.limit).await;
        if entries.is_empty() {
            return Err(SourceError::Skipped("history store is empty".into()));
        }

        let total = entries.len() as f32;
        Ok(entries
            .into_iter()
            .enumerate()
            .map(|(idx, entry)| {
                // Recency-weighted importance: oldest = 0.2, newest = 0.4.
                let recency = (idx as f32 + 1.0) / total;
                let importance = 0.2 + 0.2 * recency;
                let est = entry.estimated_tokens();
                let content = match entry {
                    HistoryEntry::Text { role, content } => {
                        ContributionContent::Text { role, content }
                    }
                    HistoryEntry::ToolCall { id, name, args } => {
                        ContributionContent::ToolCall { id, name, args }
                    }
                    HistoryEntry::ToolResult { id, content } => {
                        ContributionContent::ToolResult { id, content }
                    }
                };
                Contribution {
                    content,
                    estimated_tokens: est,
                    importance,
                    redactable: true,
                    tags: vec!["history".into()],
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenBudget;
    use serde_json::json;

    fn entry(role: Role, content: &str) -> HistoryEntry {
        HistoryEntry::Text {
            role,
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn vec_store_returns_tail_in_order() {
        let log: Vec<HistoryEntry> = vec![
            entry(Role::User, "oldest"),
            entry(Role::Assistant, "middle"),
            entry(Role::User, "newest"),
        ];
        let got = HistoryStore::recent(&log, 2).await;
        assert_eq!(got.len(), 2);
        match &got[0] {
            HistoryEntry::Text { content, .. } => assert_eq!(content, "middle"),
            _ => panic!(),
        }
        match &got[1] {
            HistoryEntry::Text { content, .. } => assert_eq!(content, "newest"),
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn vec_store_limit_above_len_returns_all() {
        let log: Vec<HistoryEntry> = vec![entry(Role::User, "only")];
        let got = HistoryStore::recent(&log, 99).await;
        assert_eq!(got.len(), 1);
    }

    #[tokio::test]
    async fn source_emits_one_contribution_per_entry() {
        let log: Vec<HistoryEntry> = vec![
            entry(Role::User, "hi"),
            entry(Role::Assistant, "hello!"),
            entry(Role::User, "more"),
        ];
        let src = HistorySource::new(log, 10);
        let ctx = BriefContext::new(TokenBudget::default());
        let cs = src.contribute(&ctx).await.expect("ok");
        assert_eq!(cs.len(), 3);
        // Newer entries should have higher importance than older ones.
        assert!(cs[2].importance > cs[0].importance);
        assert!(cs.iter().all(|c| c.redactable));
        assert!(cs.iter().all(|c| c.tags == vec!["history".to_owned()]));
    }

    #[tokio::test]
    async fn empty_store_is_skipped() {
        let src = HistorySource::new(Vec::<HistoryEntry>::new(), 10);
        let ctx = BriefContext::new(TokenBudget::default());
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn zero_limit_is_skipped() {
        let log: Vec<HistoryEntry> = vec![entry(Role::User, "hi")];
        let src = HistorySource::new(log, 0);
        let ctx = BriefContext::new(TokenBudget::default());
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_call_round_trips_through_history() {
        let log: Vec<HistoryEntry> = vec![
            HistoryEntry::ToolCall {
                id: "call_1".into(),
                name: "fs.copy".into(),
                args: json!({"src":"a","dst":"b"}),
            },
            HistoryEntry::ToolResult {
                id: "call_1".into(),
                content: "{\"ok\":true}".into(),
            },
        ];
        let src = HistorySource::new(log, 10);
        let ctx = BriefContext::new(TokenBudget::default());
        let cs = src.contribute(&ctx).await.expect("ok");
        assert_eq!(cs.len(), 2);
        match &cs[0].content {
            ContributionContent::ToolCall { id, name, .. } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "fs.copy");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match &cs[1].content {
            ContributionContent::ToolResult { id, content } => {
                assert_eq!(id, "call_1");
                assert_eq!(content, "{\"ok\":true}");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn priority_and_id_defaults() {
        let src = HistorySource::new(Vec::<HistoryEntry>::new(), 1);
        assert_eq!(src.priority(), Priority::Normal);
        assert_eq!(src.id(), SourceId::new("history"));
    }

    #[tokio::test]
    async fn with_id_overrides_default() {
        let src = HistorySource::new(Vec::<HistoryEntry>::new(), 1).with_id("chat_log");
        assert_eq!(src.id(), SourceId::new("chat_log"));
    }

    #[test]
    fn history_entry_round_trips_through_serde() {
        let e = HistoryEntry::ToolCall {
            id: "c1".into(),
            name: "t".into(),
            args: json!({"x":1}),
        };
        let json = serde_json::to_string(&e).expect("serialize");
        let back: HistoryEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(e, back);
    }
}
