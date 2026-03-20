pub mod config;
pub mod embed;
pub mod error;
pub mod memory;
pub mod graph;
pub mod indexer;
pub mod parser;
pub mod search;
pub mod ingest;

/// Synchronously embed a single text using a blocking thread.
/// Used by test harnesses that need to generate query embeddings from async contexts.
pub fn tokio_block_on_embed(
    embedder: &dyn embed::Embedder,
    text: &str,
) -> error::Result<Vec<f32>> {
    embedder.embed(text)
}
