//! Cellar memory subsystem — trait surface and supporting types.
//!
//! This crate is the locked contract between the rest of the Cellar daemon
//! (embedded agent runtime, NL rule compiler, `cel_act` gateway, rule matcher
//! post-fire hook, Activity tab queries) and the memory implementation
//! underneath. The trait surface here is the source of truth referenced by
//! [`cellar-memory-manager.md`] §12; every v1 caller compiles against it.
//!
//! v1 ships [`BasicMemoryProvider`] as the backing implementation — real bodies
//! for [`MemoryProvider::retrieve`], [`MemoryProvider::write`], session
//! lifecycle, simple deletes, export, and stats; `Err(NotImplemented)` for
//! summarization, rollups, and re-embed; no-ops for `update_importance` and
//! `supersede`. The full Memory & Context Manager subsystem (separate crate,
//! separate plan) drops in behind the same trait without caller churn.
//!
//! [`cellar-memory-manager.md`]: file:///Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod basic;
pub mod chunk;
pub mod error;
pub mod importance;
pub mod ops;
pub mod provider;
pub mod query;
pub mod session;
pub mod summarizer;
pub mod write_hook;

// Convenient re-exports — the symbols every caller will name.
pub use basic::BasicMemoryProvider;
pub use chunk::{ChunkKind, ChunkSource, MemoryChunk, MemoryTier, NewMemoryChunk};
pub use error::{MemoryError, Result};
pub use importance::score as score_importance;
pub use ops::{
    AccessEntry, AgingReport, EvictionEntry, EvictionReason, ExportBundle, ExportFilter,
    MemoryStats, PurgeReport, ReEmbedReport,
};
pub use provider::MemoryProvider;
pub use query::{CallerScope, MemoryPredicate, MemoryQuery, RetrievalProfile};
pub use session::{MemorySession, NewMemorySession, SessionFilter, SessionOutcome};
pub use summarizer::{
    MockSummarizer, MockSummaryCall, Summarizer, SummarizerError, SummarizerResult, SummaryContext,
};
pub use write_hook::{ClosureHook, MemoryWriteHook, WriteDecision};
