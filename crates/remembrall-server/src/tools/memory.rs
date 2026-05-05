//! Memory tool parameter structs and implementation helpers.
//!
//! The `#[tool]` wrapper methods live in `lib.rs` (required by `#[tool_router]`).
//! This module holds the param structs and the actual logic so each tool's
//! implementation can be read and modified in isolation.

use std::sync::Arc;

use rmcp::{ErrorData as McpError, model::*, schemars};
use serde_json::json;

use remembrall_core::{
    embed::Embedder,
    memory::{
        store::MemoryStore,
        types::{CreateMemory, MatchType, MemoryQuery, MemoryType, Scope, Source},
    },
};

// Similarity threshold above which we flag a near-duplicate as a potential
// contradiction. Defined here so the constant is co-located with the logic
// that uses it.
pub(crate) const CONTRADICTION_THRESHOLD: f64 = 0.75;

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
    #[schemars(description = "Tenant/organization scope for multi-tenant isolation. When set, only that tenant can retrieve this memory.")]
    pub organization: Option<String>,
    #[schemars(description = "Optional sub-project scope within the organization.")]
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
    #[schemars(description = "Tenant/organization scope. When set, only memories stored under this organization are returned.")]
    pub organization: Option<String>,
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
pub struct DeleteParams {
    #[schemars(description = "UUID of the memory to delete")]
    pub id: String,
}

// ---------------------------------------------------------------------------
// Logic helpers
// ---------------------------------------------------------------------------

pub async fn store_impl(
    memory: &Arc<MemoryStore>,
    embedder: &Arc<dyn Embedder>,
    params: StoreParams,
) -> Result<CallToolResult, McpError> {
    let StoreParams { content, memory_type, summary, tags, importance, source_identifier, organization, project } = params;

    let mtype: MemoryType = memory_type.parse().map_err(|e: String| {
        McpError::invalid_params(format!("invalid memory_type: {e}"), None)
    })?;

    let embedder = Arc::clone(embedder);
    let content_clone = content.clone();
    let embedding = tokio::task::spawn_blocking(move || embedder.embed(&content_clone))
        .await
        .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
        .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;

    let near_dupes = memory
        .search_semantic(embedding.clone(), 3, CONTRADICTION_THRESHOLD, None)
        .await
        .unwrap_or_default();

    let mut contradiction_list: Vec<serde_json::Value> = Vec::new();
    for (dupe_id, similarity) in near_dupes.iter().take(3) {
        if let Ok(mem) = memory.get_readonly(*dupe_id).await {
            contradiction_list.push(json!({
                "id": mem.id.to_string(),
                "similarity": (similarity * 100.0).round() / 100.0,
                "content": mem.content,
                "memory_type": mem.memory_type.to_string(),
            }));
        }
    }

    let identifier = source_identifier.unwrap_or_else(|| "mcp".to_string());
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
            organization,
            team: None,
            project,
        },
        tags: tags.unwrap_or_default(),
        metadata: None,
        importance,
        expires_at: None,
    };

    let id = memory
        .store(input, embedding)
        .await
        .map_err(|e| McpError::internal_error(format!("store failed: {e}"), None))?;

    let mut response = json!({ "id": id.to_string(), "stored": true });
    if !contradiction_list.is_empty() {
        response["contradictions"] = json!(contradiction_list);
    }
    if content_len > 2000 {
        response["note"] = json!(
            "Content exceeds 2000 characters. The semantic search embedding represents only the first ~500 words."
        );
    }

    Ok(CallToolResult::success(vec![Content::text(response.to_string())]))
}

pub async fn recall_impl(
    memory: &Arc<MemoryStore>,
    embedder: &Arc<dyn Embedder>,
    params: RecallParams,
) -> Result<CallToolResult, McpError> {
    let RecallParams { query, limit, memory_types, tags, project, organization } = params;

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

    let type_filter: Option<Vec<MemoryType>> = memory_types
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().parse::<MemoryType>())
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(|e: String| McpError::invalid_params(format!("invalid memory_type: {e}"), None))?;

    let tag_filter: Option<Vec<String>> = tags.map(|s| {
        s.split(',').map(|t| t.trim().to_string()).collect()
    });

    let scope = if organization.is_some() || project.is_some() {
        Some(Scope { organization, team: None, project })
    } else {
        None
    };

    let embedder = Arc::clone(embedder);
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

    let results = memory
        .search_hybrid(embedding, &mq)
        .await
        .map_err(|e| McpError::internal_error(format!("recall failed: {e}"), None))?;

    let result_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let content = if r.memory.content.len() > 2000 {
                let mut end = 2000;
                while !r.memory.content.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...[truncated]", &r.memory.content[..end])
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
        Some("No memories found. Store context with remembrall_store as you work to build up searchable knowledge.")
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

pub async fn update_impl(
    memory: &Arc<MemoryStore>,
    embedder: &Arc<dyn Embedder>,
    params: UpdateParams,
) -> Result<CallToolResult, McpError> {
    let UpdateParams { id, content, summary, tags, importance } = params;

    let uid: uuid::Uuid = id.parse().map_err(|e| {
        McpError::invalid_params(format!("invalid UUID: {e}"), None)
    })?;

    let embedding: Option<Vec<f32>> = if let Some(ref c) = content {
        let embedder = Arc::clone(embedder);
        let c_clone = c.clone();
        let emb = tokio::task::spawn_blocking(move || embedder.embed(&c_clone))
            .await
            .map_err(|e| McpError::internal_error(format!("spawn_blocking failed: {e}"), None))?
            .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;
        Some(emb)
    } else {
        None
    };

    let updated = memory
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

pub async fn delete_impl(
    memory: &Arc<MemoryStore>,
    params: DeleteParams,
) -> Result<CallToolResult, McpError> {
    let DeleteParams { id } = params;

    let uid: uuid::Uuid = id.parse().map_err(|e| {
        McpError::invalid_params(format!("invalid UUID: {e}"), None)
    })?;

    let deleted = memory
        .delete(uid)
        .await
        .map_err(|e| McpError::internal_error(format!("delete failed: {e}"), None))?;

    let text = json!({ "id": id, "deleted": deleted }).to_string();
    Ok(CallToolResult::success(vec![Content::text(text)]))
}
