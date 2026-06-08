//! [`SystemPromptSource`] — static system-prompt text at [`Priority::Critical`].
//!
//! The simplest possible [`Source`]: holds an owned [`String`] and emits it as
//! a [`Contribution::system`] every turn. Critical-priority, never redactable.
//!
//! For dynamic system prompts (per-user personalisation, A/B-tested copy)
//! wrap [`Source`] yourself and template the text from [`BriefContext`] inside
//! `contribute`.

use async_trait::async_trait;

use crate::source::{Contribution, Source, SourceError};
use crate::types::{BriefContext, Priority, SourceId};

/// A [`Source`] that emits a fixed system-prompt string every turn.
///
/// Critical-priority, marked non-redactable so governance can't quietly
/// rewrite the system prompt. The tokenizer hint defaults to
/// `text.len() / 4`; override with [`Self::with_estimated_tokens`] when you
/// have a tighter number from your tokenizer.
#[derive(Debug, Clone)]
pub struct SystemPromptSource {
    id: SourceId,
    text: String,
    estimated_tokens: usize,
}

impl SystemPromptSource {
    /// Construct a source with the default ID `"system_prompt"`.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let estimated_tokens = text.len().div_ceil(4);
        SystemPromptSource {
            id: SourceId::new("system_prompt"),
            text,
            estimated_tokens,
        }
    }

    /// Override the default [`SourceId`] (useful when wiring multiple system
    /// prompts — e.g. a base prompt plus a user-specific personality layer —
    /// and you want them attributed separately in the receipt).
    pub fn with_id(mut self, id: impl Into<SourceId>) -> Self {
        self.id = id.into();
        self
    }

    /// Override the source-side token estimate. The builder always
    /// re-tokenizes ground-truth, so this is a hint only.
    pub fn with_estimated_tokens(mut self, tokens: usize) -> Self {
        self.estimated_tokens = tokens;
        self
    }

    /// The raw system-prompt text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[async_trait]
impl Source for SystemPromptSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::Critical
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        Ok(vec![Contribution::system(
            self.text.clone(),
            self.estimated_tokens,
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ContributionContent;
    use crate::types::TokenBudget;

    #[tokio::test]
    async fn emits_one_critical_system_contribution() {
        let src = SystemPromptSource::new("You are a helpful assistant.");
        assert_eq!(src.priority(), Priority::Critical);
        assert_eq!(src.id(), SourceId::new("system_prompt"));

        let ctx = BriefContext::new(TokenBudget::default());
        let contributions = src.contribute(&ctx).await.expect("contribute ok");
        assert_eq!(contributions.len(), 1);
        let c = &contributions[0];
        assert!(!c.redactable);
        assert_eq!(c.importance, 1.0);
        match &c.content {
            ContributionContent::System { text } => {
                assert_eq!(text, "You are a helpful assistant.");
            }
            other => panic!("expected System, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn with_id_overrides_default() {
        let src = SystemPromptSource::new("hi").with_id("personality");
        assert_eq!(src.id(), SourceId::new("personality"));
    }

    #[tokio::test]
    async fn with_estimated_tokens_overrides_default() {
        let src = SystemPromptSource::new("hi").with_estimated_tokens(99);
        let ctx = BriefContext::new(TokenBudget::default());
        let c = src.contribute(&ctx).await.expect("contribute ok");
        assert_eq!(c[0].estimated_tokens, 99);
    }

    #[test]
    fn default_estimate_is_four_chars_per_token() {
        // 12 chars → ceil(12/4) = 3 tokens.
        let src = SystemPromptSource::new("hello, world");
        assert_eq!(src.estimated_tokens, 3);
    }
}
