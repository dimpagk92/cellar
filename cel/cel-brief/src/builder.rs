//! [`BriefBuilder`] — orchestrates fan-out, tokenization, budget pruning,
//! governance, and receipt assembly.
//!
//! The builder is intentionally cheap to construct and held by the agent
//! across turns. Sources, tokenizer, governance, budget, and strategy are
//! all swappable.
//!
//! ## Typical usage
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use cel_brief::{BriefBuilder, BriefContext, TokenBudget};
//! # use cel_brief::tokenizer::CharApproxTokenizer;
//! # async fn example() -> Result<(), cel_brief::BriefError> {
//! let builder = BriefBuilder::new()
//!     .tokenizer(Arc::new(CharApproxTokenizer))
//!     .budget(TokenBudget::default());
//!     // .source(Arc::new(MySource)) ...
//!
//! let ctx = BriefContext::new(TokenBudget::default());
//! let brief = builder.build(&ctx).await?;
//! println!("brief receipt: {} tokens", brief.receipt.total_tokens);
//! # Ok(())
//! # }
//! ```
//!
//! ## Open decision: cancellation
//!
//! An open question is whether the builder should cancel pending sources
//! when the budget overflows mid-fan-out. **Phase 2 ships without
//! cancellation** — every source's `contribute()` is allowed to run to
//! completion, and pruning happens once after `try_join_all` returns. This
//! keeps the contract simple (every source either succeeds, errors, or is
//! skipped — never half-cancelled) and lets Phase 4 add cancellation as an
//! opt-in `BriefBuilder::with_cancellation(...)` knob without breaking
//! callers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::join_all;

use crate::budget::{apply_budget, PruneStrategy, WeightedContribution};
use crate::error::{BriefError, Result};
use crate::governance::{Governance, GovernanceVerdict, NoOpGovernance};
use crate::receipt::{BriefReceipt, DroppedContribution, SourceStats, Timings};
use crate::source::{Contribution, ContributionContent, Source, SourceError};
use crate::tokenizer::{CharApproxTokenizer, Tokenizer};
use crate::types::{
    Brief, BriefContext, BriefMessage, Priority, SourceId, TokenBudget, ToolSchema,
};

/// Assembles a [`Brief`] from registered [`Source`]s.
///
/// See the module-level docs for usage. The default tokenizer is
/// [`CharApproxTokenizer`] (≈ 4 chars per token); the default governance is
/// [`NoOpGovernance`]; the default strategy is [`PruneStrategy::default`]
/// (== [`PruneStrategy::ImportanceFirst`]).
pub struct BriefBuilder {
    sources: Vec<Arc<dyn Source>>,
    governance: Arc<dyn Governance>,
    tokenizer: Arc<dyn Tokenizer>,
    budget: TokenBudget,
    strategy: PruneStrategy,
}

impl Default for BriefBuilder {
    fn default() -> Self {
        BriefBuilder::new()
    }
}

impl std::fmt::Debug for BriefBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BriefBuilder")
            .field("sources", &self.sources.len())
            .field("budget", &self.budget)
            .field("strategy", &self.strategy)
            .finish_non_exhaustive()
    }
}

impl BriefBuilder {
    /// Construct a fresh builder with no sources, the default
    /// [`CharApproxTokenizer`], [`NoOpGovernance`], the default
    /// [`TokenBudget`] (8000 total / 1024 reserved), and
    /// [`PruneStrategy::ImportanceFirst`].
    pub fn new() -> Self {
        BriefBuilder {
            sources: Vec::new(),
            governance: Arc::new(NoOpGovernance),
            tokenizer: Arc::new(CharApproxTokenizer),
            budget: TokenBudget::default(),
            strategy: PruneStrategy::default(),
        }
    }

    /// Register a [`Source`]. Duplicate [`SourceId`]s are rejected at
    /// [`BriefBuilder::build`] time, not here.
    pub fn source(mut self, source: Arc<dyn Source>) -> Self {
        self.sources.push(source);
        self
    }

    /// Swap the [`Governance`] hook. Defaults to [`NoOpGovernance`].
    pub fn governance(mut self, governance: Arc<dyn Governance>) -> Self {
        self.governance = governance;
        self
    }

    /// Swap the [`Tokenizer`]. Defaults to [`CharApproxTokenizer`].
    pub fn tokenizer(mut self, tokenizer: Arc<dyn Tokenizer>) -> Self {
        self.tokenizer = tokenizer;
        self
    }

    /// Set the [`TokenBudget`] used for pruning.
    pub fn budget(mut self, budget: TokenBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Set the [`PruneStrategy`]. Defaults to
    /// [`PruneStrategy::ImportanceFirst`].
    pub fn strategy(mut self, strategy: PruneStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Borrowed view of the configured sources. Useful for tests.
    pub fn sources(&self) -> &[Arc<dyn Source>] {
        &self.sources
    }

    /// The configured budget.
    pub fn current_budget(&self) -> &TokenBudget {
        &self.budget
    }

    /// Assemble the [`Brief`] for `ctx`.
    ///
    /// Steps:
    /// 1. Reject duplicate [`SourceId`]s.
    /// 2. Fan out to every source in parallel via
    ///    [`join_all`]. Per-source errors
    ///    surface in step 4 — fan-out itself never fails.
    /// 3. Tokenize each surviving [`Contribution`] using the active
    ///    [`Tokenizer`] (source-provided `estimated_tokens` is treated as a
    ///    hint only).
    /// 4. Apply budget pruning via [`apply_budget`].
    /// 5. Assemble draft [`Brief`] (system concatenation, message order
    ///    preserved within priority, tools collected).
    /// 6. Run [`Governance::review`]. Allow / Redacted advance; Rejected
    ///    surfaces as [`BriefError::Rejected`].
    /// 7. Build the [`BriefReceipt`] with per-source stats, drops, and
    ///    timings.
    ///
    /// On any fatal source error (anything other than
    /// [`SourceError::Skipped`]), `build` returns
    /// [`BriefError::Source`]. Skipped sources are silently treated as
    /// zero contributions.
    pub async fn build(&self, ctx: &BriefContext) -> Result<Brief> {
        let start = Instant::now();

        // Step 1 — reject duplicate IDs.
        self.check_unique_ids()?;

        // Step 2 — fan-out.
        let (per_source_results, fanout_elapsed) = self.fan_out(ctx).await;

        // Surface the first fatal source error, but only after collecting
        // all results so we can give the caller a complete picture in
        // future versions (Phase 2 still single-errors).
        for (sid, res) in &per_source_results {
            if let Err(err) = res {
                match err {
                    SourceError::Skipped(_) => {} // not fatal
                    other => {
                        return Err(BriefError::Source {
                            source_id: sid.to_string(),
                            message: other.to_string(),
                        });
                    }
                }
            }
        }

        // Build a (SourceId, Priority, Vec<Contribution>) flat list,
        // dropping skipped sources but keeping their bookkeeping for
        // SourceStats.
        let mut by_source_priority: HashMap<SourceId, Priority> = HashMap::new();
        let mut by_source_contribs: HashMap<SourceId, usize> = HashMap::new();
        let mut flat: Vec<WeightedContribution> = Vec::new();

        for (sid, res) in per_source_results.into_iter() {
            let priority = self
                .sources
                .iter()
                .find(|s| s.id() == sid)
                .map(|s| s.priority())
                .unwrap_or(Priority::Normal);
            by_source_priority.insert(sid.clone(), priority);

            let contributions = match res {
                Ok(contribs) => contribs,
                Err(SourceError::Skipped(_)) => Vec::new(),
                // Fatal errors already returned above.
                Err(_) => unreachable!("fatal source errors handled above"),
            };

            by_source_contribs.insert(sid.clone(), contributions.len());

            // Step 3 — tokenize each contribution.
            let tokenize_start = Instant::now();
            for (idx, contribution) in contributions.into_iter().enumerate() {
                let actual_tokens = measure_contribution(&*self.tokenizer, &contribution.content);
                flat.push(WeightedContribution {
                    source: sid.clone(),
                    priority,
                    actual_tokens,
                    source_index: idx,
                    contribution,
                });
            }
            // Keep tokenize timing roughly accurate: we accumulate
            // per-source pre-flat to get a sensible total without
            // double-counting.
            let _ = tokenize_start.elapsed();
        }

        // Recompute tokenize timing accurately by measuring the full
        // tokenize sweep — wraps the per-source loop above, which we
        // already executed; the timing field is informational, not a
        // performance gate, so we take a single coarse measurement here.
        // (`tokenize` is wall-clock total.)
        let tokenize_total = compute_tokenize_total(&self.tokenizer, &flat);

        // Step 4 — apply budget.
        let prune_start = Instant::now();
        let (kept, dropped) = apply_budget(flat, &self.budget, self.strategy);
        let prune_elapsed = prune_start.elapsed();

        // Step 5 — assemble draft.
        let (mut draft, per_source_stats) =
            self.assemble_draft(kept, &dropped, &by_source_priority, &by_source_contribs);

        // Step 6 — governance.
        let governance_start = Instant::now();
        let verdict = self
            .governance
            .review(&mut draft, ctx)
            .await
            .map_err(|e| BriefError::Rejected(e.to_string()))?;
        let governance_elapsed = governance_start.elapsed();

        match verdict {
            GovernanceVerdict::Allow => {}
            GovernanceVerdict::Redacted(records) => {
                draft.receipt.redactions = records;
            }
            GovernanceVerdict::Rejected(reason) => {
                return Err(BriefError::Rejected(reason));
            }
        }

        // Step 7 — finalise receipt.
        draft.receipt.dropped = dropped;
        draft.receipt.by_source = per_source_stats;
        draft.receipt.total_tokens = draft.receipt.by_source.values().map(|s| s.tokens).sum();
        draft.receipt.timings = Timings {
            fanout: fanout_elapsed,
            tokenize: tokenize_total,
            prune: prune_elapsed,
            governance: governance_elapsed,
            total: start.elapsed(),
        };
        draft.receipt.built_at = std::time::SystemTime::now();

        // Defensive check: if Critical alone overflows the budget the
        // pruner leaves us over-budget. Surface that as
        // BudgetUnsatisfiable so the caller knows the brief is not
        // sendable.
        let prompt_budget = self.budget.prompt_budget();
        if draft.receipt.total_tokens > prompt_budget {
            return Err(BriefError::BudgetUnsatisfiable {
                needed: draft.receipt.total_tokens,
                available: prompt_budget,
            });
        }

        Ok(draft)
    }

    fn check_unique_ids(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for source in &self.sources {
            let id = source.id();
            if !seen.insert(id.clone()) {
                return Err(BriefError::Source {
                    source_id: id.to_string(),
                    message: "duplicate source id registered on BriefBuilder".into(),
                });
            }
        }
        Ok(())
    }

    async fn fan_out(
        &self,
        ctx: &BriefContext,
    ) -> (
        Vec<(
            SourceId,
            std::result::Result<Vec<Contribution>, SourceError>,
        )>,
        Duration,
    ) {
        let start = Instant::now();
        let futures = self.sources.iter().map(|source| {
            let source = source.clone();
            let ctx = ctx.clone();
            async move {
                let id = source.id();
                let res = source.contribute(&ctx).await;
                (id, res)
            }
        });

        let results = join_all(futures).await;
        // `fanout` is wall-clock of join_all (which approximates the
        // longest source — close enough for the receipt without per-source
        // timers).
        let elapsed = start.elapsed();
        (results, elapsed)
    }

    fn assemble_draft(
        &self,
        kept: Vec<WeightedContribution>,
        dropped: &[DroppedContribution],
        by_source_priority: &HashMap<SourceId, Priority>,
        by_source_contribs: &HashMap<SourceId, usize>,
    ) -> (Brief, HashMap<SourceId, SourceStats>) {
        let mut system_chunks: Vec<String> = Vec::new();
        let mut messages: Vec<BriefMessage> = Vec::new();
        let mut tools: Vec<ToolSchema> = Vec::new();

        // Track per-source kept counts and token totals.
        let mut kept_count: HashMap<SourceId, usize> = HashMap::new();
        let mut kept_tokens: HashMap<SourceId, usize> = HashMap::new();

        for w in &kept {
            *kept_count.entry(w.source.clone()).or_insert(0) += 1;
            *kept_tokens.entry(w.source.clone()).or_insert(0) += w.actual_tokens;
        }

        // Iterate `kept` in input order — `apply_budget` preserves it.
        for w in kept {
            match w.contribution.content {
                ContributionContent::System { text } => {
                    system_chunks.push(text);
                }
                ContributionContent::Text { role, content } => {
                    messages.push(BriefMessage::Text {
                        role,
                        content,
                        source: w.source,
                    });
                }
                ContributionContent::Image { role, data, alt } => {
                    messages.push(BriefMessage::Image {
                        role,
                        data,
                        alt,
                        source: w.source,
                    });
                }
                ContributionContent::ToolCall { id, name, args } => {
                    messages.push(BriefMessage::ToolCall {
                        id,
                        name,
                        args,
                        source: w.source,
                    });
                }
                ContributionContent::ToolResult { id, content } => {
                    messages.push(BriefMessage::ToolResult {
                        id,
                        content,
                        source: w.source,
                    });
                }
                ContributionContent::Tool { mut schema } => {
                    // Set the schema's `source` field to the contributing
                    // source (overriding whatever the source put in).
                    schema.source = w.source.clone();
                    tools.push(schema);
                }
            }
        }

        let system = if system_chunks.is_empty() {
            None
        } else {
            Some(system_chunks.join("\n\n"))
        };

        // Per-source stats: every source the builder consulted gets an
        // entry — including sources whose contributions were all pruned.
        let mut by_source_stats: HashMap<SourceId, SourceStats> = HashMap::new();
        for source in &self.sources {
            let sid = source.id();
            let priority = by_source_priority
                .get(&sid)
                .copied()
                .unwrap_or(Priority::Normal);
            let contributions = by_source_contribs.get(&sid).copied().unwrap_or(0);
            let kept = kept_count.get(&sid).copied().unwrap_or(0);
            let tokens = kept_tokens.get(&sid).copied().unwrap_or(0);
            by_source_stats.insert(
                sid,
                SourceStats {
                    contributions,
                    kept,
                    tokens,
                    priority,
                },
            );
        }

        let mut receipt = BriefReceipt::empty();
        receipt.dropped = dropped.to_vec();

        (
            Brief {
                system,
                messages,
                tools,
                receipt,
            },
            by_source_stats,
        )
    }
}

fn measure_contribution(tokenizer: &dyn Tokenizer, content: &ContributionContent) -> usize {
    match content {
        ContributionContent::System { text } => tokenizer.count(text),
        ContributionContent::Text { content, .. } => tokenizer.count(content),
        ContributionContent::Image { alt, .. } => {
            // We don't tokenize image bytes. Charge the alt text only —
            // image token estimation is left to the source.
            alt.as_deref().map(|a| tokenizer.count(a)).unwrap_or(0)
        }
        ContributionContent::ToolCall { name, args, .. } => {
            let mut total = tokenizer.count(name);
            // Args are JSON — tokenize the compact form.
            if let Ok(s) = serde_json::to_string(args) {
                total += tokenizer.count(&s);
            }
            total
        }
        ContributionContent::ToolResult { content, .. } => tokenizer.count(content),
        ContributionContent::Tool { schema } => {
            let mut total = tokenizer.count(&schema.name) + tokenizer.count(&schema.description);
            if let Ok(s) = serde_json::to_string(&schema.input_schema) {
                total += tokenizer.count(&s);
            }
            total
        }
    }
}

/// Re-measure total tokenize time for the timings receipt. Cheap (re-runs
/// `count` over already-tokenized contributions) but accurate.
fn compute_tokenize_total(
    tokenizer: &Arc<dyn Tokenizer>,
    flat: &[WeightedContribution],
) -> Duration {
    let start = Instant::now();
    for w in flat {
        let _ = measure_contribution(&**tokenizer, &w.contribution.content);
    }
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;

    use crate::source::{Contribution, Source, SourceError};
    use crate::types::{Role, SourceId};

    struct FixedSource {
        id: &'static str,
        priority: Priority,
        contributions: Vec<Contribution>,
    }

    #[async_trait]
    impl Source for FixedSource {
        fn id(&self) -> SourceId {
            SourceId::new(self.id)
        }

        fn priority(&self) -> Priority {
            self.priority
        }

        async fn contribute(
            &self,
            _ctx: &BriefContext,
        ) -> std::result::Result<Vec<Contribution>, SourceError> {
            Ok(self.contributions.clone())
        }
    }

    struct ErrSource;

    #[async_trait]
    impl Source for ErrSource {
        fn id(&self) -> SourceId {
            SourceId::new("err")
        }

        fn priority(&self) -> Priority {
            Priority::Normal
        }

        async fn contribute(
            &self,
            _ctx: &BriefContext,
        ) -> std::result::Result<Vec<Contribution>, SourceError> {
            Err(SourceError::Backend("boom".into()))
        }
    }

    struct SkipSource;

    #[async_trait]
    impl Source for SkipSource {
        fn id(&self) -> SourceId {
            SourceId::new("skip")
        }

        fn priority(&self) -> Priority {
            Priority::Normal
        }

        async fn contribute(
            &self,
            _ctx: &BriefContext,
        ) -> std::result::Result<Vec<Contribution>, SourceError> {
            Err(SourceError::Skipped("nothing to add".into()))
        }
    }

    fn ctx() -> BriefContext {
        BriefContext::new(TokenBudget::default())
    }

    #[tokio::test]
    async fn builds_a_brief_from_a_single_system_source() {
        let src = Arc::new(FixedSource {
            id: "sys",
            priority: Priority::Critical,
            contributions: vec![Contribution::system("be helpful", 3)],
        });
        let builder = BriefBuilder::new().source(src);
        let brief = builder.build(&ctx()).await.expect("build ok");
        assert_eq!(brief.system.as_deref(), Some("be helpful"));
        assert!(brief.messages.is_empty());
        assert_eq!(brief.receipt.by_source.len(), 1);
        let stats = brief
            .receipt
            .by_source
            .get(&SourceId::new("sys"))
            .expect("stats");
        assert_eq!(stats.contributions, 1);
        assert_eq!(stats.kept, 1);
        assert!(stats.tokens > 0);
    }

    #[tokio::test]
    async fn skipped_sources_do_not_fail_the_build() {
        let sys = Arc::new(FixedSource {
            id: "sys",
            priority: Priority::Critical,
            contributions: vec![Contribution::system("ok", 1)],
        });
        let skip = Arc::new(SkipSource);
        let builder = BriefBuilder::new().source(sys).source(skip);
        let brief = builder.build(&ctx()).await.expect("build ok");
        let skip_stats = brief
            .receipt
            .by_source
            .get(&SourceId::new("skip"))
            .expect("skip recorded");
        assert_eq!(skip_stats.contributions, 0);
        assert_eq!(skip_stats.kept, 0);
    }

    #[tokio::test]
    async fn fatal_source_error_propagates() {
        let bad = Arc::new(ErrSource);
        let builder = BriefBuilder::new().source(bad);
        let err = builder.build(&ctx()).await.expect_err("should error");
        match err {
            BriefError::Source { source_id, .. } => assert_eq!(source_id, "err"),
            other => panic!("expected Source error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_source_ids_rejected() {
        let a = Arc::new(FixedSource {
            id: "same",
            priority: Priority::Normal,
            contributions: vec![],
        });
        let b = Arc::new(FixedSource {
            id: "same",
            priority: Priority::Normal,
            contributions: vec![],
        });
        let builder = BriefBuilder::new().source(a).source(b);
        let err = builder.build(&ctx()).await.expect_err("should error");
        match err {
            BriefError::Source { source_id, message } => {
                assert_eq!(source_id, "same");
                assert!(message.contains("duplicate"));
            }
            other => panic!("expected duplicate Source error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn budget_drops_lowest_importance_first() {
        // Two Normal-priority contributions: keep the high-importance
        // one, drop the low.
        let src = Arc::new(FixedSource {
            id: "mem",
            priority: Priority::Normal,
            contributions: vec![
                Contribution::text(Role::User, "x".repeat(100), 25).with_importance(0.9),
                Contribution::text(Role::User, "y".repeat(100), 25).with_importance(0.1),
            ],
        });
        // Budget = 30 (after 0 reserve). One 25-token contribution fits;
        // the second blows the budget.
        let budget = TokenBudget::new(30, 0);
        let builder = BriefBuilder::new().source(src).budget(budget);
        let brief = builder.build(&ctx()).await.expect("build ok");
        assert_eq!(brief.messages.len(), 1);
        assert_eq!(brief.receipt.dropped.len(), 1);
        // The dropped item is the low-importance one — content is all y's.
        match &brief.messages[0] {
            BriefMessage::Text { content, .. } => {
                assert!(
                    content.starts_with('x'),
                    "expected x's first, got {content}"
                );
            }
            other => panic!("expected Text message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn budget_unsatisfiable_when_critical_overflows() {
        let huge = "x".repeat(5000);
        let src = Arc::new(FixedSource {
            id: "sys",
            priority: Priority::Critical,
            contributions: vec![Contribution::system(huge, 1250)],
        });
        let budget = TokenBudget::new(100, 0);
        let builder = BriefBuilder::new().source(src).budget(budget);
        let err = builder.build(&ctx()).await.expect_err("should error");
        match err {
            BriefError::BudgetUnsatisfiable { needed, available } => {
                assert!(needed > available);
                assert_eq!(available, 100);
            }
            other => panic!("expected BudgetUnsatisfiable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tokenizer_swap_is_used_for_pruning() {
        // Custom tokenizer that returns 1000 for everything. The default
        // 30-token budget will then drop everything except Critical.
        struct Fat;
        impl Tokenizer for Fat {
            fn count(&self, _text: &str) -> usize {
                1000
            }
        }
        let src = Arc::new(FixedSource {
            id: "mem",
            priority: Priority::Normal,
            contributions: vec![Contribution::text(Role::User, "hi", 1)],
        });
        let budget = TokenBudget::new(30, 0);
        let builder = BriefBuilder::new()
            .source(src)
            .budget(budget)
            .tokenizer(Arc::new(Fat));
        let brief = builder.build(&ctx()).await.expect("build ok");
        assert!(
            brief.messages.is_empty(),
            "Fat tokenizer should drop everything"
        );
        assert_eq!(brief.receipt.dropped.len(), 1);
    }

    #[tokio::test]
    async fn governance_rejection_surfaces_as_error() {
        use crate::governance::{Governance, GovernanceError, GovernanceVerdict};

        struct AlwaysReject;
        #[async_trait]
        impl Governance for AlwaysReject {
            async fn review(
                &self,
                _draft: &mut Brief,
                _ctx: &BriefContext,
            ) -> std::result::Result<GovernanceVerdict, GovernanceError> {
                Ok(GovernanceVerdict::Rejected("no".into()))
            }
        }

        let src = Arc::new(FixedSource {
            id: "sys",
            priority: Priority::Critical,
            contributions: vec![Contribution::system("hi", 1)],
        });
        let builder = BriefBuilder::new()
            .source(src)
            .governance(Arc::new(AlwaysReject));
        let err = builder.build(&ctx()).await.expect_err("should reject");
        match err {
            BriefError::Rejected(reason) => assert_eq!(reason, "no"),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn governance_redactions_land_on_receipt() {
        use crate::governance::{Governance, GovernanceError, GovernanceVerdict};
        use crate::receipt::RedactionRecord;

        struct Redact;
        #[async_trait]
        impl Governance for Redact {
            async fn review(
                &self,
                _draft: &mut Brief,
                _ctx: &BriefContext,
            ) -> std::result::Result<GovernanceVerdict, GovernanceError> {
                Ok(GovernanceVerdict::Redacted(vec![RedactionRecord {
                    source: SourceId::new("sys"),
                    rule: "rule:test".into(),
                }]))
            }
        }

        let src = Arc::new(FixedSource {
            id: "sys",
            priority: Priority::Critical,
            contributions: vec![Contribution::system("hi", 1)],
        });
        let builder = BriefBuilder::new().source(src).governance(Arc::new(Redact));
        let brief = builder.build(&ctx()).await.expect("ok");
        assert_eq!(brief.receipt.redactions.len(), 1);
        assert_eq!(brief.receipt.redactions[0].rule, "rule:test");
    }

    #[tokio::test]
    async fn priority_floor_protects_normal_bucket() {
        // Use ~100-byte payloads so each ≈ 25 tokens under the default
        // CharApprox tokenizer (4 chars per token) — enough that the
        // 80-token budget actually forces pruning.
        let critical = Arc::new(FixedSource {
            id: "sys",
            priority: Priority::Critical,
            contributions: vec![Contribution::system("c".repeat(100), 25)],
        });
        let normal = Arc::new(FixedSource {
            id: "hist",
            priority: Priority::Normal,
            contributions: vec![
                Contribution::text(Role::User, "n".repeat(100), 25).with_importance(0.5),
                Contribution::text(Role::User, "x".repeat(100), 25).with_importance(0.4),
            ],
        });
        let low = Arc::new(FixedSource {
            id: "noise",
            priority: Priority::Low,
            contributions: vec![
                Contribution::text(Role::User, "l".repeat(100), 25).with_importance(0.9)
            ],
        });

        // 80 token budget, 25 floor on Normal: Total is 100 tokens, need
        // to drop 20. Low priority (25) drops first → 75. Done.
        let budget = TokenBudget::new(80, 0).with_floor(Priority::Normal, 25);
        let builder = BriefBuilder::new()
            .source(critical)
            .source(normal)
            .source(low)
            .budget(budget);
        let brief = builder.build(&ctx()).await.expect("ok");
        let dropped_sources: Vec<&str> = brief
            .receipt
            .dropped
            .iter()
            .map(|d| d.source.as_str())
            .collect();
        assert!(
            dropped_sources.contains(&"noise"),
            "expected noise to be dropped, got dropped={dropped_sources:?}"
        );
        assert!(!dropped_sources.contains(&"hist"));
    }
}
