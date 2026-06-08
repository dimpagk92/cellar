//! cel-brief — Per-turn LLM briefing layer.
//!
//! cel-brief answers one question: **what should the model see this turn?** It
//! sits between an agent's state and the LLM — every turn, sources contribute
//! [`source::Contribution`]s that the builder budgets, prunes, governs, and
//! assembles into a provider-agnostic [`types::Brief`] plus a
//! [`receipt::BriefReceipt`] of what was included, dropped, and redacted.
//!
//! It is deliberately scoped. cel-brief does **not** discover live device/world
//! truth itself — a `PerceptionSource` *consumes* a snapshot that some backend
//! (e.g. `cel-cortex`) produced — and it does **not** store memory; a
//! `MemorySource` reads from a `cel_memory::MemoryProvider`. cel-brief owns
//! per-turn briefing assembly only, and never depends on `cel-cortex`.
//!
//! **Status (Phases 1 + 2 + 3 + 4):** all core types, traits, sources,
//! governance, and builder are shipped.
//!
//! **Phase 1** — core types ([`types::Brief`], [`types::BriefMessage`],
//! [`types::Role`], [`types::BriefContext`], [`types::TokenBudget`],
//! [`types::Priority`], [`types::ToolSchema`], [`types::ImageData`],
//! [`types::SourceId`]) and the [`source::Source`] trait + supporting
//! types ([`source::Contribution`], [`source::ContributionContent`],
//! [`source::SourceError`]).
//!
//! **Phase 2** — [`builder::BriefBuilder`], [`tokenizer::Tokenizer`]
//! (default [`tokenizer::CharApproxTokenizer`]; opt-in
//! `TiktokenCl100k` behind the `tiktoken` feature),
//! [`budget::PruneStrategy`] + budget enforcement, full
//! [`receipt::BriefReceipt`].
//!
//! **Phase 3** — built-in [`sources::SystemPromptSource`],
//! [`sources::UserMessageSource`], [`sources::ToolCatalogSource`],
//! [`sources::HistorySource`] + [`sources::HistoryStore`] under default
//! features; `MemorySource` behind the `memory` feature;
//! `PerceptionSource` + `PerceptionSnapshot` behind
//! the `perception` feature.
//!
//! **Phase 4** — [`governance::Governance`] trait with
//! [`governance::NoOpGovernance`] default, [`governance::GovernanceVerdict`]
//! enum, and the [`receipt::RedactionRecord`] surface that
//! [`governance::GovernanceVerdict::Redacted`] returns.
//!
//! ## Quick start
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use cel_brief::{BriefBuilder, BriefContext, TokenBudget};
//! # async fn example() -> Result<(), cel_brief::BriefError> {
//! let builder = BriefBuilder::new()
//!     .budget(TokenBudget::new(8000, 1024));
//!     // .source(Arc::new(MySource)) ...
//!
//! let ctx = BriefContext::new(TokenBudget::default())
//!     .with_user_message("hi");
//! let brief = builder.build(&ctx).await?;
//! println!("brief: {} messages, {} tokens",
//!     brief.messages.len(), brief.receipt.total_tokens);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod budget;
pub mod builder;
pub mod error;
pub mod governance;
pub mod receipt;
pub mod source;
pub mod sources;
pub mod tokenizer;
pub mod types;

pub use budget::{PruneStrategy, WeightedContribution};
pub use builder::BriefBuilder;
pub use error::{BriefError, Result};
pub use governance::{Governance, GovernanceError, GovernanceVerdict, NoOpGovernance};
pub use receipt::{
    BriefReceipt, DropReason, DroppedContribution, RedactionRecord, SourceStats, Timings,
};
pub use source::{Contribution, ContributionContent, Source, SourceError};
pub use sources::{
    HistoryEntry, HistorySource, HistoryStore, SystemPromptSource, ToolCatalogSource,
    UserMessageSource,
};
#[cfg(feature = "memory")]
pub use sources::{MemoryQueryStrategy, MemorySource};
#[cfg(feature = "perception")]
pub use sources::{PerceptionError, PerceptionMode, PerceptionSnapshot, PerceptionSource};
pub use tokenizer::{CharApproxTokenizer, Tokenizer};
pub use types::{
    Brief, BriefContext, BriefMessage, ImageData, Priority, Role, SourceId, TokenBudget, ToolSchema,
};

#[cfg(feature = "tiktoken")]
pub use tokenizer::TiktokenCl100k;
