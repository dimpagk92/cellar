//! Error type for `cel-brief`.

use thiserror::Error;

/// Top-level error returned by `BriefBuilder` operations.
#[derive(Error, Debug)]
pub enum BriefError {
    /// One of the configured sources failed to contribute.
    #[error("source `{source_id}` failed: {message}")]
    Source {
        /// Identifier of the source that failed.
        source_id: String,
        /// Underlying error message.
        message: String,
    },

    /// Governance rejected the draft brief.
    #[error("brief rejected by governance: {0}")]
    Rejected(String),

    /// Token budget could not be satisfied even after pruning to priority floors.
    #[error("token budget unsatisfiable: needed at least {needed}, had {available}")]
    BudgetUnsatisfiable {
        /// Tokens still required after pruning.
        needed: usize,
        /// Tokens available under the configured budget.
        available: usize,
    },

    /// A source operation timed out.
    #[error("source `{source_id}` timed out after {timeout_ms}ms")]
    Timeout {
        /// Identifier of the source that timed out.
        source_id: String,
        /// Configured timeout in milliseconds.
        timeout_ms: u64,
    },
}

/// Convenience alias for `Result<T, BriefError>`.
pub type Result<T> = std::result::Result<T, BriefError>;
