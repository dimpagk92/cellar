//! Memory subsystem error type.
//!
//! All trait methods return `Result<T, MemoryError>`. `NotImplemented` is the
//! honest signal v1 uses for methods the [`BasicMemoryProvider`] doesn't back
//! yet (summarization, rollups, re-embed). Callers behind feature flags treat
//! it as a graceful no-feature signal.
//!
//! [`BasicMemoryProvider`]: crate::BasicMemoryProvider

use thiserror::Error;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, MemoryError>;

/// Errors produced by the memory subsystem.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// The requested feature isn't implemented by the current provider.
    ///
    /// The in-crate [`BasicMemoryProvider`] returns this for summarization,
    /// rollups, and re-embed methods. A full storage backend (e.g. the
    /// `cel-memory-sqlite` crate) implements all of these.
    ///
    /// [`BasicMemoryProvider`]: crate::BasicMemoryProvider
    #[error("not implemented in this provider: {0}")]
    NotImplemented(&'static str),

    /// The requested chunk, session, or other entity could not be found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A caller passed an invalid argument (out-of-range importance, empty
    /// query, malformed predicate, etc.).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A storage-layer error (SQLite, filesystem, embedding model load).
    #[error("storage error: {0}")]
    Storage(String),

    /// A network or external-provider error (cloud embedder, cloud summarizer).
    #[error("provider error: {0}")]
    Provider(String),

    /// An unexpected internal invariant was violated. Bug.
    #[error("internal error: {0}")]
    Internal(String),
}
