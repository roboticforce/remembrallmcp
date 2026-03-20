//! Embedding generation for memory content.

use crate::error::Result;

/// Trait for generating text embeddings. Implementations are sync because
/// fastembed (ONNX Runtime) is CPU-bound and doesn't benefit from async.
/// Callers should use `spawn_blocking` if needed.
pub trait Embedder: Send + Sync {
    /// Embed a single text string.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed multiple texts in a batch (more efficient).
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Number of dimensions in the output vectors.
    fn dimensions(&self) -> usize;
}

/// Default embedder using fastembed (ONNX Runtime, all-MiniLM-L6-v2, 384-dim).
/// Downloads the model on first use (~23 MB).
pub struct FastEmbedder {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
}

impl FastEmbedder {
    pub fn new() -> Result<Self> {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_show_download_progress(true),
        )
        .map_err(|e| {
            crate::error::EngramError::Internal(format!(
                "Failed to load embedding model: {e}"
            ))
        })?;
        Ok(Self {
            model: std::sync::Mutex::new(model),
        })
    }
}

impl Embedder for FastEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut model = self.model.lock().map_err(|e| {
            crate::error::EngramError::Internal(format!(
                "Embedding model lock poisoned: {e}"
            ))
        })?;
        let results = model
            .embed(vec![text], None)
            .map_err(|e| {
                crate::error::EngramError::Internal(format!("Embedding failed: {e}"))
            })?;
        results.into_iter().next().ok_or_else(|| {
            crate::error::EngramError::Internal("No embedding returned".into())
        })
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut model = self.model.lock().map_err(|e| {
            crate::error::EngramError::Internal(format!(
                "Embedding model lock poisoned: {e}"
            ))
        })?;
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        model.embed(owned, None).map_err(|e| {
            crate::error::EngramError::Internal(format!("Batch embedding failed: {e}"))
        })
    }

    fn dimensions(&self) -> usize {
        384
    }
}
