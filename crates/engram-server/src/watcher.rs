//! Live file watcher for auto-reindexing on source changes.
//!
//! Watches one or more project directories and incrementally updates the code
//! graph when files are created, modified, or removed.
//!
//! Design decisions:
//! - 500ms debounce: agents make bursty writes (save + format + lint). We wait
//!   for the burst to settle before re-parsing.
//! - Per-file incremental: only the changed file is re-parsed. The rest of the
//!   graph is left intact.
//! - Cross-file import resolution is skipped for watcher updates. Relationships
//!   from the initial index remain correct; the watcher only refreshes symbols
//!   and intra-file relationships for the touched file.
//! - If a file fails to parse (e.g. syntax error mid-edit), we log and skip.
//!   The stale symbols remain in the graph until the file is saved in a valid
//!   state again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use engram_core::graph::store::GraphStore;
use engram_core::parser::{
    parse_go_file, parse_java_file, parse_kotlin_file, parse_python_file, parse_ruby_file,
    parse_rust_file, parse_ts_file, FileParseResult, TsLang,
};

/// Extensions we care about. All others are silently ignored.
const WATCHED_EXTENSIONS: &[&str] = &[
    "py", "ts", "tsx", "js", "jsx", "rs", "rb", "go", "java", "kt", "kts",
];

/// Directories that are never meaningful source roots - changes inside them
/// are discarded without touching the graph.
const IGNORE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    "vendor",
    ".venv",
    "venv",
    "dist",
    "build",
    ".cache",
    ".next",
    ".nuxt",
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Watches one or more project directories and keeps the graph up to date.
///
/// Constructed with [`FileWatcher::new`] then configured via [`FileWatcher::watch`].
/// Call [`FileWatcher::run`] to start the event loop (runs until the future is
/// dropped or the process exits).
pub struct FileWatcher {
    graph: Arc<GraphStore>,
    /// Map of watched root path -> project name.
    projects: Arc<Mutex<HashMap<PathBuf, String>>>,
}

impl FileWatcher {
    pub fn new(graph: Arc<GraphStore>) -> Self {
        Self {
            graph,
            projects: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a project directory for watching.
    ///
    /// This is additive - multiple directories can be registered. The actual
    /// OS-level watch is set up when [`run`] is called.
    pub async fn add_project(&self, root: PathBuf, project: String) {
        self.projects.lock().await.insert(root, project);
    }

    /// Start the event loop. Runs until the process exits or the future is
    /// cancelled. Should be spawned as a background task.
    ///
    /// ```no_run
    /// tokio::spawn(watcher.run());
    /// ```
    pub async fn run(self) {
        let projects_snapshot: HashMap<PathBuf, String> = {
            let guard = self.projects.lock().await;
            guard.clone()
        };

        if projects_snapshot.is_empty() {
            tracing::warn!("FileWatcher started with no projects - nothing to watch");
            return;
        }

        // Channel between the notify callback thread and the tokio runtime.
        // Buffer is generous: a large save burst can produce many events quickly.
        let (tx, mut rx) = mpsc::channel::<Vec<PathBuf>>(256);

        // Build the debounced watcher on a dedicated thread (notify requires
        // the watcher to stay alive on the thread that created it, or at least
        // be kept alive by ownership - we move it into a blocking task).
        let roots: Vec<PathBuf> = projects_snapshot.keys().cloned().collect();
        let tx_clone = tx.clone();

        // spawn_blocking keeps the watcher alive and blocks the thread for the
        // duration of the process. The watcher itself uses OS-level APIs and
        // does not busy-loop.
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();

            let result = new_debouncer(
                Duration::from_millis(500),
                None,
                move |res: DebounceEventResult| {
                    match res {
                        Ok(events) => {
                            let paths: Vec<PathBuf> = events
                                .into_iter()
                                .flat_map(|e| e.event.paths)
                                .collect();
                            if !paths.is_empty() {
                                let tx = tx_clone.clone();
                                rt.spawn(async move {
                                    let _ = tx.send(paths).await;
                                });
                            }
                        }
                        Err(errs) => {
                            for e in errs {
                                tracing::warn!("watcher error: {e:?}");
                            }
                        }
                    }
                },
            );

            let mut debouncer = match result {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("failed to create file watcher: {e}");
                    return;
                }
            };

            for root in &roots {
                if let Err(e) = debouncer.watch(root, RecursiveMode::Recursive) {
                    tracing::error!("failed to watch {}: {e}", root.display());
                } else {
                    tracing::info!("watching {} for changes", root.display());
                }
            }

            // Block the thread forever so the debouncer stays alive.
            // The channel sender keeps the other side notified when events arrive.
            std::thread::park();
        });

        // Drop the original sender so the channel closes when the blocking
        // task exits (which only happens if the process exits).
        drop(tx);

        // Process incoming change batches on the tokio side.
        while let Some(paths) = rx.recv().await {
            // Deduplicate - a single burst can contain the same path many times.
            let unique: HashSet<PathBuf> = paths.into_iter().collect();

            for path in unique {
                // Only process files with watched extensions that are not in
                // ignored directories.
                if !is_watched_file(&path) {
                    continue;
                }

                // Find which project this file belongs to.
                let project = match find_project(&path, &projects_snapshot) {
                    Some(p) => p,
                    None => {
                        tracing::debug!("change in unregistered path, skipping: {}", path.display());
                        continue;
                    }
                };

                reindex_file(&self.graph, &path, &project).await;
            }
        }

        tracing::info!("FileWatcher event loop exited");
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Re-index a single changed file.
///
/// Removes the file's old symbols from the graph, re-parses the file, and
/// inserts the new symbols. Errors are logged but do not propagate - a syntax
/// error mid-edit should never crash the watcher.
async fn reindex_file(graph: &Arc<GraphStore>, path: &Path, project: &str) {
    let file_path = path.to_string_lossy().to_string();

    // File was deleted.
    if !path.exists() {
        match graph.remove_file(&file_path, project).await {
            Ok(removed) if removed > 0 => {
                tracing::info!("removed {} symbols for deleted file: {}", removed, file_path);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("remove_file failed for {}: {e}", file_path);
            }
        }
        return;
    }

    // Read source. If it fails (permissions, binary, etc.) log and skip.
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("skipping {} - could not read: {e}", file_path);
            return;
        }
    };

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mtime = chrono::Utc::now();

    // Parsers are synchronous/CPU-bound (tree-sitter). Run on a blocking thread
    // to avoid stalling the tokio reactor.
    let file_path_clone = file_path.clone();
    let project_owned = project.to_string();
    let parse_result: FileParseResult = match tokio::task::spawn_blocking(move || {
        if ext == "py" {
            parse_python_file(&file_path_clone, &source, &project_owned, mtime)
        } else if ext == "rs" {
            parse_rust_file(&file_path_clone, &source, &project_owned, mtime)
        } else if ext == "rb" {
            parse_ruby_file(&file_path_clone, &source, &project_owned, mtime)
        } else if ext == "go" {
            parse_go_file(&file_path_clone, &source, &project_owned, mtime)
        } else if ext == "java" {
            parse_java_file(&file_path_clone, &source, &project_owned, mtime)
        } else if ext == "kt" || ext == "kts" {
            parse_kotlin_file(&file_path_clone, &source, &project_owned, mtime)
        } else if let Some(lang) = TsLang::from_extension(&ext) {
            parse_ts_file(&file_path_clone, &source, &project_owned, mtime, lang)
        } else {
            FileParseResult::default()
        }
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("parse panicked for {}: {e}", file_path);
            return;
        }
    };

    // If the extension was not matched, the result will be empty - skip.
    if parse_result.symbols.is_empty() && parse_result.relationships.is_empty() {
        return;
    }

    // Remove stale symbols for this file before upserting fresh ones.
    if let Err(e) = graph.remove_file(&file_path, project).await {
        tracing::warn!("remove_file failed for {}: {e}", file_path);
        return;
    }

    // Upsert new symbols.
    let mut symbols_stored = 0u64;
    for symbol in &parse_result.symbols {
        match graph.upsert_symbol(symbol).await {
            Ok(_) => symbols_stored += 1,
            Err(e) => {
                tracing::warn!("upsert_symbol failed for {} in {}: {e}", symbol.name, file_path);
            }
        }
    }

    // Store intra-file relationships. Cross-file relationships (to symbols in
    // other files) are skipped here - they were resolved during the initial
    // full index and remain valid unless those other files also changed.
    let mut rels_stored = 0u64;
    for rel in &parse_result.relationships {
        match graph.add_relationship(rel).await {
            Ok(_) => rels_stored += 1,
            Err(e) => {
                tracing::debug!("skipping relationship in {}: {e}", file_path);
            }
        }
    }

    tracing::info!(
        "reindexed {} - {} symbols, {} relationships",
        file_path,
        symbols_stored,
        rels_stored,
    );
}

/// Return true if this file should be watched (has a supported extension and
/// is not inside an ignored directory).
fn is_watched_file(path: &Path) -> bool {
    // Must be a file path, not a directory.
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_lowercase(),
        None => return false,
    };

    if !WATCHED_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }

    // Check every component of the path against the ignore list.
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name = name.to_string_lossy();
            if name.starts_with('.') && name != "." {
                return false;
            }
            if IGNORE_DIRS.contains(&name.as_ref()) {
                return false;
            }
        }
    }

    true
}

/// Find the registered project for a given file path by longest-prefix match.
fn find_project<'a>(
    path: &Path,
    projects: &'a HashMap<PathBuf, String>,
) -> Option<String> {
    projects
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.as_os_str().len())
        .map(|(_, name)| name.clone())
}
