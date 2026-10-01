//! Live file watcher for auto-reindexing on source changes.
//!
//! Watches one or more project directories and incrementally updates the code
//! graph when files are created, modified, or removed.
//!
//! Change batches are coalesced by project, including events queued during a refresh.
//! Each refresh re-resolves the entire project under the same database writer lock
//! used by explicit indexing. This costs a full parse per batch but preserves incoming
//! cross-file edges and publishes either the whole generation or none of it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, new_debouncer};
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use remembrall_core::graph::publication::refresh_project;
use remembrall_core::graph::store::GraphStore;
use remembrall_core::indexer::supported_extensions;

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
    pub async fn add_project(&self, root: PathBuf, project: String) -> remembrall_core::error::Result<()> {
        use remembrall_core::error::RemembrallError;
        if project.trim().is_empty() || root.as_os_str().is_empty() {
            return Err(RemembrallError::InvalidInput("watch root and project must not be empty".into()));
        }
        let root = root.canonicalize().map_err(|error|
            RemembrallError::InvalidInput(format!("cannot resolve watch root: {error}")))?;
        if !root.is_dir() {
            return Err(RemembrallError::InvalidInput(format!("watch root '{}' is not a directory", root.display())));
        }
        let mut projects = self.projects.lock().await;
        for (existing_root, existing_project) in projects.iter() {
            if (existing_root == &root && existing_project != &project)
                || (existing_project == &project && existing_root != &root) {
                return Err(RemembrallError::InvalidInput(format!(
                    "conflicting watcher ownership: '{}' belongs to '{}', cannot register '{}' as '{}'",
                    existing_root.display(), existing_project, root.display(), project)));
            }
        }
        projects.insert(root, project);
        Ok(())
    }

    /// Start the event loop. Runs until the process exits or the future is
    /// cancelled. Should be spawned as a background task.
    ///
    /// ```ignore
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

        // A dirty set is bounded by registered projects, even during a slow parse.
        // The single buffered token wakes the consumer without spawning queued tasks.
        let pending = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let pending_callback = Arc::clone(&pending);
        let (tx, mut rx) = mpsc::channel::<()>(1);

        // Build the debounced watcher on a dedicated thread (notify requires
        // the watcher to stay alive on the thread that created it, or at least
        // be kept alive by ownership - we move it into a blocking task).
        let roots: Vec<PathBuf> = projects_snapshot.keys().cloned().collect();
        let tx_clone = tx.clone();

        // spawn_blocking keeps the watcher alive and blocks the thread for the
        // duration of the process. The watcher itself uses OS-level APIs and
        // does not busy-loop.
        tokio::task::spawn_blocking(move || {
            let result = new_debouncer(
                Duration::from_millis(500),
                None,
                move |res: DebounceEventResult| match res {
                    Ok(events) => {
                        let paths: Vec<PathBuf> = events
                            .into_iter()
                            .flat_map(|event| source_event_paths(event.event))
                            .collect();
                        let changed = changed_projects(&paths, &projects_snapshot);
                        if !changed.is_empty() {
                            match pending_callback.lock() {
                                Ok(mut pending) => pending.extend(changed),
                                Err(error) => {
                                    tracing::error!("watcher pending set poisoned: {error}");
                                    return;
                                }
                            }
                            // Full means a wake is already queued; the dirty set keeps all projects.
                            let _ = tx_clone.try_send(());
                        }
                    }
                    Err(errs) => {
                        for e in errs {
                            tracing::warn!("watcher error: {e:?}");
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
        while rx.recv().await.is_some() {
            let roots = match pending.lock() {
                Ok(mut pending) => std::mem::take(&mut *pending),
                Err(error) => {
                    tracing::error!("watcher pending set poisoned: {error}");
                    break;
                }
            };
            for (root, project) in roots {
                match refresh_project(&self.graph, &root, &project).await {
                    Ok(result) => tracing::info!(
                        "refreshed {}: {} symbols, {} relationships",
                        root.display(),
                        result.symbols_stored,
                        result.relationships_stored
                    ),
                    Err(error) => tracing::warn!(
                        "refresh failed for {}; previous graph preserved: {error}",
                        root.display()
                    ),
                }
            }
        }

        tracing::info!("FileWatcher event loop exited");
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Access notifications must not schedule another full parse when our parser reads files.
fn source_event_paths(event: notify::Event) -> Vec<PathBuf> {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return vec![];
    }
    let directory_or_rename = matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder | CreateKind::Any)
            | EventKind::Remove(RemoveKind::Folder | RemoveKind::Any)
            | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Any)
    );
    event
        .paths
        .into_iter()
        .filter(|path| directory_or_rename || path.is_dir() || has_source_extension(path))
        .collect()
}

/// Include directory create/remove/rename events too: their children may have
/// vanished before notification and therefore cannot be inspected individually.
fn changed_projects(
    paths: &[PathBuf],
    projects: &HashMap<PathBuf, String>,
) -> HashSet<(PathBuf, String)> {
    paths
        .iter()
        .filter_map(|path| {
            let (root, project) = find_project(path, projects)?;
            // A checkout itself may live under .cursor/.codex. Exclusions are relative
            // to its registered root, not to the user's home directory.
            let relative = path.strip_prefix(&root).ok()?;
            (!is_ignored_path(relative)).then_some((root, project))
        })
        .collect()
}

/// Return true if this file should be watched (has a supported extension and
/// is not inside an ignored directory).
fn has_source_extension(path: &Path) -> bool {
    // Must be a file path, not a directory.
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_lowercase(),
        None => return false,
    };

    supported_extensions().contains(&ext.as_str())
}

fn is_ignored_path(path: &Path) -> bool {
    // Check every component of the path against the ignore list.
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name = name.to_string_lossy();
            if name.starts_with('.') && name != "." {
                return true;
            }
            if IGNORE_DIRS.contains(&name.as_ref()) {
                return true;
            }
        }
    }

    false
}

/// Find the registered project for a given file path by longest-prefix match.
fn find_project(path: &Path, projects: &HashMap<PathBuf, String>) -> Option<(PathBuf, String)> {
    projects
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.as_os_str().len())
        .map(|(root, name)| (root.clone(), name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_coalesce_by_longest_project_and_keep_directory_renames() {
        let projects = HashMap::from([
            (PathBuf::from("/repo"), "outer".to_string()),
            (PathBuf::from("/repo/inner"), "inner".to_string()),
        ]);
        let changed = changed_projects(
            &[
                PathBuf::from("/repo/inner/a.ts"),
                PathBuf::from("/repo/inner/b.ts"),
                PathBuf::from("/repo/src.v2"),
                PathBuf::from("/repo/node_modules/a.ts"),
            ],
            &projects,
        );
        assert_eq!(changed.len(), 2);
        assert!(changed.contains(&(PathBuf::from("/repo/inner"), "inner".into())));
        assert!(changed.contains(&(PathBuf::from("/repo"), "outer".into())));
    }

    #[test]
    fn extension_filter_uses_authoritative_registry() {
        for ext in supported_extensions() {
            assert!(has_source_extension(&PathBuf::from(format!(
                "/repo/example.{ext}"
            ))));
        }
        assert!(is_ignored_path(Path::new("node_modules/x.ts")));
        assert!(!has_source_extension(Path::new("/repo/image.png")));
    }
    #[test]
    fn access_events_do_not_loop_and_hidden_parent_roots_work() {
        use notify::event::{AccessKind, RenameMode};
        let path = PathBuf::from("/home/.cursor/worktrees/repo/src.v2");
        let access = notify::Event::new(EventKind::Access(AccessKind::Read)).add_path(path.clone());
        assert!(source_event_paths(access).is_empty());
        let rename = notify::Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(path.clone());
        let paths = source_event_paths(rename);
        assert_eq!(paths, vec![path]);
        let projects = HashMap::from([(
            PathBuf::from("/home/.cursor/worktrees/repo"),
            "project".into(),
        )]);
        assert_eq!(changed_projects(&paths, &projects).len(), 1);
    }
    #[tokio::test]
    async fn registration_rejects_project_collision_and_root_reassignment() {
        // connect_lazy only constructs a pool; registration must never query a database.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused_test").unwrap();
        let graph = Arc::new(GraphStore::new(pool, "watch_test".into()).unwrap());
        let watcher = FileWatcher::new(graph);
        let root = std::env::temp_dir().join(format!("watch-register-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        watcher.add_project(root.join("a"), "one".into()).await.unwrap();
        watcher.add_project(root.join("a/../a"), "one".into()).await.unwrap();
        assert!(watcher.add_project(root.join("b"), "one".into()).await.is_err());
        assert!(watcher.add_project(root.join("a"), "two".into()).await.is_err());
        watcher.add_project(root.join("b"), "two".into()).await.unwrap();
        #[cfg(unix)] {
            std::os::unix::fs::symlink(root.join("a"), root.join("alias")).unwrap();
            watcher.add_project(root.join("alias"), "one".into()).await.unwrap();
            assert!(watcher.add_project(root.join("alias"), "three".into()).await.is_err());
        }
        assert_eq!(watcher.projects.lock().await.len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn registration_rejects_invalid_root_or_empty_name() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1:1/unused_test").unwrap();
        let graph = Arc::new(GraphStore::new(pool, "watch_test".into()).unwrap());
        let watcher = FileWatcher::new(graph);
        let root = std::env::temp_dir().join(format!("watch-invalid-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file"), "content").unwrap();
        assert!(watcher.add_project(PathBuf::new(), "one".into()).await.is_err());
        assert!(watcher.add_project(root.join("missing"), "one".into()).await.is_err());
        assert!(watcher.add_project(root.join("file"), "one".into()).await.is_err());
        assert!(watcher.add_project(root.clone(), " ".into()).await.is_err());
        assert!(watcher.projects.lock().await.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

}
