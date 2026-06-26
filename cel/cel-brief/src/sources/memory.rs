//! [`MemorySource`] — hybrid retrieval at agent-think-time over any
//! [`cel_memory::MemoryProvider`]. **Gated by the `memory` cargo feature.**
//!
//! Builds a [`cel_memory::MemoryQuery`] from [`BriefContext`] every turn,
//! calls [`MemoryProvider::retrieve`], and emits the returned
//! [`cel_memory::MemoryChunk`]s as redactable [`Role::System`] text
//! contributions at [`Priority::Normal`].
//!
//! The query text is built from the highest-signal field on
//! [`BriefContext`] — preferring `user_message`, falling back to `goal`,
//! finally an empty string (which most providers treat as a recency-only
//! retrieval). Override via [`MemorySource::with_query_text_extractor`] when
//! you want a custom rule (e.g. concatenate goal + last assistant turn).

use std::sync::Arc;

use async_trait::async_trait;
use cel_memory::{CallerScope, ChunkKind, MemoryProvider, MemoryQuery, RetrievalProfile};

use crate::source::{Contribution, ContributionContent, Source, SourceError};
use crate::types::{BriefContext, Priority, Role, SourceId};

/// How [`MemorySource`] turns a [`BriefContext`] into query text.
///
/// The strategies are convenience wrappers around the most common patterns;
/// for anything else use [`MemorySource::with_query_text_extractor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryQueryStrategy {
    /// `user_message` if present, else `goal`, else empty. The default.
    #[default]
    UserMessageThenGoal,
    /// Use `user_message` only; skip the source if `None`.
    UserMessageOnly,
    /// Use `goal` only; skip the source if `None`.
    GoalOnly,
}

impl MemoryQueryStrategy {
    /// Apply the strategy to a [`BriefContext`], returning the query text
    /// or `None` if the source should be skipped.
    fn extract<'a>(&self, ctx: &'a BriefContext) -> Option<&'a str> {
        match self {
            MemoryQueryStrategy::UserMessageThenGoal => ctx
                .user_message
                .as_deref()
                .or(ctx.goal.as_deref())
                .or(Some("")),
            MemoryQueryStrategy::UserMessageOnly => ctx.user_message.as_deref(),
            MemoryQueryStrategy::GoalOnly => ctx.goal.as_deref(),
        }
    }
}

/// Custom extractor signature for [`MemorySource::with_query_text_extractor`].
///
/// Return `None` to skip retrieval this turn.
pub type QueryTextExtractor = Arc<dyn for<'a> Fn(&'a BriefContext) -> Option<String> + Send + Sync>;

/// A [`Source`] that injects retrieved memories into the brief.
///
/// Wraps any `Arc<dyn MemoryProvider>`. Normal priority — memories are
/// useful context, but they should yield to the system prompt, the user's
/// message, and the tool catalog under budget pressure.
pub struct MemorySource {
    id: SourceId,
    provider: Arc<dyn MemoryProvider>,
    caller_id: String,
    k: usize,
    strategy: MemoryQueryStrategy,
    profile: RetrievalProfile,
    caller_scope: CallerScope,
    kinds: Option<Vec<ChunkKind>>,
    custom_extractor: Option<QueryTextExtractor>,
}

impl std::fmt::Debug for MemorySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySource")
            .field("id", &self.id)
            .field("caller_id", &self.caller_id)
            .field("k", &self.k)
            .field("strategy", &self.strategy)
            .field("profile", &self.profile)
            .field("caller_scope", &self.caller_scope)
            .field("kinds", &self.kinds)
            .field(
                "custom_extractor",
                &self.custom_extractor.as_ref().map(|_| "<fn>"),
            )
            .finish()
    }
}

impl MemorySource {
    /// Construct a memory source.
    ///
    /// - `provider` — the backing [`MemoryProvider`], held as a trait object so
    ///   callers that keep an `Arc<dyn MemoryProvider>` can wire it directly.
    ///   Concrete `Arc<ConcreteProvider>` values coerce automatically at the
    ///   call site.
    /// - `caller_id` — value used for `MemoryQuery.caller_id`. This is the
    ///   ID the memory subsystem uses for access logs and scope enforcement;
    ///   it should match the caller name your provider expects (e.g.
    ///   `"embedded"`, `"mcp:codex"`).
    /// - `k` — top-K chunks to retrieve per turn.
    pub fn new(provider: Arc<dyn MemoryProvider>, caller_id: impl Into<String>, k: usize) -> Self {
        MemorySource {
            id: SourceId::new("memory"),
            provider,
            caller_id: caller_id.into(),
            k,
            strategy: MemoryQueryStrategy::default(),
            profile: RetrievalProfile::default(),
            caller_scope: CallerScope::default(),
            kinds: None,
            custom_extractor: None,
        }
    }

    /// Override the default [`SourceId`].
    pub fn with_id(mut self, id: impl Into<SourceId>) -> Self {
        self.id = id.into();
        self
    }

    /// Override the default query-text extraction strategy.
    pub fn with_strategy(mut self, strategy: MemoryQueryStrategy) -> Self {
        self.strategy = strategy;
        self.custom_extractor = None;
        self
    }

    /// Plug in a fully custom query-text extractor — overrides any
    /// [`MemoryQueryStrategy`] previously set.
    pub fn with_query_text_extractor<F>(mut self, extractor: F) -> Self
    where
        F: for<'a> Fn(&'a BriefContext) -> Option<String> + Send + Sync + 'static,
    {
        self.custom_extractor = Some(Arc::new(extractor));
        self
    }

    /// Override the retrieval profile (default
    /// [`RetrievalProfile::AgentChatTurn`]).
    pub fn with_profile(mut self, profile: RetrievalProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Override the caller scope (default [`CallerScope::Own`]).
    pub fn with_caller_scope(mut self, scope: CallerScope) -> Self {
        self.caller_scope = scope;
        self
    }

    /// Filter retrieval to specific [`ChunkKind`]s. `None` (default) returns
    /// all kinds.
    pub fn with_kinds(mut self, kinds: Option<Vec<ChunkKind>>) -> Self {
        self.kinds = kinds;
        self
    }

    fn build_query_text(&self, ctx: &BriefContext) -> Option<String> {
        if let Some(extractor) = &self.custom_extractor {
            return extractor(ctx);
        }
        self.strategy.extract(ctx).map(|s| s.to_owned())
    }
}

#[async_trait]
impl Source for MemorySource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::Normal
    }

    async fn contribute(&self, ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        if self.k == 0 {
            return Err(SourceError::Skipped("k is 0".into()));
        }
        let Some(text) = self.build_query_text(ctx) else {
            return Err(SourceError::Skipped("no query text".into()));
        };

        let query = MemoryQuery {
            text,
            kinds: self.kinds.clone(),
            since: None,
            until: None,
            session_id: None,
            caller_scope: self.caller_scope,
            project_root_prefix: None,
            k: self.k,
            include_rollups: true,
            min_importance: None,
            profile: self.profile,
            caller_id: self.caller_id.clone(),
        };

        let chunks = self
            .provider
            .retrieve(query)
            .await
            .map_err(|e| SourceError::Backend(format!("memory retrieve: {e}")))?;

        if chunks.is_empty() {
            return Err(SourceError::Skipped("no matching memories".into()));
        }

        Ok(chunks
            .into_iter()
            .map(|chunk| {
                let est = chunk.content.len().div_ceil(4);
                let prefix = format!("[memory:{:?}] ", chunk.kind).to_lowercase();
                let body = format!("{prefix}{}", chunk.content);
                let importance = chunk.importance.clamp(0.0, 1.0).max(0.3);
                Contribution {
                    content: ContributionContent::Text {
                        role: Role::System,
                        content: body,
                    },
                    estimated_tokens: est + prefix.len().div_ceil(4),
                    importance,
                    redactable: true,
                    tags: vec!["memory".into()],
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenBudget;
    use cel_memory::{BasicMemoryProvider, ChunkSource, NewMemoryChunk, NewMemorySession};
    use serde_json::json;

    async fn seeded_provider() -> Arc<BasicMemoryProvider> {
        let p = Arc::new(BasicMemoryProvider::new());
        let session = p
            .open_session(NewMemorySession {
                caller_id: "test".into(),
                title: None,
                metadata: json!(null),
            })
            .await
            .expect("open session");
        for content in [
            "User prefers dry-run mode",
            "Q4 report under ~/Workspace/q4.md",
            "Last action: copy draft.md to Workspace",
        ] {
            p.write(NewMemoryChunk {
                kind: ChunkKind::Chat,
                source: ChunkSource::Embedded,
                session_id: Some(session.id.clone()),
                project_root: None,
                caller_id: "test".into(),
                content: content.into(),
                metadata: json!(null),
                importance: Some(0.5),
                shareable: false,
                pinned: false,
            })
            .await
            .expect("write");
        }
        p
    }

    #[tokio::test]
    async fn retrieves_chunks_under_default_strategy() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5);

        let ctx = BriefContext::new(TokenBudget::default()).with_user_message("dry-run");
        let cs = src.contribute(&ctx).await.expect("ok");
        assert!(!cs.is_empty());
        for c in &cs {
            assert!(c.redactable);
            assert_eq!(c.tags, vec!["memory".to_owned()]);
            match &c.content {
                ContributionContent::Text { role, content } => {
                    assert_eq!(*role, Role::System);
                    assert!(content.starts_with("[memory:"));
                }
                other => panic!("expected Text, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn skipped_when_no_match() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5);
        let ctx = BriefContext::new(TokenBudget::default())
            .with_user_message("nothing-matches-this-token-xyz-pqr");
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn skipped_when_k_is_zero() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 0);
        let ctx = BriefContext::new(TokenBudget::default()).with_user_message("dry-run");
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn user_message_only_strategy_skips_without_message() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5)
            .with_strategy(MemoryQueryStrategy::UserMessageOnly);
        let ctx = BriefContext::new(TokenBudget::default()).with_goal("ship it");
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn goal_only_strategy_uses_goal() {
        let provider = seeded_provider().await;
        let src =
            MemorySource::new(provider, "test", 5).with_strategy(MemoryQueryStrategy::GoalOnly);
        let ctx = BriefContext::new(TokenBudget::default()).with_goal("dry-run");
        let cs = src.contribute(&ctx).await.expect("ok");
        assert!(!cs.is_empty());
    }

    #[tokio::test]
    async fn user_then_goal_falls_back_to_goal() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5);
        let ctx = BriefContext::new(TokenBudget::default()).with_goal("dry-run");
        let cs = src.contribute(&ctx).await.expect("ok");
        assert!(!cs.is_empty());
    }

    #[tokio::test]
    async fn custom_extractor_overrides_strategy() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5)
            .with_strategy(MemoryQueryStrategy::UserMessageOnly)
            .with_query_text_extractor(|_| Some("Q4".into()));
        let ctx = BriefContext::new(TokenBudget::default());
        let cs = src.contribute(&ctx).await.expect("ok");
        assert!(!cs.is_empty());
    }

    #[tokio::test]
    async fn priority_and_id_defaults() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5);
        assert_eq!(src.priority(), Priority::Normal);
        assert_eq!(src.id(), SourceId::new("memory"));
    }

    #[tokio::test]
    async fn with_id_overrides_default() {
        let provider = seeded_provider().await;
        let src = MemorySource::new(provider, "test", 5).with_id("long_term");
        assert_eq!(src.id(), SourceId::new("long_term"));
    }
}
