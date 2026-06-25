//! [`PerceptionSnapshot`] trait + [`PerceptionSource`] — current-screen
//! context for computer-use agents. **Gated by the `perception` cargo
//! feature.**
//!
//! cel-brief defines the trait only — it does not perceive anything itself and
//! does not depend on any perception engine. Downstream runtimes adapt their
//! own live perception system into a [`PerceptionSnapshot`] (or a full
//! [`Source`]); cel-brief only consumes the snapshot. The contract is
//! intentionally minimal: three async methods returning pre-rendered strings,
//! one per projection level. Backends decide what those strings actually
//! contain (AX tree, focus subtree, vision caption) — the source's only job is
//! to ferry the bytes into the brief at [`Priority::High`].
//!
//! The three projections mirror common perception levels: full tree, focus
//! subtree, and one-line summary. Backends should map their own knobs onto
//! these levels.

use async_trait::async_trait;

use crate::source::{Contribution, ContributionContent, Source, SourceError};
use crate::types::{BriefContext, Priority, Role, SourceId};

/// Pre-rendered perception snapshot — three projection levels, async.
///
/// Implementations should:
/// - Be cheap-to-poll (return cached / warmed state — don't crawl on each
///   call).
/// - Honour the projection level: `as_focus_only` should be *meaningfully*
///   smaller than `as_ax_tree`, otherwise the budget gain vanishes.
/// - Return `Ok("")` when no perception data is available; the source maps
///   that to a `Skipped` contribution.
#[async_trait]
pub trait PerceptionSnapshot: Send + Sync {
    /// Full accessibility tree (or equivalent dense per-element dump).
    /// Typically multi-KB. Use with care under tight budgets.
    async fn as_ax_tree(&self) -> Result<String, PerceptionError>;

    /// Only the focused element + its immediate ancestry / siblings.
    /// Typically <1KB. The right default for most agent turns.
    async fn as_focus_only(&self) -> Result<String, PerceptionError>;

    /// One-line natural-language summary (active app, window title, focused
    /// field). Smallest projection. Useful for ambient awareness without
    /// budget pressure.
    async fn as_screen_summary(&self) -> Result<String, PerceptionError>;
}

/// Error returned by [`PerceptionSnapshot`] methods.
///
/// The variants mirror [`crate::source::SourceError`] for the same reasons:
/// the perception layer is a backend, it can fail, and the brief layer
/// should be able to skip cleanly without erroring out.
#[derive(Debug, thiserror::Error)]
pub enum PerceptionError {
    /// Backend (vision, screen scraper, browser adapter, etc.) returned an error.
    #[error("perception backend error: {0}")]
    Backend(String),

    /// Perception backend chose not to return data this turn (e.g. screen
    /// is locked, no active app). The source maps this to `Skipped`.
    #[error("perception unavailable: {0}")]
    Unavailable(String),
}

/// Which projection level [`PerceptionSource`] should request from its
/// [`PerceptionSnapshot`] backend.
///
/// These projection levels live in `cel-brief` on purpose: the abstraction
/// stays self-contained and downstream backends map their own knobs onto these
/// buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PerceptionMode {
    /// Call [`PerceptionSnapshot::as_ax_tree`].
    AxTree,
    /// Call [`PerceptionSnapshot::as_focus_only`] (the default).
    #[default]
    FocusOnly,
    /// Call [`PerceptionSnapshot::as_screen_summary`].
    ScreenSummary,
}

/// A [`Source`] that injects the current-screen perception snapshot as a
/// system-role text contribution.
///
/// [`Priority::High`] — perception is steering input for computer-use
/// agents; under budget pressure it should outlast memory and history but
/// can yield to the system prompt, the user's message, and tools. The
/// contribution is redactable so governance can scrub screen content
/// against rules (e.g. "never include `bank.example.com` DOM in any
/// model prompt").
pub struct PerceptionSource<P: PerceptionSnapshot> {
    id: SourceId,
    backend: std::sync::Arc<P>,
    mode: PerceptionMode,
}

impl<P: PerceptionSnapshot> std::fmt::Debug for PerceptionSource<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerceptionSource")
            .field("id", &self.id)
            .field("mode", &self.mode)
            .finish()
    }
}

impl<P: PerceptionSnapshot> PerceptionSource<P> {
    /// Construct with the default ID `"perception"` and
    /// [`PerceptionMode::FocusOnly`].
    pub fn new(backend: std::sync::Arc<P>) -> Self {
        PerceptionSource {
            id: SourceId::new("perception"),
            backend,
            mode: PerceptionMode::default(),
        }
    }

    /// Override the default [`SourceId`].
    pub fn with_id(mut self, id: impl Into<SourceId>) -> Self {
        self.id = id.into();
        self
    }

    /// Override the projection level.
    pub fn with_mode(mut self, mode: PerceptionMode) -> Self {
        self.mode = mode;
        self
    }
}

#[async_trait]
impl<P: PerceptionSnapshot + 'static> Source for PerceptionSource<P> {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::High
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        let raw = match self.mode {
            PerceptionMode::AxTree => self.backend.as_ax_tree().await,
            PerceptionMode::FocusOnly => self.backend.as_focus_only().await,
            PerceptionMode::ScreenSummary => self.backend.as_screen_summary().await,
        };

        let body = match raw {
            Ok(s) => s,
            Err(PerceptionError::Unavailable(reason)) => {
                return Err(SourceError::Skipped(reason));
            }
            Err(PerceptionError::Backend(msg)) => {
                return Err(SourceError::Backend(msg));
            }
        };

        if body.trim().is_empty() {
            return Err(SourceError::Skipped("empty perception snapshot".into()));
        }

        let label = match self.mode {
            PerceptionMode::AxTree => "[perception:ax_tree]",
            PerceptionMode::FocusOnly => "[perception:focus_only]",
            PerceptionMode::ScreenSummary => "[perception:screen_summary]",
        };
        let content = format!("{label}\n{body}");
        let est = content.len().div_ceil(4);
        Ok(vec![Contribution {
            content: ContributionContent::Text {
                role: Role::System,
                content,
            },
            estimated_tokens: est,
            importance: 0.8,
            redactable: true,
            tags: vec!["perception".into()],
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenBudget;
    use std::sync::Arc;

    /// Test fixture: returns a different string per mode so we can assert
    /// which method ran. Each call is independent.
    struct FixedSnapshot {
        ax_tree: String,
        focus_only: String,
        summary: String,
    }

    #[async_trait]
    impl PerceptionSnapshot for FixedSnapshot {
        async fn as_ax_tree(&self) -> Result<String, PerceptionError> {
            Ok(self.ax_tree.clone())
        }
        async fn as_focus_only(&self) -> Result<String, PerceptionError> {
            Ok(self.focus_only.clone())
        }
        async fn as_screen_summary(&self) -> Result<String, PerceptionError> {
            Ok(self.summary.clone())
        }
    }

    fn fixture() -> Arc<FixedSnapshot> {
        Arc::new(FixedSnapshot {
            ax_tree: "<window>...</window>".into(),
            focus_only: "<button id=submit>".into(),
            summary: "Safari: example.com — search field focused".into(),
        })
    }

    #[tokio::test]
    async fn default_mode_is_focus_only() {
        let src = PerceptionSource::new(fixture());
        assert_eq!(src.priority(), Priority::High);
        let ctx = BriefContext::new(TokenBudget::default());
        let cs = src.contribute(&ctx).await.expect("ok");
        assert_eq!(cs.len(), 1);
        match &cs[0].content {
            ContributionContent::Text { role, content } => {
                assert_eq!(*role, Role::System);
                assert!(content.contains("[perception:focus_only]"));
                assert!(content.contains("submit"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
        assert!(cs[0].redactable);
        assert_eq!(cs[0].tags, vec!["perception".to_owned()]);
    }

    #[tokio::test]
    async fn ax_tree_mode_calls_ax_tree() {
        let src = PerceptionSource::new(fixture()).with_mode(PerceptionMode::AxTree);
        let ctx = BriefContext::new(TokenBudget::default());
        let cs = src.contribute(&ctx).await.expect("ok");
        match &cs[0].content {
            ContributionContent::Text { content, .. } => {
                assert!(content.contains("[perception:ax_tree]"));
                assert!(content.contains("<window>"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn screen_summary_mode_calls_summary() {
        let src = PerceptionSource::new(fixture()).with_mode(PerceptionMode::ScreenSummary);
        let ctx = BriefContext::new(TokenBudget::default());
        let cs = src.contribute(&ctx).await.expect("ok");
        match &cs[0].content {
            ContributionContent::Text { content, .. } => {
                assert!(content.contains("[perception:screen_summary]"));
                assert!(content.contains("Safari"));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_snapshot_is_skipped() {
        struct Empty;
        #[async_trait]
        impl PerceptionSnapshot for Empty {
            async fn as_ax_tree(&self) -> Result<String, PerceptionError> {
                Ok(String::new())
            }
            async fn as_focus_only(&self) -> Result<String, PerceptionError> {
                Ok("   ".into())
            }
            async fn as_screen_summary(&self) -> Result<String, PerceptionError> {
                Ok(String::new())
            }
        }
        let src = PerceptionSource::new(Arc::new(Empty));
        let ctx = BriefContext::new(TokenBudget::default());
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unavailable_maps_to_skipped() {
        struct Unavailable;
        #[async_trait]
        impl PerceptionSnapshot for Unavailable {
            async fn as_ax_tree(&self) -> Result<String, PerceptionError> {
                Err(PerceptionError::Unavailable("locked".into()))
            }
            async fn as_focus_only(&self) -> Result<String, PerceptionError> {
                Err(PerceptionError::Unavailable("locked".into()))
            }
            async fn as_screen_summary(&self) -> Result<String, PerceptionError> {
                Err(PerceptionError::Unavailable("locked".into()))
            }
        }
        let src = PerceptionSource::new(Arc::new(Unavailable));
        let ctx = BriefContext::new(TokenBudget::default());
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn backend_error_propagates() {
        struct Broken;
        #[async_trait]
        impl PerceptionSnapshot for Broken {
            async fn as_ax_tree(&self) -> Result<String, PerceptionError> {
                Err(PerceptionError::Backend("io".into()))
            }
            async fn as_focus_only(&self) -> Result<String, PerceptionError> {
                Err(PerceptionError::Backend("io".into()))
            }
            async fn as_screen_summary(&self) -> Result<String, PerceptionError> {
                Err(PerceptionError::Backend("io".into()))
            }
        }
        let src = PerceptionSource::new(Arc::new(Broken));
        let ctx = BriefContext::new(TokenBudget::default());
        match src.contribute(&ctx).await {
            Err(SourceError::Backend(_)) => {}
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn with_id_overrides_default() {
        let src = PerceptionSource::new(fixture()).with_id("perception");
        assert_eq!(src.id(), SourceId::new("perception"));
    }
}
