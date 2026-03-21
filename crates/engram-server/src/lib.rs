//! Engram MCP server - exposes memory and graph tools over the Model Context Protocol.

use std::sync::Arc;

use walkdir::WalkDir;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

use engram_core::{
    embed::{Embedder, FastEmbedder},
    graph::{
        store::GraphStore,
        types::{Direction, SymbolType},
    },
    memory::{
        store::MemoryStore,
        types::{CreateMemory, MatchType, MemoryQuery, MemoryType, Scope, Source},
    },
    parser::index_directory,
};

// Similarity threshold above which we flag a near-duplicate as a potential contradiction.
const CONTRADICTION_THRESHOLD: f64 = 0.75;

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StoreParams {
    #[schemars(description = "The knowledge to store. Be specific - include the why, not just the what.")]
    pub content: String,
    #[schemars(description = "One of: decision, pattern, error_pattern, preference, outcome, code_context, guideline, incident, architecture")]
    pub memory_type: String,
    #[schemars(description = "One-line summary")]
    pub summary: Option<String>,
    #[schemars(description = "Categorization tags")]
    pub tags: Option<Vec<String>>,
    #[schemars(description = "0.0 to 1.0, default 0.5. Use 0.8+ for architectural decisions.")]
    pub importance: Option<f32>,
    #[schemars(description = "Where this came from (PR URL, file path, etc.)")]
    pub source_identifier: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ImpactParams {
    #[schemars(description = "Name of the function, class, or method to analyze")]
    pub symbol_name: String,
    #[schemars(description = "One of: function, class, method, file")]
    pub symbol_type: Option<String>,
    #[schemars(description = "Filter to a specific project (as used in engram_index)")]
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
    #[schemars(description = "Filter to a specific project (as used in engram_index)")]
    pub project: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexParams {
    #[schemars(description = "Absolute path to the project root directory")]
    pub path: String,
    #[schemars(description = "Logical project name for namespacing")]
    pub project: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteParams {
    #[schemars(description = "UUID of the memory to delete")]
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateParams {
    #[schemars(description = "UUID of the memory to update")]
    pub id: String,
    #[schemars(description = "New content (replaces existing). If provided, a new embedding will be generated.")]
    pub content: Option<String>,
    #[schemars(description = "New summary")]
    pub summary: Option<String>,
    #[schemars(description = "New tags (replaces existing)")]
    pub tags: Option<Vec<String>>,
    #[schemars(description = "New importance (0.0 to 1.0)")]
    pub importance: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IngestGithubParams {
    #[schemars(description = "GitHub repo in owner/repo format (e.g. 'roboticforce/engram')")]
    pub repo: String,
    #[schemars(description = "Maximum number of recent merged PRs to ingest (default 50, max 200)")]
    pub limit: Option<u32>,
    #[schemars(description = "Project name to tag memories with (defaults to the repo name)")]
    pub project: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IngestDocsParams {
    #[schemars(description = "Path to project directory to scan for markdown files")]
    pub path: String,
    #[schemars(description = "Project name to tag memories with")]
    pub project: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecallParams {
    #[schemars(description = "What to search for. Can be a question ('how does auth work?') or keywords ('DATABASE_URL timeout').")]
    pub query: String,
    #[schemars(description = "Maximum number of results (default 10, max 25)")]
    pub limit: Option<i64>,
    #[schemars(description = "Filter by type: decision, pattern, error_pattern, preference, outcome, code_context, guideline, incident, architecture. Comma-separated for multiple.")]
    pub memory_types: Option<String>,
    #[schemars(description = "Filter by tags (comma-separated). Returns memories matching ALL supplied tags.")]
    pub tags: Option<String>,
    #[schemars(description = "Filter to memories about a specific project")]
    pub project: Option<String>,
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct EngramServer {
    memory: Arc<MemoryStore>,
    graph: Arc<GraphStore>,
    embedder: Arc<dyn Embedder>,
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl EngramServer {
    #[tool(description = "Store knowledge or a decision for future reference. Use this whenever you learn something important about a codebase, make an architectural decision, observe a pattern, or encounter an error worth remembering.")]
    async fn engram_store(
        &self,
        Parameters(StoreParams {
            content,
            memory_type,
            summary,
            tags,
            importance,
            source_identifier,
        }): Parameters<StoreParams>,
    ) -> Result<CallToolResult, McpError> {
        // Parse memory type.
        let mtype: MemoryType = memory_type.parse().map_err(|e: String| {
            McpError::invalid_params(
                format!("invalid memory_type: {e}"),
                None,
            )
        })?;

        // Generate embedding via spawn_blocking (fastembed is sync/CPU-bound).
        let embedder = Arc::clone(&self.embedder);
        let content_clone = content.clone();
        let embedding = tokio::task::spawn_blocking(move || embedder.embed(&content_clone))
            .await
            .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
            .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;

        // Check for near-duplicate memories before storing (contradiction detection).
        // We search at a high similarity threshold so only genuine semantic overlaps surface.
        // Uses get_readonly so contradiction checks don't inflate access counts.
        let near_dupes = self
            .memory
            .search_semantic(embedding.clone(), 3, CONTRADICTION_THRESHOLD, None)
            .await
            .unwrap_or_default();

        let mut contradiction_list: Vec<serde_json::Value> = Vec::new();
        for (dupe_id, similarity) in near_dupes.iter().take(3) {
            if let Ok(mem) = self.memory.get_readonly(*dupe_id).await {
                contradiction_list.push(json!({
                    "id": mem.id.to_string(),
                    "similarity": (similarity * 100.0).round() / 100.0,
                    "content": mem.content,
                    "memory_type": mem.memory_type.to_string(),
                }));
            }
        }

        let identifier = source_identifier.unwrap_or_else(|| "mcp".to_string());

        // Capture length before `content` is moved into `CreateMemory`.
        let content_len = content.len();

        let input = CreateMemory {
            content,
            summary,
            memory_type: mtype,
            source: Source {
                system: "mcp".to_string(),
                identifier,
                author: None,
            },
            scope: Scope {
                organization: None,
                team: None,
                project: None,
            },
            tags: tags.unwrap_or_default(),
            metadata: None,
            importance,
            expires_at: None,
        };

        let id = self
            .memory
            .store(input, embedding)
            .await
            .map_err(|e| McpError::internal_error(format!("store failed: {e}"), None))?;

        let mut response = json!({ "id": id.to_string(), "stored": true });
        if !contradiction_list.is_empty() {
            response["contradictions"] = json!(contradiction_list);
        }
        // Inform the caller when content is large enough that the embedding only
        // covers the first ~500 words (model token limit). We still store the full
        // text; only semantic search fidelity is affected.
        if content_len > 2000 {
            response["note"] = json!(
                "Content exceeds 2000 characters. The semantic search embedding represents only the first ~500 words."
            );
        }

        let text = response.to_string();
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Analyze the blast radius of changing a code symbol. Returns all callers (upstream) or all callees (downstream) up to max_depth levels. Use this before refactoring to understand what will break.")]
    async fn engram_impact(
        &self,
        Parameters(ImpactParams {
            symbol_name,
            symbol_type,
            project,
            direction,
            max_depth,
        }): Parameters<ImpactParams>,
    ) -> Result<CallToolResult, McpError> {
        // Parse optional symbol type filter.
        let stype: Option<SymbolType> = symbol_type
            .as_deref()
            .map(|s| {
                s.parse::<SymbolType>().map_err(|e: String| {
                    McpError::invalid_params(format!("invalid symbol_type: {e}"), None)
                })
            })
            .transpose()?;

        // Find the symbol.
        let symbols = self
            .graph
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

        // Use the first match.
        let symbol = &symbols[0];

        // Parse direction.
        let dir = match direction.as_deref().unwrap_or("upstream") {
            "downstream" => Direction::Downstream,
            "both" => Direction::Both,
            _ => Direction::Upstream,
        };

        let depth = max_depth.unwrap_or(3).min(10).max(1);

        let results = self
            .graph
            .impact_analysis(symbol.id, dir, depth)
            .await
            .map_err(|e| McpError::internal_error(format!("impact_analysis failed: {e}"), None))?;

        // Collect unique affected files.
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
                    "depth": r.depth,
                    "relationship": r.relationship.to_string(),
                    "confidence": r.confidence,
                })
            })
            .collect();

        let text = json!({
            "symbol": symbol.name,
            "file": symbol.file_path,
            "direction": format!("{:?}", dir).to_lowercase(),
            "affected_symbols": affected,
            "affected_files": files,
            "total_symbols": affected.len(),
            "total_files": files.len(),
        })
        .to_string();

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Look up a code symbol by name. Returns its file location, type, and line numbers. Use this to find where a function or class is defined.")]
    async fn engram_lookup_symbol(
        &self,
        Parameters(LookupParams { name, symbol_type, project }): Parameters<LookupParams>,
    ) -> Result<CallToolResult, McpError> {
        let stype: Option<SymbolType> = symbol_type
            .as_deref()
            .map(|s| {
                s.parse::<SymbolType>().map_err(|e: String| {
                    McpError::invalid_params(format!("invalid symbol_type: {e}"), None)
                })
            })
            .transpose()?;

        let symbols = self
            .graph
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

    #[tool(description = "Index a project directory to build the code graph. Must be run before impact analysis or symbol lookup. Supports Python, TypeScript, and JavaScript files.")]
    async fn engram_index(
        &self,
        Parameters(IndexParams { path, project }): Parameters<IndexParams>,
    ) -> Result<CallToolResult, McpError> {
        let graph = Arc::clone(&self.graph);
        let path_clone = path.clone();
        let project_clone = project.clone();

        // index_directory is sync and CPU-bound; run in a blocking thread.
        let index_result = tokio::task::spawn_blocking(move || {
            index_directory(&path_clone, &project_clone, None)
        })
        .await
        .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
        .map_err(|e| McpError::internal_error(format!("index_directory failed: {e}"), None))?;

        // Store symbols and relationships in graph.
        let mut symbols_stored = 0u64;
        let mut relationships_stored = 0u64;

        for symbol in &index_result.symbols {
            graph
                .upsert_symbol(symbol)
                .await
                .map_err(|e| McpError::internal_error(format!("upsert_symbol failed: {e}"), None))?;
            symbols_stored += 1;
        }

        for rel in &index_result.relationships {
            // Skip relationships whose target doesn't exist in the graph
            // (unresolved imports to third-party or stdlib).
            if let Err(e) = graph.add_relationship(rel).await {
                tracing::debug!("skipping unresolvable relationship: {e}");
            } else {
                relationships_stored += 1;
            }
        }

        let text = json!({
            "path": path,
            "project": project,
            "files_parsed": index_result.files_parsed,
            "files_skipped": index_result.files_skipped,
            "symbols_stored": symbols_stored,
            "relationships_stored": relationships_stored,
            "note": "Indexing complete. Impact analysis results will reflect the updated graph.",
        })
        .to_string();

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Delete a stored memory by its UUID. Use this to remove outdated or incorrect knowledge.")]
    async fn engram_delete(
        &self,
        Parameters(DeleteParams { id }): Parameters<DeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        let uid: uuid::Uuid = id.parse().map_err(|e| {
            McpError::invalid_params(format!("invalid UUID: {e}"), None)
        })?;

        let deleted = self
            .memory
            .delete(uid)
            .await
            .map_err(|e| McpError::internal_error(format!("delete failed: {e}"), None))?;

        let text = json!({ "id": id, "deleted": deleted }).to_string();
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Update an existing memory. Only the fields you provide will be changed. If content is updated, a new embedding is generated automatically.")]
    async fn engram_update(
        &self,
        Parameters(UpdateParams {
            id,
            content,
            summary,
            tags,
            importance,
        }): Parameters<UpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let uid: uuid::Uuid = id.parse().map_err(|e| {
            McpError::invalid_params(format!("invalid UUID: {e}"), None)
        })?;

        // If content is provided, generate a new embedding.
        let embedding: Option<Vec<f32>> = if let Some(ref c) = content {
            let embedder = Arc::clone(&self.embedder);
            let c_clone = c.clone();
            let emb = tokio::task::spawn_blocking(move || embedder.embed(&c_clone))
                .await
                .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
                .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;
            Some(emb)
        } else {
            None
        };

        let updated = self
            .memory
            .update(uid, content, summary, tags, importance, embedding)
            .await
            .map_err(|e| McpError::internal_error(format!("update failed: {e}"), None))?;

        let text = if updated {
            json!({ "id": id, "updated": true }).to_string()
        } else {
            json!({ "id": id, "updated": false, "reason": "not found" }).to_string()
        };

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Ingest merged pull request descriptions from a GitHub repository as memories. Solves the cold-start problem by bulk-importing architectural decisions, rationale, and context from your PR history. Requires the GitHub CLI (gh) to be installed and authenticated.")]
    async fn engram_ingest_github(
        &self,
        Parameters(IngestGithubParams { repo, limit, project }): Parameters<IngestGithubParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = limit.unwrap_or(50).min(200);
        let project = project.unwrap_or_else(|| {
            repo.split('/').last().unwrap_or("unknown").to_string()
        });

        // Shell out to gh CLI - already authenticated on the user's machine, no token needed.
        let output = tokio::process::Command::new("gh")
            .args([
                "pr", "list",
                "--repo", &repo,
                "--state", "merged",
                "--limit", &limit.to_string(),
                "--json", "number,title,body,mergedAt,author,url",
            ])
            .output()
            .await
            .map_err(|e| McpError::internal_error(
                format!("Failed to run gh CLI. Is GitHub CLI installed and on PATH? Error: {e}"),
                None,
            ))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(McpError::internal_error(
                format!("gh CLI failed: {stderr}"),
                None,
            ));
        }

        let prs: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)
            .map_err(|e| McpError::internal_error(format!("Failed to parse gh output: {e}"), None))?;

        let mut ingested = 0u32;
        let mut skipped = 0u32;
        let mut errors = 0u32;

        for pr in &prs {
            let title = pr["title"].as_str().unwrap_or("");
            let body = pr["body"].as_str().unwrap_or("");
            let number = pr["number"].as_u64().unwrap_or(0);
            let url = pr["url"].as_str().unwrap_or("");
            let author = pr["author"]
                .as_object()
                .and_then(|a| a["login"].as_str())
                .unwrap_or("unknown");

            // Skip PRs with empty or very short bodies - no meaningful content to index.
            if body.trim().len() < 50 {
                skipped += 1;
                continue;
            }

            let content = format!("PR #{number}: {title}\n\n{body}");

            // Dedup by content fingerprint before touching the embedder.
            let fingerprint = engram_core::memory::store::compute_fingerprint_pub(&content);
            match self.memory.find_by_fingerprint(&fingerprint).await {
                Ok(Some(_)) => {
                    skipped += 1;
                    continue;
                }
                Err(e) => {
                    tracing::warn!("fingerprint check failed for PR #{number}: {e}");
                    errors += 1;
                    continue;
                }
                Ok(None) => {}
            }

            // Generate embedding (fastembed is sync/CPU-bound).
            let embedder = Arc::clone(&self.embedder);
            let content_clone = content.clone();
            let embedding = match tokio::task::spawn_blocking(move || embedder.embed(&content_clone)).await {
                Ok(Ok(emb)) => emb,
                Ok(Err(e)) => {
                    tracing::warn!("embedding failed for PR #{number}: {e}");
                    errors += 1;
                    continue;
                }
                Err(e) => {
                    tracing::warn!("spawn_blocking panicked for PR #{number}: {e}");
                    errors += 1;
                    continue;
                }
            };

            // Classify memory type by title keywords.
            let title_lower = title.to_lowercase();
            let memory_type = if title_lower.contains("fix") || title_lower.contains("bug") {
                MemoryType::ErrorPattern
            } else if title_lower.contains("refactor") {
                MemoryType::Pattern
            } else {
                MemoryType::Decision
            };

            let input = CreateMemory {
                content,
                summary: Some(format!("PR #{number}: {title}")),
                memory_type,
                source: Source {
                    system: "github".to_string(),
                    identifier: url.to_string(),
                    author: Some(author.to_string()),
                },
                scope: Scope {
                    organization: None,
                    team: None,
                    project: Some(project.clone()),
                },
                tags: vec!["github".to_string(), "pull-request".to_string()],
                metadata: None,
                importance: Some(0.6),
                expires_at: None,
            };

            match self.memory.store(input, embedding).await {
                Ok(_) => ingested += 1,
                Err(e) => {
                    tracing::warn!("store failed for PR #{number}: {e}");
                    errors += 1;
                }
            }
        }

        let text = json!({
            "repo": repo,
            "project": project,
            "total_prs": prs.len(),
            "ingested": ingested,
            "skipped": skipped,
            "errors": errors,
        })
        .to_string();

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Search organizational memory for relevant knowledge - decisions, patterns, errors, and context from past sessions. Use this before making significant decisions or when you need context about how something works or why it was built a certain way.")]
    async fn engram_recall(
        &self,
        Parameters(RecallParams { query, limit, memory_types, tags, project }): Parameters<RecallParams>,
    ) -> Result<CallToolResult, McpError> {
        // Guard: reject empty or whitespace-only queries before touching the DB or
        // embedding model. `plainto_tsquery('')` fails in Postgres, and an empty
        // string produces a meaningless embedding.
        if query.trim().is_empty() {
            let text = json!({
                "query": "",
                "total_results": 0,
                "results": [],
                "suggestion": "Please provide a search query."
            })
            .to_string();
            return Ok(CallToolResult::success(vec![Content::text(text)]));
        }

        let limit = limit.unwrap_or(10).min(25).max(1);

        // Parse comma-separated memory type filter.
        let type_filter: Option<Vec<MemoryType>> = memory_types
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().parse::<MemoryType>())
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|e: String| McpError::invalid_params(format!("invalid memory_type: {e}"), None))?;

        // Parse comma-separated tag filter.
        let tag_filter: Option<Vec<String>> = tags.map(|s| {
            s.split(',').map(|t| t.trim().to_string()).collect()
        });

        // Build scope from project filter if provided.
        let scope = project.map(|p| Scope {
            organization: None,
            team: None,
            project: Some(p),
        });

        // Embed the query.
        let embedder = Arc::clone(&self.embedder);
        let query_clone = query.clone();
        let embedding = tokio::task::spawn_blocking(move || embedder.embed(&query_clone))
            .await
            .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
            .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;

        let mq = MemoryQuery {
            query: query.clone(),
            memory_types: type_filter,
            scope,
            tags: tag_filter,
            limit: Some(limit),
            min_similarity: None,
        };

        let results = self
            .memory
            .search_hybrid(embedding, &mq)
            .await
            .map_err(|e| McpError::internal_error(format!("recall failed: {e}"), None))?;

        let result_json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                let content = if r.memory.content.len() > 2000 {
                    format!("{}...[truncated]", &r.memory.content[..2000])
                } else {
                    r.memory.content.clone()
                };

                let match_type = match r.match_type {
                    MatchType::Semantic => "semantic",
                    MatchType::FullText => "fulltext",
                    MatchType::Hybrid => "hybrid",
                };

                json!({
                    "id": r.memory.id.to_string(),
                    "score": r.score,
                    "match_type": match_type,
                    "memory_type": r.memory.memory_type.to_string(),
                    "content": content,
                    "summary": r.memory.summary,
                    "tags": r.memory.tags,
                    "importance": r.memory.importance,
                    "source": r.memory.source.identifier,
                    "created_at": r.memory.created_at.to_rfc3339(),
                    "access_count": r.memory.access_count,
                })
            })
            .collect();

        let suggestion = if result_json.is_empty() {
            Some("No memories found. Store context with engram_store as you work to build up searchable knowledge.")
        } else {
            None
        };

        let text = json!({
            "query": query,
            "total_results": result_json.len(),
            "results": result_json,
            "suggestion": suggestion,
        })
        .to_string();

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Ingest markdown files from a project directory as memories. Walks the directory tree, splits files by H2 section headers, and stores each section as a searchable memory. Solves the cold-start problem - run this once per project to immediately populate Engram with knowledge from README, ARCHITECTURE, ADRs, and docs.")]
    async fn engram_ingest_docs(
        &self,
        Parameters(IngestDocsParams { path, project }): Parameters<IngestDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        // Derive project name from directory basename when not supplied.
        let project_name = project.unwrap_or_else(|| {
            std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        // Directories to skip - these are never meaningful documentation sources.
        const SKIP_DIRS: &[&str] = &[
            "node_modules", ".git", "vendor", "target", ".venv", "__pycache__",
            ".tox", "dist", "build", ".cache", ".next", ".nuxt",
        ];

        // Collect markdown file paths without following symlinks.
        let md_paths: Vec<std::path::PathBuf> = WalkDir::new(&path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Skip hidden directories and known noise directories.
                let name = e.file_name().to_string_lossy();
                if e.file_type().is_dir() {
                    if name.starts_with('.') {
                        return false;
                    }
                    if SKIP_DIRS.iter().any(|&d| d == name.as_ref()) {
                        return false;
                    }
                }
                true
            })
            .filter_map(|entry| entry.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .map(|ext| ext.eq_ignore_ascii_case("md"))
                        .unwrap_or(false)
            })
            .map(|e| e.into_path())
            .collect();

        let files_scanned = md_paths.len();
        let mut sections_ingested: u32 = 0;
        let mut skipped: u32 = 0;
        let mut errors: u32 = 0;

        for file_path in &md_paths {
            // Read file, skip gracefully on UTF-8 or I/O errors.
            let raw = match std::fs::read(file_path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!("skipping {}: read error: {e}", file_path.display());
                    errors += 1;
                    continue;
                }
            };
            let content = match std::str::from_utf8(&raw) {
                Ok(s) => s.to_string(),
                Err(_) => {
                    tracing::debug!("skipping {} (not valid UTF-8)", file_path.display());
                    skipped += 1;
                    continue;
                }
            };

            // Derive a short display name relative to the scanned root.
            let display_name = file_path
                .strip_prefix(&path)
                .unwrap_or(file_path)
                .display()
                .to_string();

            let file_stem = file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            // Classify memory type by filename convention.
            let memory_type = classify_memory_type(&file_stem);

            // Importance: architecture docs and ADRs are higher value.
            let importance = match memory_type {
                MemoryType::Architecture => 0.8,
                MemoryType::Guideline => 0.7,
                _ => 0.6,
            };

            let sections = split_markdown_sections(&content, &display_name);

            for (summary, section_content) in sections {
                // Skip sections that are too short to be meaningful.
                if section_content.trim().len() < 200 {
                    skipped += 1;
                    continue;
                }

                // Dedup by content fingerprint.
                let fingerprint =
                    engram_core::memory::store::compute_fingerprint_pub(&section_content);
                match self.memory.find_by_fingerprint(&fingerprint).await {
                    Ok(Some(_)) => {
                        skipped += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("fingerprint check failed for {display_name}: {e}");
                        errors += 1;
                        continue;
                    }
                    Ok(None) => {}
                }

                // Generate embedding (fastembed is sync/CPU-bound).
                let embedder = Arc::clone(&self.embedder);
                let content_for_embed = section_content.clone();
                let embedding = match tokio::task::spawn_blocking(move || {
                    embedder.embed(&content_for_embed)
                })
                .await
                {
                    Ok(Ok(emb)) => emb,
                    Ok(Err(e)) => {
                        tracing::warn!("embedding failed for {display_name}: {e}");
                        errors += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("spawn_blocking panicked for {display_name}: {e}");
                        errors += 1;
                        continue;
                    }
                };

                let tags = vec![
                    "docs".to_string(),
                    "markdown".to_string(),
                    file_stem.to_lowercase().replace([' ', '/'], "-"),
                ];

                let input = CreateMemory {
                    content: section_content,
                    summary: Some(summary),
                    memory_type: memory_type.clone(),
                    source: Source {
                        system: "ingest_docs".to_string(),
                        identifier: file_path.display().to_string(),
                        author: None,
                    },
                    scope: Scope {
                        organization: None,
                        team: None,
                        project: Some(project_name.clone()),
                    },
                    tags,
                    metadata: None,
                    importance: Some(importance),
                    expires_at: None,
                };

                match self.memory.store(input, embedding).await {
                    Ok(_) => sections_ingested += 1,
                    Err(e) => {
                        tracing::warn!("store failed for {display_name}: {e}");
                        errors += 1;
                    }
                }
            }
        }

        let text = json!({
            "path": path,
            "project": project_name,
            "files_scanned": files_scanned,
            "sections_ingested": sections_ingested,
            "skipped": skipped,
            "errors": errors,
        })
        .to_string();

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for EngramServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Engram is a persistent knowledge memory layer for AI agents. \
                 Use engram_recall to search for past decisions, patterns, errors, and knowledge before starting work. \
                 Use engram_store to save decisions, patterns, and context. \
                 Use engram_update to edit an existing memory (content, summary, tags, or importance) without deleting and re-creating it. \
                 Use engram_delete to remove stale memories. \
                 Use engram_ingest_github to bulk-import merged PR descriptions from a GitHub repo - run this once per project to solve the cold-start problem. \
                 Use engram_ingest_docs to scan a project directory for markdown files and ingest them as memories - run this once per project to immediately populate Engram from README, ARCHITECTURE, ADRs, and docs. \
                 Use engram_index to build a code graph from a project directory. \
                 Use engram_impact to analyze what code would break if you change a symbol. \
                 Use engram_lookup_symbol to find where a function or class is defined."
                    .to_string(),
            )
    }
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

impl EngramServer {
    /// Build from explicit parameters.
    pub async fn from_config(database_url: &str, schema: &str) -> anyhow::Result<Self> {
        // Strip credentials from the URL before logging so passwords don't leak.
        let safe_url = database_url.split('@').last().unwrap_or("<url>");
        tracing::info!("Connecting to database: {safe_url}");

        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(database_url)
            .await
            .map_err(|e| anyhow::anyhow!(
                "Cannot connect to database at {}. Is Postgres running? \
                 Try 'engram doctor' to diagnose.\nError: {}",
                safe_url,
                e
            ))?;

        let memory = Arc::new(MemoryStore::new(pool.clone(), schema.to_string()));
        let graph = Arc::new(GraphStore::new(pool.clone(), schema.to_string()));

        tracing::info!("Initializing stores...");
        memory.init().await?;
        graph.init().await?;

        tracing::info!("Loading embedding model...");
        let embedder: Arc<dyn Embedder> = Arc::new(FastEmbedder::new()?);

        Ok(Self {
            memory,
            graph,
            embedder,
            tool_router: Self::tool_router(),
        })
    }

    /// Build from environment. Reads DATABASE_URL / ENGRAM_DATABASE_URL.
    /// Delegates to from_config.
    pub async fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("ENGRAM_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| {
                "postgres://postgres:postgres@localhost:5450/engram".to_string()
            });

        let schema = std::env::var("ENGRAM_SCHEMA")
            .unwrap_or_else(|_| "engram".to_string());

        Self::from_config(&database_url, &schema).await
    }
}

// ---------------------------------------------------------------------------
// Markdown ingestion helpers
// ---------------------------------------------------------------------------

/// Split a markdown document into sections on `## ` (H2) boundaries.
///
/// Each section becomes `(summary, content)` where summary is
/// `"filename: Section Title"` and content is the section text including
/// the header line. If the file has no H2 headers the whole file is returned
/// as a single section with summary `"filename"`.
fn split_markdown_sections(content: &str, file_name: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in content.lines() {
        if line.starts_with("## ") {
            // Flush current accumulator.
            if !current_lines.is_empty() {
                let body = current_lines.join("\n");
                let summary = match &current_header {
                    Some(h) => format!("{file_name}: {h}"),
                    None => file_name.to_string(),
                };
                sections.push((summary, body));
                current_lines.clear();
            }
            // Start a new section.
            let title = line.trim_start_matches('#').trim().to_string();
            current_header = Some(title);
        }
        current_lines.push(line);
    }

    // Flush the final accumulator.
    if !current_lines.is_empty() {
        let body = current_lines.join("\n");
        let summary = match &current_header {
            Some(h) => format!("{file_name}: {h}"),
            None => file_name.to_string(),
        };
        sections.push((summary, body));
    }

    // If there were no H2 headers we get a single section with no header prefix;
    // that is correct - the summary is just the filename.
    sections
}

/// Map filename stem to a `MemoryType`.
///
/// Rules (case-insensitive):
/// - `ARCHITECTURE`, `DESIGN`, files ending in `-adr` or `-decision` -> Architecture
/// - `CONTRIBUTING`, `STYLE`, `CODE_OF_CONDUCT`, files containing `guideline` -> Guideline
/// - Everything else -> CodeContext
fn classify_memory_type(stem: &str) -> MemoryType {
    let lower = stem.to_lowercase();
    if lower.contains("architecture")
        || lower.contains("design")
        || lower.ends_with("-adr")
        || lower.ends_with("-decision")
        || lower.starts_with("adr-")
        || lower.starts_with("adr_")
    {
        return MemoryType::Architecture;
    }
    if lower.contains("contributing")
        || lower.contains("guideline")
        || lower.contains("style")
        || lower.contains("code_of_conduct")
        || lower.contains("conduct")
    {
        return MemoryType::Guideline;
    }
    MemoryType::CodeContext
}
