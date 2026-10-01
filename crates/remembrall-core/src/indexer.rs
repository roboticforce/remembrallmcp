//! Code indexer with atomic project snapshots.
//!
//! Walks a directory and compares mtimes for reporting, but parses the complete
//! configured file set through [`CodeParser`] before atomically publishing graph
//! and tracking rows. Errors preserve the previous complete snapshot.
//!
//! The indexer owns no parsing logic - it coordinates only. Swap the parser by
//! providing a different [`CodeParser`] implementation.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::info;
use walkdir::WalkDir;

use crate::config::validate_schema_name;
use crate::error::{RemembrallError, Result};
use crate::graph::GraphStore;
use crate::graph::types::{Relationship, Symbol};

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
    /// All supported extensions. Uses [`supported_extensions`] as the source of truth.
    pub fn default_extensions() -> Vec<String> {
        supported_extensions()
            .iter()
            .map(|s| (*s).to_string())
            .collect()
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
    pub fn new(pool: PgPool, schema: String, config: IndexerConfig) -> Result<Self> {
        validate_schema_name(&schema).map_err(RemembrallError::InvalidInput)?;
        let graph = GraphStore::new(pool.clone(), schema.clone())?;
        Ok(Self {
            pool,
            graph,
            schema,
            config,
        })
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

    /// Parse a complete configured snapshot under the project writer lock.
    /// The custom parser contract is preserved. Mtimes report changed files but
    /// no longer suppress parsing: fresh UUIDs require complete relationships.
    pub async fn run(&self, parser: &dyn CodeParser) -> Result<IndexStats> {
        let started = Instant::now();
        let mut update = self
            .graph
            .begin_project_update(&self.config.project)
            .await?;
        let disk_files = self.collect_files()?;
        let stored = update.file_mtimes().await?;
        let mut stats = IndexStats {
            files_scanned: disk_files.len() as u64,
            ..Default::default()
        };
        let mut symbols = Vec::new();
        let mut relationships = Vec::new();
        let mut mtimes = Vec::new();
        for (path, mtime) in &disk_files {
            let path_str = path.to_string_lossy().into_owned();
            if stored.get(&path_str) != Some(mtime) {
                stats.files_changed += 1;
            }
            let source = std::fs::read_to_string(path).map_err(|error| {
                RemembrallError::Internal(format!("read failed for {path_str}: {error}"))
            })?;
            let parsed = parser.parse(&path_str, &source, language_for_extension(path))?;
            symbols.extend(parsed.symbols);
            relationships.extend(parsed.relationships);
            mtimes.push((path_str, *mtime));
        }
        let disk_set: HashSet<_> = mtimes.iter().map(|(path, _)| path).collect();
        stats.files_deleted = stored
            .keys()
            .filter(|path| !disk_set.contains(path))
            .count() as u64;
        let counts = update
            .replace_with_file_mtimes(&symbols, &relationships, &mtimes)
            .await?;
        stats.symbols_added = counts.symbols as u64;
        stats.relationships_added = counts.relationships as u64;
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
        if !self.config.root_path.is_dir() {
            return Err(RemembrallError::InvalidInput(
                "index root must be an existing directory".into(),
            ));
        }
        let ext_set: HashSet<&str> = self.config.extensions.iter().map(|s| s.as_str()).collect();
        let ignore_set: HashSet<&str> = self
            .config
            .ignore_patterns
            .iter()
            .map(|s| s.as_str())
            .collect();

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
                    return Err(RemembrallError::Internal(format!("walkdir error: {err}")));
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
            let mtime =
                match entry.metadata() {
                    Ok(m) => system_time_to_utc(m.modified().map_err(|e| {
                        RemembrallError::Internal(format!("mtime read error: {e}"))
                    })?)?,
                    Err(e) => {
                        return Err(RemembrallError::Internal(format!(
                            "cannot stat {}: {e}",
                            entry.path().display()
                        )));
                    }
                };

            files.push((entry.into_path(), mtime));
        }

        Ok(files)
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// All file extensions the indexer and walker support, without a leading dot.
///
/// This is the single authoritative list. `IndexerConfig::default_extensions`
/// and `parser::walker` both derive from this set - update here first when
/// adding language support.
pub fn supported_extensions() -> &'static [&'static str] {
    &[
        "py",   // Python
        "ts",   // TypeScript
        "tsx",  // TypeScript + JSX
        "js",   // JavaScript
        "jsx",  // JavaScript + JSX
        "mjs",  // ES module JavaScript
        "cjs",  // CommonJS JavaScript
        "rs",   // Rust
        "go",   // Go
        "rb",   // Ruby
        "java", // Java
        "kt",   // Kotlin
        "kts",  // Kotlin script
        "cs",   // C#
    ]
}

/// Map a file extension to a normalised language tag.
fn language_for_extension(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        _ => "unknown",
    }
}

/// Convert a [`std::time::SystemTime`] to [`chrono::DateTime<Utc>`].
fn system_time_to_utc(st: std::time::SystemTime) -> Result<DateTime<Utc>> {
    let duration = st
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| RemembrallError::Internal(format!("system time before epoch: {e}")))?;
    let nanos = duration.subsec_nanos() as i64;
    let secs = duration.as_secs() as i64;
    DateTime::from_timestamp(secs, nanos as u32)
        .ok_or_else(|| RemembrallError::Internal("timestamp out of range".into()))
}
