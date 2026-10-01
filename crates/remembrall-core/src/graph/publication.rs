//! Shared full-index and watcher publication. Acquire project ownership before
//! reading files, resolve the complete graph, and expose it in one commit.

use std::path::Path;

use super::store::GraphStore;
use crate::error::{RemembrallError, Result};
use crate::parser::index_directory_strict;

#[derive(Debug)]
pub struct IndexPublication {
    pub files_parsed: usize,
    pub files_skipped: usize,
    pub symbols_stored: usize,
    pub relationships_stored: usize,
}

/// Refresh a complete project. Watcher refreshes deliberately use this same path:
/// replacing a single file with fresh IDs would lose incoming cross-file edges.
/// Parsing cost is proportional to project size; callers should coalesce changes.
pub async fn refresh_project(
    graph: &GraphStore,
    root: &Path,
    project: &str,
) -> Result<IndexPublication> {
    let update = graph.begin_project_update(project).await?;
    let root = root.to_owned();
    let project = project.to_owned();
    let snapshot = tokio::task::spawn_blocking(move || index_directory_strict(root, &project))
        .await
        .map_err(|e| RemembrallError::Internal(format!("index task failed: {e}")))?
        .map_err(|e| {
            RemembrallError::Internal(format!("index failed; previous graph preserved: {e}"))
        })?;
    let counts = update
        .replace(&snapshot.symbols, &snapshot.relationships)
        .await?;
    Ok(IndexPublication {
        files_parsed: snapshot.files_parsed,
        files_skipped: snapshot.files_skipped,
        symbols_stored: counts.symbols,
        relationships_stored: counts.relationships,
    })
}
