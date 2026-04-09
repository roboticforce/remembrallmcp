//! Directory walker for incremental code indexing.
//!
//! Walks a directory tree, finds supported source files (.py, .ts, .tsx, .js, .jsx),
//! reads their content, delegates to the appropriate language parser, and returns
//! the combined set of symbols and relationships.
//!
//! Two-phase processing:
//!  1. Parse all files and collect raw import metadata.
//!  2. Resolve import paths against the full set of known file symbols, rewriting
//!     IMPORTS relationship target UUIDs to point at real file symbols.
//!     Also rewrite synthetic call target UUIDs for cross-file CALLS resolution.
//!
//! Incremental indexing: callers can pass a `since` timestamp; only files whose
//! `mtime` is newer will be parsed. The mtime is stored on every `Symbol` so
//! the graph store can cheaply determine which symbols need replacement.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::graph::layers::detect_layer;
use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::indexer::supported_extensions;
use crate::parser::python::{resolve_python_import, RawImport};
use crate::parser::{parse_file, FileParseResult};

/// Combined output from indexing a directory.
#[derive(Debug, Default)]
pub struct IndexResult {
    pub symbols: Vec<Symbol>,
    pub relationships: Vec<Relationship>,
    /// Number of files that were parsed.
    pub files_parsed: usize,
    /// Number of files skipped (unchanged since `since`).
    pub files_skipped: usize,
}

/// Walk `root_dir` and index all supported source files.
///
/// - `project`  - project name tag written to every `Symbol`
/// - `since`    - if `Some`, skip files whose mtime is older than this timestamp
///
/// Files and directories starting with `.` are skipped.
/// `node_modules`, `__pycache__`, `.git`, `target`, `dist`, and `build`
/// directories are skipped automatically.
pub fn index_directory(
    root_dir: impl AsRef<Path>,
    project: &str,
    since: Option<DateTime<Utc>>,
) -> anyhow::Result<IndexResult> {
    let root_dir = root_dir.as_ref();
    let mut result = IndexResult::default();
    // Collect all per-file results before resolving cross-file references.
    let mut file_results: Vec<FileParseResult> = Vec::new();

    for entry in WalkDir::new(root_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_skip(e.path()))
    {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("walkdir error: {err}");
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Check against the single authoritative extension list in indexer.rs.
        if !supported_extensions().contains(&ext.as_str()) {
            continue;
        }

        // Get mtime for incremental check.
        let mtime = file_mtime(path);

        if let Some(since_dt) = since {
            if mtime <= since_dt {
                result.files_skipped += 1;
                continue;
            }
        }

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("failed to read {}: {err}", path.display());
                continue;
            }
        };

        let file_path = path.to_string_lossy().to_string();

        // Dispatch to the correct parser via the central registry in parser/mod.rs.
        // If the extension somehow slipped through but has no parser, skip the file.
        let file_result: FileParseResult = match parse_file(&ext, &file_path, &source, project, mtime) {
            Some(r) => r,
            None => {
                tracing::warn!("no parser for extension '{ext}' in {file_path}");
                continue;
            }
        };

        result.files_parsed += 1;
        file_results.push(file_result);
    }

    // ---------------------------------------------------------------------------
    // Phase 2a: build resolution tables from all parsed symbols.
    // ---------------------------------------------------------------------------

    // Map: absolute_file_path_stem -> file_symbol_uuid
    // Used to resolve IMPORTS relationships to actual file symbols.
    let mut path_stem_to_uuid: HashMap<String, Uuid> = HashMap::new();
    for fr in &file_results {
        for sym in &fr.symbols {
            if sym.symbol_type == SymbolType::File {
                let p = sym.file_path.as_str();
                // Index by stem (no extension) and by full path with extension.
                let stem = strip_source_extension(p);
                path_stem_to_uuid.insert(stem.to_string(), sym.id);
                path_stem_to_uuid.insert(p.to_string(), sym.id);
            }
        }
    }

    // Map: synthetic_v5_uuid -> real_uuid  (for IMPORTS relationships)
    // Built by resolving each raw import against the known file paths.
    let mut import_resolution: HashMap<(Uuid, Uuid), Uuid> = HashMap::new();
    let mut resolved_count = 0usize;

    for fr in &file_results {
        let file_path = fr
            .symbols
            .first()
            .filter(|s| s.symbol_type == SymbolType::File)
            .map(|s| s.file_path.as_str())
            .unwrap_or("");

        for raw in &fr.raw_imports {
            let placeholder_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, raw.module_raw.as_bytes());
            if let Some(resolved_uuid) =
                resolve_import_to_uuid(file_path, raw, &path_stem_to_uuid)
            {
                import_resolution.insert((raw.source_id, placeholder_id), resolved_uuid);
                resolved_count += 1;
            }
        }
    }

    // Map: synthetic_v5_uuid -> Vec<real_uuid>  (for cross-file CALLS relationships)
    //
    // When multiple symbols share the same bare name (e.g., several `start` methods
    // across different classes), we emit one Calls edge per candidate so that any
    // comparator checking for a specific target can find it.
    let mut synthetic_to_real: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for fr in &file_results {
        for s in &fr.symbols {
            if matches!(
                s.symbol_type,
                SymbolType::Function | SymbolType::Method | SymbolType::Class
            ) {
                let key = Uuid::new_v5(&Uuid::NAMESPACE_OID, s.name.as_bytes());
                synthetic_to_real.entry(key).or_default().push(s.id);
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Phase 2b: merge all symbols and rewrite relationships.
    // ---------------------------------------------------------------------------

    // Propagate layer detection: for each file's parse result, detect the layer
    // from the file path and apply it to all symbols in that file.
    let file_results: Vec<_> = file_results
        .into_iter()
        .map(|mut fr| {
            // Determine the layer from the file symbol (always index 0).
            let file_path = fr
                .symbols
                .first()
                .filter(|s| s.symbol_type == SymbolType::File)
                .map(|s| s.file_path.as_str())
                .unwrap_or("");
            let layer = detect_layer(file_path);
            // Apply the same layer to every symbol in this file.
            for sym in &mut fr.symbols {
                sym.layer = layer.clone();
            }
            fr
        })
        .collect();

    for fr in file_results {
        result.symbols.extend(fr.symbols);

        for rel in fr.relationships {
            if rel.rel_type == RelationType::Imports {
                let key = (rel.source_id, rel.target_id);
                if let Some(&resolved_target) = import_resolution.get(&key) {
                    result.relationships.push(Relationship {
                        source_id: rel.source_id,
                        target_id: resolved_target,
                        rel_type: RelationType::Imports,
                        confidence: 1.0,
                    });
                    continue;
                }
                // Unresolved import (stdlib, third-party) - keep placeholder but
                // skip storing it since the target file symbol doesn't exist in
                // this project and the graph store would reject the FK anyway.
                // We still push it so callers can inspect it if desired.
                result.relationships.push(rel);
            } else if rel.rel_type == RelationType::Calls
                || rel.rel_type == RelationType::UsesType
            {
                // For CALLS and USES_TYPE: when a synthetic target UUID maps to
                // multiple real symbols (ambiguous name), emit one edge per candidate.
                if let Some(real_ids) = synthetic_to_real.get(&rel.target_id) {
                    let conf = rel.confidence / real_ids.len() as f32;
                    for &real_id in real_ids {
                        result.relationships.push(Relationship {
                            source_id: rel.source_id,
                            target_id: real_id,
                            rel_type: rel.rel_type.clone(),
                            confidence: conf,
                        });
                    }
                } else {
                    result.relationships.push(rel);
                }
            } else {
                // For DEFINES, INHERITS: rewrite synthetic target UUIDs
                // to real symbol UUIDs where possible (single match).
                let mut rel = rel;
                if let Some(real_ids) = synthetic_to_real.get(&rel.target_id) {
                    if let Some(&first) = real_ids.first() {
                        rel.target_id = first;
                    }
                }
                result.relationships.push(rel);
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Phase 2c: derive Import edges from cross-file relationships.
    // ---------------------------------------------------------------------------
    // Languages with autoloading (e.g., Ruby/Rails with Zeitwerk) may not have
    // explicit import statements. We supplement by deriving Import edges: if a
    // symbol in file A has a resolved Inherits, Calls, or UsesType edge pointing
    // at a symbol in file B, we add an Imports edge from file A to file B.

    let sym_to_file: HashMap<Uuid, &str> = result
        .symbols
        .iter()
        .map(|s| (s.id, s.file_path.as_str()))
        .collect();

    let file_to_sym: HashMap<&str, Uuid> = result
        .symbols
        .iter()
        .filter(|s| s.symbol_type == SymbolType::File)
        .map(|s| (s.file_path.as_str(), s.id))
        .collect();

    let mut import_pairs: HashSet<(Uuid, Uuid)> = result
        .relationships
        .iter()
        .filter(|r| r.rel_type == RelationType::Imports)
        .map(|r| (r.source_id, r.target_id))
        .collect();

    let mut derived_imports = Vec::new();
    for rel in &result.relationships {
        if !matches!(
            rel.rel_type,
            RelationType::Inherits | RelationType::Calls | RelationType::UsesType
        ) {
            continue;
        }
        let src_file = sym_to_file
            .get(&rel.source_id)
            .and_then(|fp| file_to_sym.get(fp))
            .copied();
        let tgt_file = sym_to_file
            .get(&rel.target_id)
            .and_then(|fp| file_to_sym.get(fp))
            .copied();

        if let (Some(sf), Some(tf)) = (src_file, tgt_file) {
            if sf != tf && !import_pairs.contains(&(sf, tf)) {
                import_pairs.insert((sf, tf));
                derived_imports.push(Relationship {
                    source_id: sf,
                    target_id: tf,
                    rel_type: RelationType::Imports,
                    confidence: 0.6,
                });
            }
        }
    }

    let derived_count = derived_imports.len();
    result.relationships.extend(derived_imports);

    tracing::info!(
        "Indexed {} - {} files parsed, {} skipped, {} symbols, {} relationships ({} imports resolved, {} derived)",
        root_dir.display(),
        result.files_parsed,
        result.files_skipped,
        result.symbols.len(),
        result.relationships.len(),
        resolved_count,
        derived_count,
    );

    Ok(result)
}

// ---------------------------------------------------------------------------
// Import resolution
// ---------------------------------------------------------------------------

/// Attempt to resolve a raw import to a file symbol UUID.
///
/// Returns `None` if the import cannot be resolved (e.g., stdlib, third-party).
fn resolve_import_to_uuid(
    importing_file: &str,
    raw: &RawImport,
    path_stem_to_uuid: &HashMap<String, Uuid>,
) -> Option<Uuid> {
    if raw.is_relative {
        resolve_relative_import(importing_file, raw, path_stem_to_uuid)
    } else {
        resolve_absolute_import(&raw.module_path, path_stem_to_uuid)
    }
}

/// Resolve a relative import to a UUID.
///
/// Python: uses dot_count + module_path (e.g., dot_count=2, module_path="storage.work_queue")
/// TypeScript: module_path is the raw specifier (e.g., "./types" or "../utils/helper")
/// Rust: module_path is a slash-path ("foo/bar") from a `crate::` or `super::` use declaration.
///       dot_count is always 0; Rust paths are identified by `::` in module_raw.
fn resolve_relative_import(
    importing_file: &str,
    raw: &RawImport,
    path_stem_to_uuid: &HashMap<String, Uuid>,
) -> Option<Uuid> {
    // Rust crate-relative imports have `::` in their raw module string (e.g., "crate::foo::Bar").
    // TypeScript relative imports start with "./" or "../" in the raw module specifier.
    // Python relative imports use dot_count > 0.
    //
    // NOTE: Do NOT use module_path.contains('/') to detect TS, because Rust module_path
    // has already been converted from "::" to "/" (e.g., "assets/HighlightingAssets"),
    // which would falsely trigger as a TS path.
    let is_rust_path = raw.dot_count == 0 && raw.module_raw.contains("::");
    let is_ts_path = !is_rust_path
        && (raw.module_raw.starts_with("./")
            || raw.module_raw.starts_with("../")
            || raw.module_path.starts_with("./")
            || raw.module_path.starts_with("../"));

    if is_rust_path {
        return resolve_rust_crate_import(&raw.module_path, path_stem_to_uuid);
    }

    let resolved_stem = if is_ts_path {
        resolve_ts_relative(importing_file, &raw.module_path)?
    } else {
        resolve_python_import(importing_file, raw.dot_count, &raw.module_path)?
    };

    try_path_variants(&resolved_stem, path_stem_to_uuid)
}

/// Resolve a Rust crate-internal use path (already in slash form) via suffix matching.
///
/// Tries progressively shorter paths so that `assets/HighlightingAssets` can
/// resolve to `assets.rs` when there is no file named `assets/HighlightingAssets.rs`.
///
/// Examples:
///   `memory/store`              -> matches `.../memory/store.rs`
///   `assets/HighlightingAssets` -> first tries `assets/HighlightingAssets.rs` (no match),
///                                  then tries `assets` -> matches `.../assets.rs`
fn resolve_rust_crate_import(
    slash_path: &str,
    path_stem_to_uuid: &HashMap<String, Uuid>,
) -> Option<Uuid> {
    if slash_path.is_empty() {
        return None;
    }

    // Try the path as-is, then progressively drop the last segment until we find a match.
    // Prefer the LONGEST match (most specific file stem) to avoid false matches on short
    // path suffixes.  Also, prefer full paths (with extension) over stems to ensure
    // a deterministic result when both are in the map pointing to the same UUID.
    let mut current = slash_path;
    loop {
        // Collect all matching stems for this candidate suffix, pick the SHORTEST one
        // (closest to the crate root) to prefer root-level modules over nested ones
        // with the same name (e.g., "src/assets.rs" over "src/bin/bat/assets.rs").
        let best = path_stem_to_uuid
            .iter()
            .filter_map(|(file_stem, &uuid)| {
                if !file_stem.ends_with(current) {
                    return None;
                }
                let remainder = &file_stem[..file_stem.len() - current.len()];
                if remainder.is_empty() || remainder.ends_with('/') {
                    Some((file_stem.len(), uuid))
                } else {
                    None
                }
            })
            .min_by_key(|(len, _)| *len);

        if let Some((_, uuid)) = best {
            return Some(uuid);
        }

        // Drop the last slash-separated segment and retry.
        if let Some(last_slash) = current.rfind('/') {
            current = &current[..last_slash];
        } else {
            break;
        }
    }

    None
}

/// Resolve a TypeScript relative import specifier to an absolute path stem.
///
/// `./types` from `/project/src/foo.ts` -> `/project/src/types`
fn resolve_ts_relative(importing_file: &str, module_specifier: &str) -> Option<String> {
    let file = Path::new(importing_file);
    let file_dir = file.parent()?;
    let joined = file_dir.join(module_specifier);
    let normalized = normalize_path(&joined);
    Some(normalized.to_string_lossy().to_string())
}

/// Resolve an absolute/package import using suffix matching against known file paths.
///
/// `sugar.memory.store` may match `/Users/steve/Dev/sugar/sugar/memory/store` stem.
///
/// For Rust binary crates that import from their library crate using the crate
/// name as the first path segment (e.g. `bat::controller::Controller`), the
/// first segment is stripped and the remainder is resolved as a crate-root
/// relative import.  This handles patterns like:
///   `use bat::controller::Controller` -> strip `bat` -> resolve `controller/Controller`
fn resolve_absolute_import(
    module_path: &str,
    path_stem_to_uuid: &HashMap<String, Uuid>,
) -> Option<Uuid> {
    let slash_path = module_path.replace('.', "/");

    for (file_stem, &uuid) in path_stem_to_uuid {
        if file_stem.ends_with(&slash_path) {
            // Ensure we matched a complete path component boundary.
            let remainder = &file_stem[..file_stem.len() - slash_path.len()];
            if remainder.is_empty() || remainder.ends_with('/') {
                return Some(uuid);
            }
        }
    }

    // Rust binary-crate-imports-library pattern: `bat/controller/Controller`
    // The first segment is the crate name.  Strip it and try to resolve the
    // rest as a project-internal path (same logic as `resolve_rust_crate_import`).
    if slash_path.contains('/') {
        let after_first_slash = slash_path.splitn(2, '/').nth(1).unwrap_or("");
        if !after_first_slash.is_empty() {
            if let Some(uuid) = resolve_rust_crate_import(after_first_slash, path_stem_to_uuid) {
                return Some(uuid);
            }
        }
    }

    None
}

/// Try several path extension variants to find a matching file symbol.
///
/// Tries the stem as-is, then with common source extensions appended, and
/// package index variants (`__init__.py`, `index.ts`, etc.).
/// Falls back to scanning for any file under the stem directory when the
/// import specifier points to a directory without an index file (e.g.,
/// `./router/smart-router` -> `router/smart-router/router.ts`).
fn try_path_variants(stem: &str, path_stem_to_uuid: &HashMap<String, Uuid>) -> Option<Uuid> {
    let candidates = [
        stem.to_string(),
        format!("{stem}.py"),
        format!("{stem}/__init__.py"),
        format!("{stem}.ts"),
        format!("{stem}.tsx"),
        format!("{stem}/index.ts"),
        format!("{stem}/index.tsx"),
        format!("{stem}.js"),
        format!("{stem}/index.js"),
        format!("{stem}.rs"),
        format!("{stem}/mod.rs"),
        format!("{stem}.go"),
        format!("{stem}.rb"),
        format!("{stem}.java"),
        format!("{stem}.kt"),
        format!("{stem}.kts"),
    ];

    for candidate in &candidates {
        if let Some(&uuid) = path_stem_to_uuid.get(candidate.as_str()) {
            return Some(uuid);
        }
    }

    // Fallback: if the stem looks like a directory (no extension in the last segment),
    // scan for any file that lives directly under that directory.  This handles
    // TypeScript packages where the entry point is not `index.ts` but named after the
    // directory (e.g., `./router/smart-router` -> `router/smart-router/router.ts`).
    let dir_prefix = format!("{stem}/");
    let mut best: Option<(&str, &Uuid)> = None;
    for (key, uuid) in path_stem_to_uuid {
        if !key.starts_with(&dir_prefix) {
            continue;
        }
        let after_prefix = &key[dir_prefix.len()..];
        // Only match direct children (no further slashes in the remaining segment).
        if after_prefix.contains('/') {
            continue;
        }
        // Prefer shorter keys (closer to the directory root) and TypeScript files.
        let is_better = match best {
            None => true,
            Some((prev_key, _)) => key.len() < prev_key.len(),
        };
        if is_better {
            best = Some((key.as_str(), uuid));
        }
    }
    if let Some((_, &uuid)) = best {
        return Some(uuid);
    }

    None
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Strip the source file extension to get a canonical path stem.
fn strip_source_extension(path: &str) -> &str {
    let extensions = [
        ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".rs", ".rb", ".go", ".java",
        ".kt", ".kts",
    ];
    for ext in &extensions {
        if path.ends_with(ext) {
            return &path[..path.len() - ext.len()];
        }
    }
    path
}

/// Normalize a path by resolving `..` and `.` components without filesystem access.
fn normalize_path(path: &Path) -> std::path::PathBuf {
    let mut components: Vec<std::path::Component> = Vec::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                components.pop();
            }
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// Read the mtime of a file, returning epoch start on failure.
fn file_mtime(path: &Path) -> DateTime<Utc> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            let secs = t
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            DateTime::from_timestamp(secs, 0).unwrap_or_else(|| Utc::now())
        })
        .unwrap_or_else(|_| Utc::now())
}

/// Return true if the path should be excluded from indexing.
fn should_skip(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    // Hidden files/directories.
    if name.starts_with('.') {
        return true;
    }

    // Well-known non-source directories.
    matches!(
        name,
        "node_modules"
            | "__pycache__"
            | "target"
            | "dist"
            | "build"
            | ".git"
            | "vendor"
            | "venv"
            | ".venv"
            | "log"
            | "logs"
            | "tmp"
            | "coverage"
            | "public"
            | "storage"
            | ".bundle"
            | "e2e"
            | "playwright-report"
    )
}
