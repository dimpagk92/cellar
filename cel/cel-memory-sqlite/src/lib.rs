//! SQLite-backed [`MemoryProvider`] for Cellar.
//!
//! This crate is the foundation of the Memory & Context Manager subsystem
//! per `/Users/dimitriospagkratis/.claude/plans/cellar-memory-manager.md`.
//! Phase 0 (current) delivers:
//!
//! - Schema migrations for every table in §6.2 (`memory_chunks`,
//!   `memory_vec`, `memory_fts`, `memory_sessions`, `memory_summary_members`,
//!   `memory_access_log`, `memory_eviction_log`).
//! - `sqlite-vec` extension loaded into the connection at open time so the
//!   `memory_vec` virtual table is available.
//! - [`Embedder`] trait + [`MockEmbedder`] (always available) and
//!   [`FastEmbedEmbedder`] (gated behind the `fastembed` feature) for the
//!   real `bge-small-en-v1.5` model.
//! - [`SqliteMemoryProvider`] that opens a DB, runs migrations, and
//!   implements [`MemoryProvider`] with real bodies for the methods the
//!   v1 daemon needs at boot time (`stats`, `write`, `get`, `purge_all`)
//!   plus `Err(NotImplemented)` for the rest, ready for Phase 1 to fill in.
//!
//! [`MemoryProvider`]: cel_memory::MemoryProvider

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub(crate) mod cache;
pub mod embedder;
pub mod error;
pub mod migrations;
pub mod provider;
pub mod summarizer;
pub mod vec_extension;

#[cfg(feature = "fastembed")]
pub mod fastembed_impl;

pub use embedder::{Embedder, MockEmbedder};
pub use error::SqliteMemoryError;
pub use provider::SqliteMemoryProvider;
pub use summarizer::{
    build_default as build_default_summarizer, AnthropicSummarizer, OllamaSummarizer,
    DEFAULT_ANTHROPIC_MODEL, DEFAULT_OLLAMA_MODEL,
};

#[cfg(feature = "fastembed")]
pub use fastembed_impl::FastEmbedEmbedder;
