//! cel-brief — Per-turn LLM briefing layer.
//!
//! See [`cellar-cel-brief.md`] for the full implementation plan. This crate is
//! the per-turn briefing layer that sits between an agent's state and the LLM:
//! every turn, sources contribute [`source::Contribution`]s to a budgeted,
//! governed [`types::Brief`].
//!
//! **Status (Phase 1):** core types ([`types::Brief`], [`types::BriefMessage`],
//! [`types::Role`], [`types::BriefContext`], [`types::TokenBudget`],
//! [`types::Priority`], [`types::ToolSchema`], [`types::ImageData`],
//! [`types::SourceId`]) and the [`source::Source`] trait + supporting types
//! ([`source::Contribution`], [`source::ContributionContent`],
//! [`source::SourceError`]) are landed. [`receipt::BriefReceipt`] is a Phase 2
//! placeholder — usable via [`receipt::BriefReceipt::empty`] so callers can
//! hand-construct a [`types::Brief`] before the builder ships.
//! [`builder::BriefBuilder`], [`tokenizer::Tokenizer`], [`governance::Governance`],
//! and [`budget`] enforcement land in later phases.
//!
//! [`cellar-cel-brief.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-cel-brief.md

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod budget;
pub mod builder;
pub mod error;
pub mod governance;
pub mod receipt;
pub mod source;
pub mod tokenizer;
pub mod types;

pub use error::{BriefError, Result};
pub use receipt::{
    BriefReceipt, DropReason, DroppedContribution, RedactionRecord, SourceStats, Timings,
};
pub use source::{Contribution, ContributionContent, Source, SourceError};
pub use types::{
    Brief, BriefContext, BriefMessage, ImageData, Priority, Role, SourceId, TokenBudget, ToolSchema,
};
