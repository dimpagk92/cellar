//! `FastEmbedEmbedder` — the real local embedder, gated by the
//! `fastembed` feature.
//!
//! Uses [`fastembed::TextEmbedding`] with the `bge-small-en-v1.5` model
//! (384 dim) by default. The model file downloads on first instantiation
//! and caches at `~/.cache/fastembed` (or wherever the user's `XDG_CACHE_HOME`
//! points). Cellar's production config will point fastembed at
//! `~/.cellar/models/` once the daemon lifecycle work lands.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::embedder::{EmbedResult, Embedder};
use crate::error::SqliteMemoryError;

/// Default model identifier used by [`FastEmbedEmbedder::new`].
pub const DEFAULT_MODEL_NAME: &str = "bge-small-en-v1.5";

/// Dimensionality of the default model.
pub const DEFAULT_DIM: usize = 384;

/// fastembed-rs-backed [`Embedder`].
///
/// Internally wraps a [`TextEmbedding`] behind an `Arc<Mutex<…>>` because
/// the underlying ONNX session is not thread-safe for concurrent
/// `embed()` calls. The Arc is cloned into the blocking task that runs
/// each embed call; the lock is held only inside the blocking task.
pub struct FastEmbedEmbedder {
    inner: Arc<Mutex<TextEmbedding>>,
    dim: usize,
    model_name: String,
}

impl FastEmbedEmbedder {
    /// Construct with the default model (`bge-small-en-v1.5`, 384 dim).
    /// Downloads the model file on first use; cached afterward.
    pub fn new() -> Result<Self, SqliteMemoryError> {
        let inner = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))
            .map_err(|e| SqliteMemoryError::VecLoad(format!("fastembed init: {e}")))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            dim: DEFAULT_DIM,
            model_name: DEFAULT_MODEL_NAME.into(),
        })
    }

    /// Construct with explicit InitOptions — lets callers override the
    /// cache directory, model variant, or onnxruntime threading.
    pub fn with_options(
        opts: InitOptions,
        dim: usize,
        model_name: impl Into<String>,
    ) -> Result<Self, SqliteMemoryError> {
        let inner = TextEmbedding::try_new(opts)
            .map_err(|e| SqliteMemoryError::VecLoad(format!("fastembed init: {e}")))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
            dim,
            model_name: model_name.into(),
        })
    }
}

#[async_trait]
impl Embedder for FastEmbedEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }

    async fn embed(&self, text: &str) -> EmbedResult<Vec<f32>> {
        let text = text.to_string();
        let inner = Arc::clone(&self.inner);
        let v = tokio::task::spawn_blocking(move || -> Result<Vec<f32>, SqliteMemoryError> {
            let mut guard = inner
                .lock()
                .map_err(|e| SqliteMemoryError::VecLoad(format!("mutex poisoned: {e}")))?;
            let out = guard
                .embed(vec![text], None)
                .map_err(|e| SqliteMemoryError::VecLoad(format!("fastembed embed: {e}")))?;
            out.into_iter()
                .next()
                .ok_or_else(|| SqliteMemoryError::VecLoad("fastembed returned no vectors".into()))
        })
        .await
        .map_err(|e| SqliteMemoryError::BlockingJoin(e.to_string()))??;
        Ok(v)
    }

    async fn embed_batch(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
        let texts = texts.to_vec();
        let inner = Arc::clone(&self.inner);
        let out =
            tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>, SqliteMemoryError> {
                let mut guard = inner
                    .lock()
                    .map_err(|e| SqliteMemoryError::VecLoad(format!("mutex poisoned: {e}")))?;
                guard
                    .embed(texts, None)
                    .map_err(|e| SqliteMemoryError::VecLoad(format!("fastembed embed: {e}")))
            })
            .await
            .map_err(|e| SqliteMemoryError::BlockingJoin(e.to_string()))??;
        Ok(out)
    }
}
