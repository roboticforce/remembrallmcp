//! Incremental code indexer.
//!
//! Walks a directory, compares file mtimes against the `file_index` table,
//! and calls a [`CodeParser`] for each new or changed file. Deleted files are
//! cleaned up from the graph store automatically.
//!
//! The indexer owns no parsing logic - it coordinates only. Swap the parser by
//! providing a different [`CodeParser`] implementation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::error::{EngramError, Result};
use crate::graph::types::{Relationship, Symbol};
use crate::graph::GraphStore;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of parsing a single source file.
pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub relationships: Vec<Relationship>,
}

/// Plug-in parsing interface. Implement this to teach the indexer about a new
/// language or analysis strategy. Parsing is synchronous because it is CPU-bound;
/// the indexer runs it on the calling async task (acceptable for now - callers
/// can spawn_blocking if needed).
pub trait CodeParser: Send + Sync {
    /// Parse `source` (the file contents) for the file at `file_path`.
    /// `language` is the normalised language tag (e.g. `"python"`, `"typescript"`).
    fn parse(&self, file_path: &str, source: &str, language: &str) -> Result<ParseResult>;
}

/// Configuration for a single index run.
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// Root directory to walk.
    pub root_path: PathBuf,

    /// Logical project name stored in the DB (used to namespace symbols).
    pub project: String,

    /// File extensions to index, without the leading dot (e.g. `["py", "ts"]`).
    pub extensions: Vec<String>,

    /// Directory names to skip entirely (e.g. `[".git", "node_modules"]`).
    pub ignore_patterns: Vec<String>,
}

impl IndexerConfig {
    /// Sensible defaults covering Python, TypeScript, JavaScript, and Rust.
    pub fn default_extensions() -> Vec<String> {
        vec!["py".into(), "ts".into(), "js".into(), "rs".into()]
    }

    /// Directories that almost never contain user code worth indexing.
    pub fn default_ignore_patterns() -> Vec<String> {
        vec![
            ".git".into(),
            "node_modules".into(),
            "__pycache__".into(),
            ".venv".into(),
            "venv".into(),
            "target".into(),
            ".mypy_cache".into(),
            ".pytest_cache".into(),
            "dist".into(),
            "build".into(),
            ".next".into(),
        ]
    }
}

/// Summary returned after each index run.
#[derive(Debug, Default)]
pub struct IndexStats {
    pub files_scanned: u64,
    pub files_changed: u64,
    pub files_deleted: u64,
    pub symbols_added: u64,
    pub relationships_added: u64,
    pub elapsed_ms: u64,
}

impl std::fmt::Display for IndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scanned={} changed={} deleted={} symbols={} relationships={} elapsed={}ms",
            self.files_scanned,
            self.files_changed,
            self.files_deleted,
            self.symbols_added,
            self.relationships_added,
            self.elapsed_ms,
        )
    }
}

// ---------------------------------------------------------------------------
// Indexer
// ---------------------------------------------------------------------------

/// Coordinates directory walking, mtime diffing, parsing, and graph storage.
pub struct Indexer {
    pool: PgPool,
    graph: GraphStore,
    schema: String,
    config: IndexerConfig,
}

impl Indexer {
    pub fn new(pool: PgPool, schema: String, config: IndexerConfig) -> Self {
        let graph = GraphStore::new(pool.clone(), schema.clone());
        Self { pool, graph, schema, config }
    }

    /// Ensure the `file_index` tracking table exists.
    pub async fn init(&self) -> Result<()> {
        self.graph.init().await?;

        sqlx::query(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS {schema}.file_index (
                file_path  TEXT        NOT NULL,
                project    TEXT        NOT NULL,
                mtime      TIMESTAMPTZ NOT NULL,
                indexed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY (file_path, project)
            )
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        info!("Indexer initialized (schema='{}')", self.schema);
        Ok(())
    }

    /// Run a full incremental index of the configured root path.
    ///
    /// Steps:
    /// 1. Walk the directory and collect candidate files with their disk mtimes.
    /// 2. Load the previous mtime snapshot from `file_index`.
    /// 3. Parse + store every new or changed file.
    /// 4. Remove any file that was previously indexed but no longer exists.
    pub async fn run(&self, parser: &dyn CodeParser) -> Result<IndexStats> {
        let started = Instant::now();
        let mut stats = IndexStats::default();

        // --- Step 1: walk disk ---
        let disk_files = self.collect_files()?;
        stats.files_scanned = disk_files.len() as u64;

        // --- Step 2: load stored mtimes ---
        let stored = self.load_stored_mtimes().await?;

        // --- Step 3: index new/changed files ---
        for (path, mtime) in &disk_files {
            let path_str = path.to_string_lossy();
            let needs_index = match stored.get(path_str.as_ref()) {
                None => true,
                Some(stored_mtime) => mtime != stored_mtime,
            };

            if !needs_index {
                debug!("unchanged: {path_str}");
                continue;
            }

            stats.files_changed += 1;
            match self.index_file(parser, path, *mtime, &mut stats).await {
                Ok(()) => {}
                Err(e) => {
                    warn!("Failed to index {path_str}: {e}");
                }
            }
        }

        // --- Step 4: remove deleted files ---
        let disk_set: HashSet<&str> =
            disk_files.iter().map(|(p, _)| p.to_str().unwrap_or("")).collect();

        for stored_path in stored.keys() {
            if !disk_set.contains(stored_path.as_str()) {
                info!("File deleted, removing from graph: {stored_path}");
                let removed = self.graph.remove_file(stored_path, &self.config.project).await?;
                self.delete_file_index_row(stored_path).await?;
                stats.files_deleted += 1;
                debug!("Removed {removed} symbols for {stored_path}");
            }
        }

        stats.elapsed_ms = started.elapsed().as_millis() as u64;
        info!("Index run complete: {stats}");
        Ok(stats)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Walk the directory tree and return (absolute_path, mtime_utc) for every
    /// file whose extension matches the config.
    fn collect_files(&self) -> Result<Vec<(PathBuf, DateTime<Utc>)>> {
        let ext_set: HashSet<&str> = self.config.extensions.iter().map(|s| s.as_str()).collect();
        let ignore_set: HashSet<&str> =
            self.config.ignore_patterns.iter().map(|s| s.as_str()).collect();

        let mut files = Vec::new();

        for entry in WalkDir::new(&self.config.root_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Skip ignored directory names at any depth.
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    return !ignore_set.contains(name.as_ref());
                }
                true
            })
        {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!("walkdir error: {err}");
                    continue;
                }
            };

            if !entry.file_type().is_file() {
                continue;
            }

            // Extension filter.
            let ext = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !ext_set.contains(ext) {
                continue;
            }

            // Read mtime from filesystem metadata.
            let mtime = match entry.metadata() {
                Ok(m) => system_time_to_utc(m.modified().map_err(|e| {
                    EngramError::Internal(format!("mtime read error: {e}"))
                })?)?,
                Err(e) => {
                    warn!("Cannot stat {}: {e}", entry.path().display());
                    continue;
                }
            };

            files.push((entry.into_path(), mtime));
        }

        Ok(files)
    }

    /// Load `(file_path -> mtime)` for the current project from `file_index`.
    async fn load_stored_mtimes(&self) -> Result<HashMap<String, DateTime<Utc>>> {
        let rows: Vec<(String, DateTime<Utc>)> = sqlx::query_as(&format!(
            "SELECT file_path, mtime FROM {schema}.file_index WHERE project = $1",
            schema = self.schema,
        ))
        .bind(&self.config.project)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().collect())
    }

    /// Parse one file and persist its symbols + relationships.
    async fn index_file(
        &self,
        parser: &dyn CodeParser,
        path: &Path,
        mtime: DateTime<Utc>,
        stats: &mut IndexStats,
    ) -> Result<()> {
        let path_str = path.to_string_lossy().into_owned();
        let language = language_for_extension(path);

        // Read source.
        let source = std::fs::read_to_string(path).map_err(|e| {
            EngramError::Internal(format!("read error for {path_str}: {e}"))
        })?;

        // Parse (synchronous, CPU-bound).
        let parsed = parser.parse(&path_str, &source, language)?;

        // Remove stale symbols for this file before inserting fresh ones.
        // (This handles renames/deletions of individual symbols within the file.)
        self.graph.remove_file(&path_str, &self.config.project).await?;

        // Persist symbols.
        for symbol in &parsed.symbols {
            self.graph.upsert_symbol(symbol).await?;
            stats.symbols_added += 1;
        }

        // Persist relationships.
        for rel in &parsed.relationships {
            self.graph.add_relationship(rel).await?;
            stats.relationships_added += 1;
        }

        // Update tracking row.
        self.upsert_file_index_row(&path_str, mtime).await?;

        debug!(
            "Indexed {path_str}: {} symbols, {} relationships",
            parsed.symbols.len(),
            parsed.relationships.len()
        );
        Ok(())
    }

    async fn upsert_file_index_row(&self, file_path: &str, mtime: DateTime<Utc>) -> Result<()> {
        sqlx::query(&format!(
            r#"
            INSERT INTO {schema}.file_index (file_path, project, mtime, indexed_at)
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (file_path, project) DO UPDATE SET
                mtime      = EXCLUDED.mtime,
                indexed_at = NOW()
            "#,
            schema = self.schema,
        ))
        .bind(file_path)
        .bind(&self.config.project)
        .bind(mtime)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_file_index_row(&self, file_path: &str) -> Result<()> {
        sqlx::query(&format!(
            "DELETE FROM {schema}.file_index WHERE file_path = $1 AND project = $2",
            schema = self.schema,
        ))
        .bind(file_path)
        .bind(&self.config.project)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Map a file extension to a normalised language tag.
fn language_for_extension(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        _ => "unknown",
    }
}

/// Convert a [`std::time::SystemTime`] to [`chrono::DateTime<Utc>`].
fn system_time_to_utc(st: std::time::SystemTime) -> Result<DateTime<Utc>> {
    let duration = st
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| EngramError::Internal(format!("system time before epoch: {e}")))?;
    let nanos = duration.subsec_nanos() as i64;
    let secs = duration.as_secs() as i64;
    DateTime::from_timestamp(secs, nanos as u32)
        .ok_or_else(|| EngramError::Internal("timestamp out of range".into()))
}
