//! RemembrallMCP server - exposes memory and graph tools over the Model Context Protocol.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

pub mod tools;
pub mod watcher;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use sqlx::postgres::PgPoolOptions;

use remembrall_core::{
    embed::{Embedder, FastEmbedder},
    graph::store::GraphStore,
    memory::store::MemoryStore,
};

use tools::{
    graph::{ImpactParams, IndexParams, LookupParams, TourParams},
    ingest::{IngestDocsParams, IngestGithubParams},
    memory::{DeleteParams, RecallParams, StoreParams, UpdateParams},
};

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct RemembrallServer {
    memory: Arc<MemoryStore>,
    graph: Arc<GraphStore>,
    embedder: Arc<dyn Embedder>,
    tool_router: ToolRouter<Self>,
    /// Directories that have already had a background watcher spawned.
    /// Guarded by a mutex so concurrent `remembrall_index` calls don't race.
    watched_dirs: Arc<Mutex<HashSet<PathBuf>>>,
}

// ---------------------------------------------------------------------------
// Tool wrappers - thin delegates into the tools:: modules
// ---------------------------------------------------------------------------
//
// All `#[tool]` methods must live in this single `#[tool_router]` impl block
// because the proc-macro scans the block to build the router. Logic lives in
// the sub-modules; these methods are intentionally one-liners.

#[tool_router]
impl RemembrallServer {
    #[tool(description = "Store knowledge or a decision for future reference. Use this whenever you learn something important about a codebase, make an architectural decision, observe a pattern, or encounter an error worth remembering.")]
    async fn remembrall_store(
        &self,
        Parameters(params): Parameters<StoreParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::memory::store_impl(&self.memory, &self.embedder, params).await
    }

    #[tool(description = "Search organizational memory for relevant knowledge - decisions, patterns, errors, and context from past sessions. Use this before making significant decisions or when you need context about how something works or why it was built a certain way.")]
    async fn remembrall_recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::memory::recall_impl(&self.memory, &self.embedder, params).await
    }

    #[tool(description = "Update an existing memory. Only the fields you provide will be changed. If content is updated, a new embedding is generated automatically.")]
    async fn remembrall_update(
        &self,
        Parameters(params): Parameters<UpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::memory::update_impl(&self.memory, &self.embedder, params).await
    }

    #[tool(description = "Delete a stored memory by its UUID. Use this to remove outdated or incorrect knowledge.")]
    async fn remembrall_delete(
        &self,
        Parameters(params): Parameters<DeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::memory::delete_impl(&self.memory, params).await
    }

    #[tool(description = "Index a project directory to build the code graph. Must be run before impact analysis or symbol lookup. Supports Python, TypeScript, JavaScript, Rust, Go, Ruby, Java, and Kotlin.")]
    async fn remembrall_index(
        &self,
        Parameters(params): Parameters<IndexParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::graph::index_impl(&self.graph, &self.watched_dirs, params).await
    }

    #[tool(description = "Analyze the blast radius of changing a code symbol. Returns all callers (upstream) or all callees (downstream) up to max_depth levels. Use this before refactoring to understand what will break.")]
    async fn remembrall_impact(
        &self,
        Parameters(params): Parameters<ImpactParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::graph::impact_impl(&self.graph, params).await
    }

    #[tool(description = "Look up a code symbol by name. Returns its file location, type, and line numbers. Use this to find where a function or class is defined.")]
    async fn remembrall_lookup_symbol(
        &self,
        Parameters(params): Parameters<LookupParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::graph::lookup_symbol_impl(&self.graph, params).await
    }

    #[tool(description = "Generate a guided onboarding tour of an indexed codebase. Returns files in recommended reading order, starting from entry points and following the dependency graph. Use this to understand an unfamiliar project.")]
    async fn remembrall_tour(
        &self,
        Parameters(params): Parameters<TourParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::graph::tour_impl(&self.graph, params).await
    }

    #[tool(description = "Ingest merged pull request descriptions from a GitHub repository as memories. Solves the cold-start problem by bulk-importing architectural decisions, rationale, and context from your PR history. Requires the GitHub CLI (gh) to be installed and authenticated.")]
    async fn remembrall_ingest_github(
        &self,
        Parameters(params): Parameters<IngestGithubParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ingest::ingest_github_impl(&self.memory, &self.embedder, params).await
    }

    #[tool(description = "Ingest markdown files from a project directory as memories. Walks the directory tree, splits files by H2 section headers, and stores each section as a searchable memory. Solves the cold-start problem - run this once per project to immediately populate RemembrallMCP with knowledge from README, ARCHITECTURE, ADRs, and docs.")]
    async fn remembrall_ingest_docs(
        &self,
        Parameters(params): Parameters<IngestDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        tools::ingest::ingest_docs_impl(&self.memory, &self.embedder, params).await
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for RemembrallServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "RemembrallMCP is a persistent knowledge memory layer for AI agents. \
                 Use remembrall_recall to search for past decisions, patterns, errors, and knowledge before starting work. \
                 Use remembrall_store to save decisions, patterns, and context. \
                 Use remembrall_update to edit an existing memory (content, summary, tags, or importance) without deleting and re-creating it. \
                 Use remembrall_delete to remove stale memories. \
                 Use remembrall_ingest_github to bulk-import merged PR descriptions from a GitHub repo - run this once per project to solve the cold-start problem. \
                 Use remembrall_ingest_docs to scan a project directory for markdown files and ingest them as memories - run this once per project to immediately populate RemembrallMCP from README, ARCHITECTURE, ADRs, and docs. \
                 Use remembrall_index to build a code graph from a project directory. \
                 Use remembrall_tour to get a guided reading-order tour of an indexed project - start here when onboarding to an unfamiliar codebase. \
                 Use remembrall_impact to analyze what code would break if you change a symbol. \
                 Use remembrall_lookup_symbol to find where a function or class is defined."
                    .to_string(),
            )
    }
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

impl RemembrallServer {
    /// Build from explicit parameters.
    pub async fn from_config(database_url: &str, schema: &str) -> anyhow::Result<Self> {
        let safe_url = database_url.split('@').last().unwrap_or("<url>");
        tracing::info!("Connecting to database: {safe_url}");

        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(database_url)
            .await
            .map_err(|e| anyhow::anyhow!(
                "Cannot connect to database at {}. Is Postgres running? \
                 Try 'remembrall doctor' to diagnose.\nError: {}",
                safe_url,
                e
            ))?;

        let memory = Arc::new(MemoryStore::new(pool.clone(), schema.to_string())?);
        let graph = Arc::new(GraphStore::new(pool.clone(), schema.to_string())?);

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
            watched_dirs: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// Build from environment. Reads DATABASE_URL / REMEMBRALL_DATABASE_URL.
    /// Delegates to from_config.
    pub async fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("REMEMBRALL_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| {
                "postgres://postgres:postgres@localhost:5450/remembrall".to_string()
            });

        let schema = std::env::var("REMEMBRALL_SCHEMA")
            .unwrap_or_else(|_| "remembrall".to_string());

        Self::from_config(&database_url, &schema).await
    }
}
