//! Embedder trait + reference implementations.
//!
//! The [`Embedder`] trait abstracts over local (fastembed/ONNX) and cloud
//! (OpenAI/Voyage) embedding backends. The [`SqliteMemoryProvider`] takes
//! a `Box<dyn Embedder>` at construction and uses it for write-time
//! embedding and retrieval-time query embedding.
//!
//! v1 ships two implementations:
//!
//! - [`MockEmbedder`] — deterministic small-dim vectors for tests. No
//!   external dependencies; always available.
//! - `FastEmbedEmbedder` — real `bge-small-en-v1.5` model (384 dim) via
//!   the [`fastembed`] crate. Gated behind the `fastembed` feature
//!   because the model file downloads on first instantiation (~130 MB)
//!   and onnxruntime is a heavy dep.
//!
//! [`SqliteMemoryProvider`]: crate::SqliteMemoryProvider
//! [`fastembed`]: https://crates.io/crates/fastembed

use async_trait::async_trait;

use crate::error::SqliteMemoryError;

/// Embedding result alias.
pub type EmbedResult<T> = Result<T, SqliteMemoryError>;

/// An embedder turns text into a fixed-dimension vector.
///
/// Implementations must produce vectors of [`Embedder::dim`] length on
/// every call; the storage layer validates and rejects mismatches.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Vector dimensionality.
    fn dim(&self) -> usize;

    /// Stable model identifier (e.g., `"bge-small-en-v1.5"`).
    fn model_name(&self) -> &str;

    /// Embed one piece of text.
    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>>;

    /// Embed a batch of texts. Default implementation calls [`embed`]
    /// sequentially; production embedders should override for batching.
    ///
    /// [`embed`]: Embedder::embed
    async fn embed_batch(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }
}

/// Deterministic test embedder. Hashes the input text to a small vector
/// of pseudo-random floats. **Never use in production** — produces
/// meaningless vectors.
///
/// Useful for unit tests of the SQLite layer where we just need *some*
/// vector to round-trip through `memory_vec`.
#[derive(Debug, Clone)]
pub struct MockEmbedder {
    dim: usize,
    model: String,
}

impl MockEmbedder {
    /// Mock embedder with the default `384` dimension, matching the
    /// `memory_vec` schema produced by the initial migration.
    pub fn new() -> Self {
        Self {
            dim: 384,
            model: "mock-384".into(),
        }
    }

    /// Mock embedder with an arbitrary dim. Use only for tests that
    /// override the migration schema.
    pub fn with_dim(dim: usize) -> Self {
        Self {
            dim,
            model: format!("mock-{dim}"),
        }
    }
}

impl Default for MockEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>> {
        // Tiny deterministic hash → seed → fill. Not cryptographic; just
        // makes equal inputs produce equal outputs and slightly differs
        // across short inputs.
        let mut seed: u64 = 0xcbf29ce484222325;
        for b in text.bytes() {
            seed ^= b as u64;
            seed = seed.wrapping_mul(0x100000001b3);
        }
        let mut out = Vec::with_capacity(self.dim);
        for i in 0..self.dim {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Map to [-1, 1].
            let f = ((seed >> (i % 32)) as i32) as f32 / i32::MAX as f32;
            out.push(f.clamp(-1.0, 1.0));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_embedder_deterministic() {
        let e = MockEmbedder::new();
        let a = e.embed("hello").await.unwrap();
        let b = e.embed("hello").await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 384);
    }

    #[tokio::test]
    async fn mock_embedder_different_for_different_input() {
        let e = MockEmbedder::new();
        let a = e.embed("hello").await.unwrap();
        let b = e.embed("world").await.unwrap();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn mock_embedder_with_dim_honors_dim() {
        let e = MockEmbedder::with_dim(8);
        let v = e.embed("hi").await.unwrap();
        assert_eq!(v.len(), 8);
        assert_eq!(e.dim(), 8);
    }

    #[tokio::test]
    async fn batch_default_works() {
        let e = MockEmbedder::new();
        let v = e
            .embed_batch(&["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].len(), 384);
    }
}
