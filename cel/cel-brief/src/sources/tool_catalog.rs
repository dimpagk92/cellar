//! [`ToolCatalogSource`] — static [`ToolSchema`] list at [`Priority::High`].
//!
//! Owns a `Vec<ToolSchema>` and emits each as a [`Contribution::tool`] every
//! turn. High priority because tool schemas usually steer model behaviour more
//! than any single memory or history item; the builder is free to drop tools
//! under budget pressure, but they outrank `Normal` / `Low` content.

use async_trait::async_trait;

use crate::source::{Contribution, ContributionContent, Source, SourceError};
use crate::types::{BriefContext, Priority, SourceId, ToolSchema};

/// A [`Source`] that exposes a fixed catalog of [`ToolSchema`]s to the model.
///
/// Each contributed schema inherits this source's [`SourceId`] via the
/// builder's admission step (the input schemas keep whatever `source` they
/// were constructed with — see [`Self::new`] for details).
#[derive(Debug, Clone)]
pub struct ToolCatalogSource {
    id: SourceId,
    tools: Vec<ToolSchema>,
}

impl ToolCatalogSource {
    /// Construct with the default ID `"tool_catalog"`. The input schemas'
    /// `source` field is rewritten to this source's ID so attribution in the
    /// final brief is correct.
    pub fn new(tools: impl IntoIterator<Item = ToolSchema>) -> Self {
        let id = SourceId::new("tool_catalog");
        let tools = tools
            .into_iter()
            .map(|mut t| {
                t.source = id.clone();
                t
            })
            .collect();
        ToolCatalogSource { id, tools }
    }

    /// Override the default [`SourceId`]. The source rewrites every contained
    /// schema's `source` field to match.
    pub fn with_id(mut self, id: impl Into<SourceId>) -> Self {
        self.id = id.into();
        for tool in &mut self.tools {
            tool.source = self.id.clone();
        }
        self
    }

    /// Read-only access to the contained schemas.
    pub fn tools(&self) -> &[ToolSchema] {
        &self.tools
    }

    /// Default per-tool token estimate (`description.len() / 4`).
    fn estimate_tokens(tool: &ToolSchema) -> usize {
        // Cheap heuristic: description + serialised schema length / 4.
        let schema_len = serde_json::to_string(&tool.input_schema)
            .map(|s| s.len())
            .unwrap_or(0);
        (tool.description.len() + schema_len + tool.name.len()).div_ceil(4)
    }
}

#[async_trait]
impl Source for ToolCatalogSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn priority(&self) -> Priority {
        Priority::High
    }

    async fn contribute(&self, _ctx: &BriefContext) -> Result<Vec<Contribution>, SourceError> {
        if self.tools.is_empty() {
            return Err(SourceError::Skipped("empty tool catalog".into()));
        }
        Ok(self
            .tools
            .iter()
            .map(|tool| {
                let est = Self::estimate_tokens(tool);
                Contribution {
                    content: ContributionContent::Tool {
                        schema: tool.clone(),
                    },
                    estimated_tokens: est,
                    importance: 0.9,
                    redactable: false,
                    tags: vec!["tool".into()],
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

    fn schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: format!("Test tool {name}"),
            input_schema: json!({"type":"object","properties":{}}),
            source: SourceId::new("__unset__"),
        }
    }

    #[tokio::test]
    async fn emits_one_contribution_per_tool() {
        let src = ToolCatalogSource::new(vec![schema("alpha"), schema("beta")]);
        let ctx = BriefContext::new(TokenBudget::default());
        let contributions = src.contribute(&ctx).await.expect("ok");
        assert_eq!(contributions.len(), 2);

        for c in &contributions {
            assert_eq!(c.importance, 0.9);
            assert!(!c.redactable);
            match &c.content {
                ContributionContent::Tool { schema } => {
                    assert_eq!(schema.source, SourceId::new("tool_catalog"));
                }
                other => panic!("expected Tool, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn empty_catalog_is_skipped() {
        let src = ToolCatalogSource::new(Vec::<ToolSchema>::new());
        let ctx = BriefContext::new(TokenBudget::default());
        match src.contribute(&ctx).await {
            Err(SourceError::Skipped(_)) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn with_id_rewrites_tool_source() {
        let src = ToolCatalogSource::new(vec![schema("alpha")]).with_id("example_tools");
        assert_eq!(src.id(), SourceId::new("example_tools"));
        let ctx = BriefContext::new(TokenBudget::default());
        let contributions = src.contribute(&ctx).await.expect("ok");
        match &contributions[0].content {
            ContributionContent::Tool { schema } => {
                assert_eq!(schema.source, SourceId::new("example_tools"));
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn priority_is_high() {
        let src = ToolCatalogSource::new(vec![schema("alpha")]);
        assert_eq!(src.priority(), Priority::High);
    }
}
