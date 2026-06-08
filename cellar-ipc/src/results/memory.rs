//! `memory.*` response shapes.
//!
//! The chunk shape on the wire is a JSON value rather than a typed
//! `MemoryChunk` re-export to keep this crate from taking a hard dep on
//! `cel-memory`. The daemon serializes `cel_memory::MemoryChunk` directly,
//! so consumers can deserialize back into [`cel_memory::MemoryChunk`] if
//! they have that crate available, or work with the raw fields if not.

use serde::{Deserialize, Serialize};

/// Result for `memory.remember` — the freshly persisted chunk.
///
/// The chunk includes the provider-assigned `id`, `created_at`,
/// `embedding_model`, `embedding_dim`, the resolved `caller_id` (so the
/// caller can verify what the daemon stamped), and the `shareable` flag
/// as accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryRememberResult {
    /// The persisted chunk as a JSON value. Matches the wire format from
    /// `cellar-memory-manager.md` §12.4.
    pub chunk: serde_json::Value,
}

/// Result for `memory.recall` — top-k chunks in score order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryRecallResult {
    /// Ordered list of matching chunks. Each entry follows the wire
    /// format from `cellar-memory-manager.md` §12.4.
    pub chunks: Vec<serde_json::Value>,
    /// Number of chunks returned (equal to `chunks.len()` — included
    /// for convenience and to make zero-results trivially detectable).
    pub count: usize,
}

/// Result for `memory.forget` — count of chunks actually removed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryForgetResult {
    /// Number of chunks deleted. An empty predicate is a no-op and
    /// returns `0` (matches the trait-side
    /// [`cel_memory::MemoryProvider::delete_matching`] short-circuit).
    pub deleted: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forget_result_round_trip() {
        let r = MemoryForgetResult { deleted: 3 };
        let s = serde_json::to_string(&r).unwrap();
        let back: MemoryForgetResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back.deleted, 3);
    }

    #[test]
    fn recall_result_round_trip() {
        let r = MemoryRecallResult {
            chunks: vec![json!({"id": "x", "content": "hi"})],
            count: 1,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: MemoryRecallResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back.count, 1);
    }
}
