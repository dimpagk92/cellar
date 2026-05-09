//! WK2 (un-deferred): vector-embedding infrastructure for cortex memory
//! and (later) knowledge.
//!
//! This module ships the **seam** — the trait, the byte-serialization
//! helpers, and a deterministic similarity function — without bundling
//! any concrete embedder. Production callers wire one of:
//!
//! - A cel-llm-provider-based embedder (one API call per memory write)
//! - A local ONNX/candle model (~50 MB binary, no per-call cost)
//! - A test stub (deterministic; no network)
//!
//! Choice of which embedder to ship by default stays a deployment
//! decision gated on the recall eval (see `COGNITION_LAYER_PLAN.md`).
//! Until that decision lands, callers pass `None` and the storage +
//! selector code paths simply skip the embedding-aware steps.
//!
//! ## Why pre-compute, not embed-in-builder?
//!
//! Embedding is async (provider call or model invocation); the
//! cortex `build_planning_view` is sync. Rather than make every
//! `PlanningView` build async, callers compute the goal embedding
//! once at the top of a run and pass the bytes through to each turn's
//! `PlanningViewInputs.goal_embedding`. The goal doesn't change within
//! a run, so one embed call covers the whole loop.

use async_trait::async_trait;

use crate::LlmError;

/// A single text embedding — fixed-dimension f32 vector with
/// platform-independent serialization helpers.
///
/// Stored as little-endian f32 bytes in `cortex_memories.embedding`.
/// All embeddings within a single store **MUST** come from the same
/// embedder (same model_id, same dimensions); cosine comparison across
/// dimension mismatches is meaningless and the selector will skip
/// candidates whose stored bytes don't decode at the expected
/// dimension.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingVector(Vec<f32>);

impl EmbeddingVector {
    pub fn new(values: Vec<f32>) -> Self {
        Self(values)
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn dimensions(&self) -> usize {
        self.0.len()
    }

    /// Serialize to a flat byte buffer suitable for storage in the
    /// SQLite BLOB column. Little-endian f32 throughout.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() * 4);
        for v in &self.0 {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Parse from the raw bytes the storage layer hands back. Returns
    /// `None` when the byte length isn't a multiple of 4 (invalid /
    /// corrupted) — selector treats `None` as "skip this candidate's
    /// cosine boost", same as a missing embedding.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
            return None;
        }
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|chunk| {
                let arr: [u8; 4] = chunk.try_into().expect("chunks_exact guarantees length 4");
                f32::from_le_bytes(arr)
            })
            .collect();
        Some(Self(values))
    }
}

/// Cosine similarity in `[-1, 1]`. Returns `0.0` for length mismatch
/// or zero-magnitude inputs (defensive — never NaN reaches the scorer).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// The contract `cel-cortex` and `cel-goal-runner` use to populate
/// `cortex_memories.embedding` at write time and (when the runner
/// pre-computes a goal embedding) the cortex selector uses to score
/// candidate memories at read time.
///
/// Keep the surface minimal — a trait with too many methods is hard
/// to substitute. If you need batching or caching, wrap one of these
/// at a higher layer rather than expanding the trait.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single text. Returns an `EmbeddingVector` whose
    /// `dimensions()` equals `self.dimensions()`. Implementations
    /// MUST guarantee this contract — the storage and selector layers
    /// rely on it for cross-record consistency.
    async fn embed(&self, text: &str) -> Result<EmbeddingVector, LlmError>;

    /// The fixed dimension count this embedder produces. Stable for
    /// the lifetime of a given instance. Used to early-reject stored
    /// embeddings whose decoded length doesn't match (e.g. after a
    /// model swap that wasn't paired with a cortex_memories wipe).
    fn dimensions(&self) -> usize;

    /// Optional model identifier (e.g. "openai:text-embedding-3-small"
    /// or "local-onnx:all-MiniLM-L6-v2"). When stable, the runner can
    /// stamp it on writes so future tooling can detect mixed-model
    /// pollution. Returns `None` for test stubs and embedders that
    /// don't have a meaningful identity.
    fn model_id(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_vector_roundtrips_through_bytes() {
        let v = EmbeddingVector::new(vec![0.1, -0.5, 1.5, 0.0, -1.0]);
        let bytes = v.to_bytes();
        assert_eq!(bytes.len(), 5 * 4);
        let back = EmbeddingVector::from_bytes(&bytes).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn empty_or_misaligned_bytes_decode_to_none() {
        assert!(EmbeddingVector::from_bytes(&[]).is_none());
        assert!(EmbeddingVector::from_bytes(&[1, 2, 3]).is_none()); // not multiple of 4
        assert!(EmbeddingVector::from_bytes(&[1, 2, 3, 4, 5]).is_none()); // not multiple of 4
    }

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_opposite_vectors_is_negative_one() {
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_handles_length_mismatch_gracefully() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_handles_zero_magnitude_without_nan() {
        let v = cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]);
        assert_eq!(v, 0.0);
        assert!(!v.is_nan());
    }

    /// Deterministic stub embedder used by downstream tests
    /// (cel-cortex, cel-goal-runner). Hash-based, no model bundled.
    /// Same `text` always produces the same vector.
    struct StubEmbedder {
        dim: usize,
    }

    #[async_trait]
    impl Embedder for StubEmbedder {
        async fn embed(&self, text: &str) -> Result<EmbeddingVector, LlmError> {
            // Trivial deterministic embedding: bucket bytes into `dim`
            // slots, normalize to unit length. Good enough for tests
            // that need "same text → same vector, different text →
            // different vector."
            let mut out = vec![0f32; self.dim];
            for (i, b) in text.bytes().enumerate() {
                out[i % self.dim] += (b as f32) / 255.0;
            }
            // Normalize so cosine of identical text == 1.
            let mag: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
            if mag > 0.0 {
                for v in &mut out {
                    *v /= mag;
                }
            }
            Ok(EmbeddingVector::new(out))
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
        fn model_id(&self) -> Option<&str> {
            Some("stub:test")
        }
    }

    #[tokio::test]
    async fn stub_embedder_is_deterministic_and_normalized() {
        let e = StubEmbedder { dim: 16 };
        let a = e.embed("hello world").await.unwrap();
        let b = e.embed("hello world").await.unwrap();
        assert_eq!(a, b);
        // Self-cosine of normalized non-zero vector should be ~1.
        assert!((cosine_similarity(a.as_slice(), b.as_slice()) - 1.0).abs() < 1e-5);
    }

    #[tokio::test]
    async fn different_text_produces_different_embeddings() {
        let e = StubEmbedder { dim: 16 };
        let a = e.embed("submit invoice in Concur").await.unwrap();
        let b = e.embed("water the plants in the kitchen").await.unwrap();
        assert_ne!(a, b);
        // Different text → cosine should be substantially less than 1.
        let cos = cosine_similarity(a.as_slice(), b.as_slice());
        assert!(
            cos < 0.99,
            "expected substantially-different vectors; got cos={cos}"
        );
    }
}
