//! [`Governance`] trait + [`GovernanceVerdict`] + [`NoOpGovernance`] default.
//!
//! After a [`crate::types::Brief`] draft is
//! assembled and budget-pruned, the builder hands the draft to
//! [`Governance::review`]. The hook decides one of three things:
//!
//! - **Allow** — let the brief through unchanged.
//! - **Redacted** — the hook mutated `redactable` content; the returned
//!   [`crate::receipt::RedactionRecord`]s describe what changed and which
//!   rule did it, and the builder forwards them into the
//!   [`crate::receipt::BriefReceipt`].
//! - **Rejected** — the brief cannot go to the model; the builder returns
//!   [`crate::error::BriefError::Rejected`] to the caller.
//!
//! `cel-brief` does **not** ship a rules implementation. The default
//! [`NoOpGovernance`] always returns `Allow`. Concrete governance — e.g. a
//! rules-engine implementation wired to a downstream runtime's policy store —
//! lives in that runtime (such as the Cellar daemon), which keeps the brief
//! layer free of any runtime-specific dependencies.

use async_trait::async_trait;
use thiserror::Error;

use crate::receipt::RedactionRecord;
use crate::types::{Brief, BriefContext};

/// Verdict returned by [`Governance::review`].
///
/// Mutating variants (`Redacted`) implicitly trust that the hook has
/// already updated the draft brief in place; the receipt entries are the
/// audit trail, not the rewrite log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GovernanceVerdict {
    /// The brief is fine as-is.
    Allow,
    /// The hook mutated redactable content in the brief. The records
    /// describe which sources were touched and why. The builder copies
    /// these into [`crate::receipt::BriefReceipt::redactions`].
    Redacted(Vec<RedactionRecord>),
    /// The brief violates policy and must not reach the model. The string
    /// becomes the error message in [`crate::error::BriefError::Rejected`].
    Rejected(String),
}

impl GovernanceVerdict {
    /// True if this verdict carries any [`RedactionRecord`]s.
    pub fn is_redacted(&self) -> bool {
        matches!(self, GovernanceVerdict::Redacted(_))
    }

    /// True if this verdict represents a rejection.
    pub fn is_rejected(&self) -> bool {
        matches!(self, GovernanceVerdict::Rejected(_))
    }
}

/// Error returned by [`Governance::review`].
///
/// Use [`Self::Backend`] when the rules engine itself fails (database
/// down, rule compile error) — that's distinct from a clean `Rejected`
/// verdict. Bubble up via [`crate::error::BriefError`] at the builder
/// layer.
#[derive(Debug, Error)]
pub enum GovernanceError {
    /// Rules engine backend failed.
    #[error("governance backend error: {0}")]
    Backend(String),

    /// Catch-all.
    #[error("governance error: {0}")]
    Other(String),
}

/// The pluggable per-turn governance hook.
///
/// One [`Governance`] implementation per [`crate::builder::BriefBuilder`].
/// Implementations should:
/// - Be cheap. The hook runs on every turn.
/// - Only rewrite [`crate::source::Contribution`]s whose `redactable` flag
///   the source set to `true`. Non-redactable content (system prompt,
///   user message) should only ever trigger [`GovernanceVerdict::Allow`]
///   or [`GovernanceVerdict::Rejected`].
/// - Be deterministic given the same `(draft, ctx)` so receipts are
///   reproducible.
#[async_trait]
pub trait Governance: Send + Sync {
    /// Review (and optionally mutate) the draft brief.
    async fn review(
        &self,
        draft: &mut Brief,
        ctx: &BriefContext,
    ) -> Result<GovernanceVerdict, GovernanceError>;
}

/// The default [`Governance`] — always returns
/// [`GovernanceVerdict::Allow`] without inspecting the draft.
///
/// Use this when you've already decided you don't need governance, or
/// when you're prototyping. Production callers should plug in a real
/// implementation backed by their own policy/rules engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpGovernance;

impl NoOpGovernance {
    /// Construct a new [`NoOpGovernance`].
    pub fn new() -> Self {
        NoOpGovernance
    }
}

#[async_trait]
impl Governance for NoOpGovernance {
    async fn review(
        &self,
        _draft: &mut Brief,
        _ctx: &BriefContext,
    ) -> Result<GovernanceVerdict, GovernanceError> {
        Ok(GovernanceVerdict::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::BriefReceipt;
    use crate::types::{BriefMessage, Role, SourceId, TokenBudget};

    fn empty_draft() -> Brief {
        Brief {
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            receipt: BriefReceipt::empty(),
        }
    }

    #[tokio::test]
    async fn no_op_always_allows() {
        let mut draft = empty_draft();
        let ctx = BriefContext::new(TokenBudget::default());
        let v = NoOpGovernance.review(&mut draft, &ctx).await.expect("ok");
        assert_eq!(v, GovernanceVerdict::Allow);
    }

    /// Fixture: replaces any User text message containing "secret" with
    /// "[REDACTED]" and records one [`RedactionRecord`] per rewrite.
    struct SecretScrubber;

    #[async_trait]
    impl Governance for SecretScrubber {
        async fn review(
            &self,
            draft: &mut Brief,
            _ctx: &BriefContext,
        ) -> Result<GovernanceVerdict, GovernanceError> {
            let mut records = Vec::new();
            for msg in &mut draft.messages {
                if let BriefMessage::Text {
                    role: Role::User,
                    content,
                    source,
                } = msg
                {
                    if content.contains("secret") {
                        records.push(RedactionRecord {
                            source: source.clone(),
                            rule: "rule:no_user_secrets".into(),
                        });
                        *content = content.replace("secret", "[REDACTED]");
                    }
                }
            }
            Ok(if records.is_empty() {
                GovernanceVerdict::Allow
            } else {
                GovernanceVerdict::Redacted(records)
            })
        }
    }

    #[tokio::test]
    async fn redacted_verdict_carries_records() {
        let mut draft = empty_draft();
        draft.messages.push(BriefMessage::Text {
            role: Role::User,
            content: "my secret is hunter2".into(),
            source: SourceId::new("user_message"),
        });
        let ctx = BriefContext::new(TokenBudget::default());

        let v = SecretScrubber.review(&mut draft, &ctx).await.expect("ok");
        match v {
            GovernanceVerdict::Redacted(records) => {
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].source, SourceId::new("user_message"));
                assert_eq!(records[0].rule, "rule:no_user_secrets");
            }
            other => panic!("expected Redacted, got {other:?}"),
        }
        match &draft.messages[0] {
            BriefMessage::Text { content, .. } => {
                assert!(content.contains("[REDACTED]"));
                assert!(!content.contains("secret"));
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn allow_returned_when_nothing_to_redact() {
        let mut draft = empty_draft();
        draft.messages.push(BriefMessage::Text {
            role: Role::User,
            content: "hi".into(),
            source: SourceId::new("user_message"),
        });
        let ctx = BriefContext::new(TokenBudget::default());

        let v = SecretScrubber.review(&mut draft, &ctx).await.expect("ok");
        assert_eq!(v, GovernanceVerdict::Allow);
    }

    /// Fixture: rejects unconditionally with a fixed reason string. Used to
    /// exercise the `Rejected` path's helpers.
    struct AlwaysReject;

    #[async_trait]
    impl Governance for AlwaysReject {
        async fn review(
            &self,
            _draft: &mut Brief,
            _ctx: &BriefContext,
        ) -> Result<GovernanceVerdict, GovernanceError> {
            Ok(GovernanceVerdict::Rejected("locked down".into()))
        }
    }

    #[tokio::test]
    async fn rejected_verdict_carries_reason() {
        let mut draft = empty_draft();
        let ctx = BriefContext::new(TokenBudget::default());
        let v = AlwaysReject.review(&mut draft, &ctx).await.expect("ok");
        match &v {
            GovernanceVerdict::Rejected(reason) => assert_eq!(reason, "locked down"),
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(v.is_rejected());
        assert!(!v.is_redacted());
    }

    #[tokio::test]
    async fn backend_error_propagates() {
        struct Broken;
        #[async_trait]
        impl Governance for Broken {
            async fn review(
                &self,
                _draft: &mut Brief,
                _ctx: &BriefContext,
            ) -> Result<GovernanceVerdict, GovernanceError> {
                Err(GovernanceError::Backend("policy db unreachable".into()))
            }
        }
        let mut draft = empty_draft();
        let ctx = BriefContext::new(TokenBudget::default());
        let err = Broken.review(&mut draft, &ctx).await.expect_err("err");
        match err {
            GovernanceError::Backend(msg) => assert!(msg.contains("policy db")),
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[test]
    fn verdict_helpers() {
        assert!(!GovernanceVerdict::Allow.is_redacted());
        assert!(!GovernanceVerdict::Allow.is_rejected());
        assert!(GovernanceVerdict::Redacted(Vec::new()).is_redacted());
        assert!(GovernanceVerdict::Rejected("x".into()).is_rejected());
    }
}
