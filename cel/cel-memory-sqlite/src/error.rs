//! Storage-side error type. Converted to [`MemoryError`] on the way out
//! of the [`SqliteMemoryProvider`](crate::SqliteMemoryProvider).
//!
//! [`MemoryError`]: cel_memory::MemoryError

use cel_memory::MemoryError;
use thiserror::Error;

/// Errors that originate inside the SQLite memory backend.
#[derive(Debug, Error)]
pub enum SqliteMemoryError {
    /// rusqlite returned an error.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Migration file failed to apply.
    #[error("migration {name} failed: {source}")]
    Migration {
        /// Migration file basename, e.g. `"001_initial.sql"`.
        name: String,
        /// Underlying SQLite error.
        #[source]
        source: rusqlite::Error,
    },

    /// sqlite-vec couldn't be loaded into the connection.
    #[error("sqlite-vec load failed: {0}")]
    VecLoad(String),

    /// `tokio::task::spawn_blocking` panicked.
    #[error("blocking task panicked: {0}")]
    BlockingJoin(String),

    /// Embedder produced a vector with the wrong dimension.
    #[error("embedding dim mismatch: provider expects {expected}, embedder produced {actual}")]
    DimMismatch {
        /// Dim the schema expects (matches `memory_vec`'s declared dim).
        expected: usize,
        /// Dim the embedder actually produced.
        actual: usize,
    },

    /// JSON serialization or deserialization failed inside the storage layer.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<SqliteMemoryError> for MemoryError {
    fn from(e: SqliteMemoryError) -> Self {
        match e {
            SqliteMemoryError::DimMismatch { expected, actual } => MemoryError::InvalidArgument(
                format!("embedding dim mismatch: expected {expected}, got {actual}"),
            ),
            SqliteMemoryError::Json(err) => MemoryError::Storage(format!("json: {err}")),
            other => MemoryError::Storage(other.to_string()),
        }
    }
}
