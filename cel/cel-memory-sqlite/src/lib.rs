//! SQLite-backed [`MemoryProvider`] — one concrete persistence backend for the
//! `cel-memory` crate.
//!
//! This crate implements the [`MemoryProvider`] contract on top of a single
//! local SQLite file (plus `sqlite-vec` for vector search and FTS5 for lexical
//! search). It owns persistence behavior only — schema, migrations, embeddings,
//! hybrid retrieval, caching — and depends on `cel-memory` for the trait and
//! value types. It does not depend on `cel-cortex` or `cel-brief`.
//!
//! Cellar is the motivating consumer, but the crate is a drop-in backend for
//! any agent runtime that speaks `cel_memory::MemoryProvider`.
//!
//! What it delivers:
//!
//! - Schema migrations for every memory table (`memory_chunks`, `memory_vec`,
//!   `memory_fts`, `memory_sessions`, `memory_summary_members`,
//!   `memory_access_log`, `memory_eviction_log`).
//! - `sqlite-vec` extension loaded into the connection at open time so the
//!   `memory_vec` virtual table is available.
//! - [`Embedder`] trait + [`MockEmbedder`] (always available) and
//!   `FastEmbedEmbedder` (gated behind the `fastembed` feature) for the
//!   real `bge-small-en-v1.5` model.
//! - [`SqliteMemoryProvider`] implementing the full [`MemoryProvider`] surface:
//!   writes, hybrid (vector + FTS + recency) retrieval with a TTL+LRU cache,
//!   sessions, summarization and rollups (via an injected
//!   [`cel_memory::Summarizer`]), aging sweeps, export, and stats. The only
//!   method still returning `Err(NotImplemented)` is `re_embed_all`.
//!
//! [`MemoryProvider`]: cel_memory::MemoryProvider

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub(crate) mod cache;
pub mod embedder;
pub mod error;
pub mod migrations;
pub mod provider;
pub mod vec_extension;

#[cfg(feature = "fastembed")]
pub mod fastembed_impl;

pub use embedder::{Embedder, MockEmbedder};
pub use error::SqliteMemoryError;
pub use provider::SqliteMemoryProvider;

#[cfg(feature = "fastembed")]
pub use fastembed_impl::FastEmbedEmbedder;
