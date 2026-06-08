//! cel-memory — the durable cross-turn memory contract for AI agents.
//!
//! This crate answers one question: **what should persist across turns?** It
//! owns the memory *contract* — the [`MemoryProvider`] trait plus the value
//! types every backend and caller share (chunks, sessions, queries, retrieval
//! profiles, caller scopes, write hooks, summaries, rollups, aging, export).
//! Storage backends implement the trait; callers depend only on it.
//!
//! The crate is deliberately narrow. It does **not** observe live device/world
//! state — that is `cel-cortex`'s job ("what is true now?") — and it does
//! **not** assemble per-turn LLM prompts — that is `cel-brief`'s job ("what
//! should the model see this turn?"). cel-memory owns persistence only.
//!
//! Cellar is the motivating consumer: its embedded agent runtime, NL rule
//! compiler, `cel_act` gateway, rule-matcher post-fire hook, and Activity /
//! Memory tabs all compile against this trait. But nothing here depends on
//! Cellar — the crate is reusable memory infrastructure for any agent runtime.
//!
//! [`BasicMemoryProvider`] is the in-crate reference implementation — real
//! bodies for [`MemoryProvider::retrieve`], [`MemoryProvider::write`], session
//! lifecycle, simple deletes, export, and stats; `Err(NotImplemented)` for
//! summarization, rollups, and re-embed; no-ops for `update_importance` and
//! `supersede`. A full storage backend (e.g. the `cel-memory-sqlite` crate)
//! drops in behind the same trait without caller churn.

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod basic;
pub mod chunk;
pub mod error;
pub mod importance;
pub mod offdevice_hook;
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
pub use offdevice_hook::{
    ClosureOffdeviceHook, OffdeviceCallDescriptor, OffdeviceCallHook, OffdeviceDecision,
};
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
