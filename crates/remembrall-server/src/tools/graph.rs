//! Graph tool parameter structs and implementation helpers.
//!
//! Covers: remembrall_index, remembrall_impact, remembrall_lookup_symbol, remembrall_tour.
//! The `#[tool]` wrapper methods live in `lib.rs` (required by `#[tool_router]`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{ErrorData as McpError, model::*, schemars};
use serde_json::json;
use tokio::sync::Mutex;

use remembrall_core::{
    graph::{
        store::GraphStore,
        types::{Direction, SymbolType, TourStop},
    },
    parser::index_directory,
};

use crate::watcher;

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexParams {
    #[schemars(description = "Absolute path to the project root directory")]
    pub path: String,
    #[schemars(description = "Logical project name for namespacing")]
    pub project: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ImpactParams {
    #[schemars(description = "Name of the function, class, or method to analyze")]
    pub symbol_name: String,
    #[schemars(description = "One of: function, class, method, file")]
    pub symbol_type: Option<String>,
    #[schemars(description = "Filter to a specific project (as used in remembrall_index)")]
    pub project: Option<String>,
    #[schemars(description = "upstream (who calls me?), downstream (what do I call?), or both. Default: upstream")]
    pub direction: Option<String>,
    #[schemars(description = "How many levels deep to traverse (default 3, max 10)")]
    pub max_depth: Option<i32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LookupParams {
    #[schemars(description = "Symbol name to look up")]
    pub name: String,
    #[schemars(description = "Filter: function, class, method, file")]
    pub symbol_type: Option<String>,
    #[schemars(description = "Filter to a specific project (as used in remembrall_index)")]
    pub project: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TourParams {
    #[schemars(description = "Project name (must have been indexed first with remembrall_index)")]
    pub project: String,
    #[schemars(description = "Maximum number of stops in the tour (default 20)")]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Logic helpers
// ---------------------------------------------------------------------------

pub async fn index_impl(
    graph: &Arc<GraphStore>,
    watched_dirs: &Arc<Mutex<HashSet<PathBuf>>>,
    params: IndexParams,
) -> Result<CallToolResult, McpError> {
    let IndexParams { path, project } = params;

    let graph_arc = Arc::clone(graph);
    let path_clone = path.clone();
    let project_clone = project.clone();

    let index_result = tokio::task::spawn_blocking(move || {
        index_directory(&path_clone, &project_clone, None)
    })
    .await
    .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
    .map_err(|e| McpError::internal_error(format!("index_directory failed: {e}"), None))?;

    graph_arc
        .upsert_symbols_batch(&index_result.symbols)
        .await
        .map_err(|e| McpError::internal_error(format!("upsert_symbols_batch failed: {e}"), None))?;

    graph_arc
        .add_relationships_batch(&index_result.relationships)
        .await
        .map_err(|e| McpError::internal_error(format!("add_relationships_batch failed: {e}"), None))?;

    let symbols_stored = index_result.symbols.len() as u64;
    let relationships_stored = index_result.relationships.len() as u64;

    let canonical = std::path::Path::new(&path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&path));

    let already_watching = {
        let mut guard = watched_dirs.lock().await;
        if guard.contains(&canonical) {
            true
        } else {
            guard.insert(canonical.clone());
            false
        }
    };

    if !already_watching {
        let watcher_graph = Arc::clone(graph);
        let project_name = project.clone();
        let watch_root = canonical.clone();

        tokio::spawn(async move {
            let fw = watcher::FileWatcher::new(watcher_graph);
            fw.add_project(watch_root, project_name).await;
            fw.run().await;
        });

        tracing::info!("background watcher started for {}", canonical.display());
    }

    let text = json!({
        "path": path,
        "project": project,
        "files_parsed": index_result.files_parsed,
        "files_skipped": index_result.files_skipped,
        "symbols_stored": symbols_stored,
        "relationships_stored": relationships_stored,
        "watching": !already_watching,
        "note": "Indexing complete. A background watcher is now keeping the graph up to date.",
    })
    .to_string();

    Ok(CallToolResult::success(vec![Content::text(text)]))
}

pub async fn impact_impl(
    graph: &Arc<GraphStore>,
    params: ImpactParams,
) -> Result<CallToolResult, McpError> {
    let ImpactParams { symbol_name, symbol_type, project, direction, max_depth } = params;

    let stype: Option<SymbolType> = symbol_type
        .as_deref()
        .map(|s| {
            s.parse::<SymbolType>().map_err(|e: String| {
                McpError::invalid_params(format!("invalid symbol_type: {e}"), None)
            })
        })
        .transpose()?;

    let symbols = graph
        .find_symbol(&symbol_name, stype.as_ref(), project.as_deref())
        .await
        .map_err(|e| McpError::internal_error(format!("find_symbol failed: {e}"), None))?;

    if symbols.is_empty() {
        let text = json!({
            "symbol_name": symbol_name,
            "found": false,
            "message": "Symbol not found in graph. Has the project been indexed?",
        })
        .to_string();
        return Ok(CallToolResult::success(vec![Content::text(text)]));
    }

    let symbol = &symbols[0];

    let dir = match direction.as_deref().unwrap_or("upstream") {
        "downstream" => Direction::Downstream,
        "both" => Direction::Both,
        _ => Direction::Upstream,
    };

    let depth = max_depth.unwrap_or(3).min(10).max(1);

    let results = graph
        .impact_analysis(symbol.id, dir, depth)
        .await
        .map_err(|e| McpError::internal_error(format!("impact_analysis failed: {e}"), None))?;

    let mut files: Vec<String> = results
        .iter()
        .map(|r| r.symbol.file_path.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    files.sort();

    let affected: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            json!({
                "name": r.symbol.name,
                "type": r.symbol.symbol_type.to_string(),
                "file": r.symbol.file_path,
                "layer": r.symbol.layer,
                "depth": r.depth,
                "relationship": r.relationship.to_string(),
                "confidence": r.confidence,
            })
        })
        .collect();

    let mut layers_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(ref l) = symbol.layer {
        layers_set.insert(l.clone());
    }
    for r in &results {
        if let Some(ref l) = r.symbol.layer {
            layers_set.insert(l.clone());
        }
    }
    let layers_crossed: Vec<String> = layers_set.into_iter().collect();
    let layer_crossing_count = layers_crossed.len();

    let mut response = json!({
        "symbol": symbol.name,
        "file": symbol.file_path,
        "layer": symbol.layer,
        "direction": format!("{:?}", dir).to_lowercase(),
        "affected_symbols": affected,
        "affected_files": files,
        "total_symbols": affected.len(),
        "total_files": files.len(),
        "layers_crossed": layers_crossed,
        "layer_crossing_count": layer_crossing_count,
    });

    if layer_crossing_count >= 3 {
        response["risk_note"] = json!(format!(
            "This change crosses {} architectural layers - review carefully.",
            layer_crossing_count
        ));
    }

    Ok(CallToolResult::success(vec![Content::text(response.to_string())]))
}

pub async fn lookup_symbol_impl(
    graph: &Arc<GraphStore>,
    params: LookupParams,
) -> Result<CallToolResult, McpError> {
    let LookupParams { name, symbol_type, project } = params;

    let stype: Option<SymbolType> = symbol_type
        .as_deref()
        .map(|s| {
            s.parse::<SymbolType>().map_err(|e: String| {
                McpError::invalid_params(format!("invalid symbol_type: {e}"), None)
            })
        })
        .transpose()?;

    let symbols = graph
        .find_symbol(&name, stype.as_ref(), project.as_deref())
        .await
        .map_err(|e| McpError::internal_error(format!("find_symbol failed: {e}"), None))?;

    let result: Vec<serde_json::Value> = symbols
        .iter()
        .map(|s| {
            json!({
                "id": s.id.to_string(),
                "name": s.name,
                "type": s.symbol_type.to_string(),
                "file": s.file_path,
                "start_line": s.start_line,
                "end_line": s.end_line,
                "language": s.language,
                "project": s.project,
                "signature": s.signature,
            })
        })
        .collect();

    let text = json!({ "symbols": result, "count": result.len() }).to_string();
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

pub async fn tour_impl(
    graph: &Arc<GraphStore>,
    params: TourParams,
) -> Result<CallToolResult, McpError> {
    let TourParams { project, limit } = params;
    let limit = limit.unwrap_or(20).max(1).min(100);

    let stops: Vec<TourStop> = graph
        .generate_tour(&project, limit)
        .await
        .map_err(|e| McpError::internal_error(format!("generate_tour failed: {e}"), None))?;

    if stops.is_empty() {
        let text = json!({
            "project": project,
            "total_files": 0,
            "tour_stops": 0,
            "tour": [],
            "message": "No files found for this project. Has it been indexed with remembrall_index?",
        })
        .to_string();
        return Ok(CallToolResult::success(vec![Content::text(text)]));
    }

    let total_files = stops.last().map(|s| s.order).unwrap_or(0);

    let tour: Vec<serde_json::Value> = stops
        .iter()
        .map(|stop| {
            json!({
                "order": stop.order,
                "file": stop.file_path,
                "language": stop.language,
                "symbols": stop.symbols,
                "imports_from": stop.imports_from,
                "imported_by": stop.imported_by,
                "reason": stop.reason,
            })
        })
        .collect();

    let text = json!({
        "project": project,
        "total_files": total_files,
        "tour_stops": tour.len(),
        "tour": tour,
    })
    .to_string();

    Ok(CallToolResult::success(vec![Content::text(text)]))
}
