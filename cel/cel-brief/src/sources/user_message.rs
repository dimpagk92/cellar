//! [`UserMessageSource`] — pulls [`BriefContext::user_message`] into the brief.
//!
//! Critical-priority, never redactable. If `ctx.user_message` is `None` the
//! source returns [`SourceError::Skipped`] — the builder treats that as zero
//! contributions, not as a failure, so it's safe to register
//! `UserMessageSource` unconditionally.

use async_trait::async_trait;

use crate::source::{Contribution, Source, SourceError};
use crate::types::{BriefContext, Priority, Role, SourceId};

/// A [`Source`] that mirrors [`BriefContext::user_message`] into the brief.
///
/// Skipped (not failed) when no user message is present. Critical priority so
/// it survives any budget pruning short of `BudgetUnsatisfiable`. Emits a
/// [`Role::User`] text contribution; not redactable — the user's literal
/// words should reach the model verbatim.
#[derive(Debug, Clone)]
pub struct UserMessageSource {
    id: SourceId,
}

impl UserMessageSource {
    /// Construct with the default ID `"user_message"`.
    pub fn new() -> Self {
        UserMessageSource {
            id: SourceId::new("user_message"),
        }
    }

    /// Override the default [`SourceId`].
    pub fn with_id(mut self, id: impl Into<SourceId>) -> Self {
        self.id = id.into();
        self
    }
}

impl Default for UserMessageSource {
    fn default() -> Self {
        UserMessageSource::new()
    }
}

#[async_trait]
impl Source for UserMessageSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::Critical
    }

    async fn contribute(&self, ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        let Some(msg) = ctx.user_message.as_deref() else {
            return Err(SourceError::Skipped(
                "BriefContext.user_message is None".into(),
            ));
        };
        let est = msg.len().div_ceil(4);
        Ok(vec![Contribution::text(Role::User, msg.to_owned(), est)
            .with_importance(1.0)
            .with_redactable(false)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ContributionContent;
    use crate::types::TokenBudget;

    #[tokio::test]
    async fn round_trips_user_message() {
        let src = UserMessageSource::new();
        let ctx = BriefContext::new(TokenBudget::default()).with_user_message("hi there");
        let contributions = src.contribute(&ctx).await.expect("contribute ok");
        assert_eq!(contributions.len(), 1);
        let c = &contributions[0];
        assert_eq!(c.importance, 1.0);
        assert!(!c.redactable);
        match &c.content {
            ContributionContent::Text { role, content } => {
                assert_eq!(*role, Role::User);
                assert_eq!(content, "hi there");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn skipped_when_no_user_message() {
        let src = UserMessageSource::default();
        let ctx = BriefContext::new(TokenBudget::default());
        let err = src.contribute(&ctx).await.expect_err("should skip");
        match err {
            SourceError::Skipped(_) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn priority_is_critical() {
        assert_eq!(UserMessageSource::new().priority(), Priority::Critical);
    }

    #[tokio::test]
    async fn with_id_overrides_default() {
        let src = UserMessageSource::new().with_id("user_say");
        assert_eq!(src.id(), SourceId::new("user_say"));
    }
}
