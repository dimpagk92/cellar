//! [`Source`] trait + [`Contribution`] + [`ContributionContent`] + [`SourceError`].
//!
//! A `Source` is the unit of pluggability: every per-turn
//! input — memory, perception, history, tools, the user message — implements
//! the same trait, returns the same [`Contribution`] shape, and is composed
//! by [`crate::builder::BriefBuilder`] (Phase 2). The crate has zero opinions
//! about what sources you wire in.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{BriefContext, ImageData, Priority, Role, SourceId, ToolSchema};

/// What a [`Source`] can contribute to a [`crate::types::Brief`].
///
/// One [`Contribution`] becomes (eventually) one
/// [`crate::types::BriefMessage`] or one [`ToolSchema`] in the final brief,
/// modulo budget pruning and governance redaction. The variants intentionally
/// mirror [`crate::types::BriefMessage`] plus a [`ContributionContent::System`]
/// variant for system-prompt text and a [`ContributionContent::Tool`] variant
/// for tool catalog entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContributionContent {
    /// System-prompt text. Multiple contributions are concatenated in source
    /// order by the builder.
    System {
        /// System-prompt text.
        text: String,
    },
    /// Plain text content under a role.
    Text {
        /// Role this message is attributed to.
        role: Role,
        /// Message body.
        content: String,
    },
    /// Image content under a role.
    Image {
        /// Role this image is attributed to.
        role: Role,
        /// The image payload.
        data: ImageData,
        /// Optional alt / caption text.
        #[serde(default)]
        alt: Option<String>,
    },
    /// A prior tool invocation, replayed into the conversation.
    ToolCall {
        /// Provider-issued tool-call ID.
        id: String,
        /// Tool name (matches a [`ToolSchema::name`]).
        name: String,
        /// JSON arguments the model passed.
        args: serde_json::Value,
    },
    /// The result returned for a prior tool call.
    ToolResult {
        /// Tool-call ID this result responds to.
        id: String,
        /// Serialised result content.
        content: String,
    },
    /// A tool schema to be made available to the model this turn.
    Tool {
        /// The tool schema. Its `source` field is set by the builder when
        /// the contribution is admitted.
        schema: ToolSchema,
    },
}

/// One item produced by [`Source::contribute`].
///
/// The builder treats `estimated_tokens` as a hint (always re-tokenized
/// ground-truth on the builder side), but uses `importance` and the source's
/// [`Priority`] to drive budget pruning. `redactable` lets governance
/// (Phase 4) selectively rewrite content rather than dropping it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contribution {
    /// What this contribution carries.
    pub content: ContributionContent,
    /// Source-estimated token cost. Treated as a hint — the builder
    /// re-tokenizes for ground truth before pruning.
    pub estimated_tokens: usize,
    /// Importance in `[0.0, 1.0]`. Higher importance survives pruning.
    /// Out-of-range values are clamped on admission.
    pub importance: f32,
    /// If `true`, governance may rewrite the content rather than dropping
    /// it. If `false`, governance can only allow or reject.
    pub redactable: bool,
    /// Free-form tags (e.g. `"summary"`, `"recent"`) used by governance and
    /// debugging.
    ///
    /// Originally sketched as `Vec<&'static str>`, widened to
    /// `Vec<String>` so [`Contribution`] can satisfy [`serde::Deserialize`]
    /// — required so all brief types round-trip through serde. Static
    /// callers pay one allocation per tag; receipt-side consumers gain
    /// round-trippability.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Contribution {
    /// Build a [`ContributionContent::System`] contribution with sensible
    /// defaults (Critical-importance text, not redactable, no tags).
    pub fn system(text: impl Into<String>, estimated_tokens: usize) -> Self {
        Contribution {
            content: ContributionContent::System { text: text.into() },
            estimated_tokens,
            importance: 1.0,
            redactable: false,
            tags: Vec::new(),
        }
    }

    /// Build a [`ContributionContent::Text`] contribution.
    pub fn text(role: Role, content: impl Into<String>, estimated_tokens: usize) -> Self {
        Contribution {
            content: ContributionContent::Text {
                role,
                content: content.into(),
            },
            estimated_tokens,
            importance: 0.5,
            redactable: true,
            tags: Vec::new(),
        }
    }

    /// Build a [`ContributionContent::Tool`] contribution.
    pub fn tool(schema: ToolSchema, estimated_tokens: usize) -> Self {
        Contribution {
            content: ContributionContent::Tool { schema },
            estimated_tokens,
            importance: 0.9,
            redactable: false,
            tags: Vec::new(),
        }
    }

    /// Set the importance, clamped to `[0.0, 1.0]`.
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Mark the contribution as redactable (or not) by governance.
    pub fn with_redactable(mut self, redactable: bool) -> Self {
        self.redactable = redactable;
        self
    }

    /// Append a free-form tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
}

/// Error returned by [`Source::contribute`].
///
/// Bubble up via [`crate::error::BriefError::Source`] at the builder layer.
/// Sources should map their own backend errors into one of these variants;
/// `Other` is the catch-all when no better fit exists.
#[derive(Error, Debug)]
pub enum SourceError {
    /// Backend (database, network, adapter) was reachable but returned an
    /// error.
    #[error("backend error: {0}")]
    Backend(String),

    /// Source was misconfigured (e.g. missing required input on the
    /// [`crate::types::BriefContext`]).
    #[error("misconfigured: {0}")]
    Misconfigured(String),

    /// Source decided not to contribute this turn for a non-error reason.
    /// The builder treats this as zero contributions, not as a failure.
    #[error("source skipped: {0}")]
    Skipped(String),

    /// Catch-all for sources whose backend errors don't fit the above.
    #[error("source error: {0}")]
    Other(String),
}

/// A pluggable contributor to the per-turn [`crate::types::Brief`].
///
/// `id` is the stable label that ends up on every emitted
/// [`crate::types::BriefMessage`] / [`crate::types::ToolSchema`] and in the
/// [`crate::receipt::BriefReceipt`]. `priority` drives the budget floor
/// pre-pruning. `contribute` is the work — read from `ctx`, return zero or
/// more [`Contribution`]s.
///
/// Implementations should:
/// - Keep `contribute` deterministic given `ctx` where possible.
/// - Honour `ctx.budget` as a hint but not a hard cap — pruning is the
///   builder's job.
/// - Return cheap, pre-rendered text; don't perform heavy formatting per
///   turn.
#[async_trait]
pub trait Source: Send + Sync {
    /// Stable identifier for this source. Receipts and message attribution
    /// key off this value.
    fn id(&self) -> SourceId;

    /// Priority bucket. Drives budget floors and prune ordering.
    fn priority(&self) -> Priority;

    /// Produce zero or more [`Contribution`]s for this turn.
    async fn contribute(&self, ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenBudget;

    /// Hard-coded source — returns a fixed system prompt every turn.
    /// Exercises the trait surface end-to-end.
    struct FixedSystemSource;

    #[async_trait]
    impl Source for FixedSystemSource {
        fn id(&self) -> SourceId {
            SourceId::new("fixed_system")
        }

        fn priority(&self) -> Priority {
            Priority::Critical
        }

        async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
            Ok(vec![Contribution::system(
                "You are a helpful assistant.",
                7,
            )])
        }
    }

    /// Source that mirrors the user message back as a Text contribution.
    /// Returns `Skipped` when no user message is present.
    struct EchoUserSource;

    #[async_trait]
    impl Source for EchoUserSource {
        fn id(&self) -> SourceId {
            SourceId::new("echo_user")
        }

        fn priority(&self) -> Priority {
            Priority::Critical
        }

        async fn contribute(&self, ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
            let Some(msg) = ctx.user_message.as_deref() else {
                return Err(SourceError::Skipped("no user message".into()));
            };
            Ok(vec![Contribution::text(
                Role::User,
                msg.to_owned(),
                // 4 chars per token rule of thumb, rounded up.
                msg.len().div_ceil(4),
            )
            .with_importance(1.0)
            .with_redactable(false)])
        }
    }

    #[tokio::test]
    async fn fixed_source_returns_contribution() {
        let source = FixedSystemSource;
        assert_eq!(source.id(), SourceId::new("fixed_system"));
        assert_eq!(source.priority(), Priority::Critical);

        let ctx = BriefContext::new(TokenBudget::default());
        let contributions = source.contribute(&ctx).await.expect("contribute ok");
        assert_eq!(contributions.len(), 1);
        match &contributions[0].content {
            ContributionContent::System { text } => {
                assert_eq!(text, "You are a helpful assistant.");
            }
            other => panic!("expected System, got {other:?}"),
        }
        assert_eq!(contributions[0].estimated_tokens, 7);
        assert_eq!(contributions[0].importance, 1.0);
    }

    #[tokio::test]
    async fn echo_source_returns_skipped_when_no_message() {
        let source = EchoUserSource;
        let ctx = BriefContext::new(TokenBudget::default());
        let err = source.contribute(&ctx).await.expect_err("should skip");
        match err {
            SourceError::Skipped(_) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn echo_source_round_trips_user_message() {
        let source = EchoUserSource;
        let ctx = BriefContext::new(TokenBudget::default()).with_user_message("Hello, world!");
        let contributions = source.contribute(&ctx).await.expect("contribute ok");
        assert_eq!(contributions.len(), 1);
        match &contributions[0].content {
            ContributionContent::Text { role, content } => {
                assert_eq!(*role, Role::User);
                assert_eq!(content, "Hello, world!");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn contribution_with_importance_clamps() {
        let c = Contribution::text(Role::User, "hi", 1).with_importance(2.0);
        assert_eq!(c.importance, 1.0);
        let c = Contribution::text(Role::User, "hi", 1).with_importance(-0.5);
        assert_eq!(c.importance, 0.0);
    }

    #[test]
    fn contribution_tags_chain() {
        let c = Contribution::system("hi", 1)
            .with_tag("system")
            .with_tag(String::from("default"));
        assert_eq!(c.tags, vec!["system".to_owned(), "default".to_owned()]);
    }

    #[test]
    fn contribution_round_trips_through_serde_json() {
        let c = Contribution::text(Role::Assistant, "ok", 1)
            .with_importance(0.75)
            .with_redactable(false);
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Contribution = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}
