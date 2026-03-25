//! Spike 1: Validate Engram core against real Postgres + pgvector.
//! Run: cargo run --bin spike

use engram_core::config::Config;
use engram_core::graph::store::GraphStore;
use engram_core::graph::types::*;
use engram_core::memory::store::MemoryStore;
use engram_core::memory::types::*;

use chrono::Utc;
use sqlx::postgres::PgPoolOptions;
use std::time::Instant;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("engram=debug,info")
        .init();

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or("postgres://postgres@localhost:5450/engram".into());

    println!("=== Engram Spike 1: Core Validation ===\n");
    println!("Connecting to {database_url}...");

    let pool = PgPoolOptions::new()
        .max_connections(25)
        .connect(&database_url)
        .await?;

    let schema = "engram".to_string();
    let memory_store = MemoryStore::new(pool.clone(), schema.clone())?;
    let graph_store = GraphStore::new(pool.clone(), schema.clone())?;

    // --- Initialize ---
    println!("\n--- Initializing schema ---");
    let t = Instant::now();
    memory_store.init().await?;
    graph_store.init().await?;
    println!("Schema initialized in {:?}", t.elapsed());

    // --- Memory Store Tests ---
    println!("\n--- Memory Store ---");

    // Store some memories with fake embeddings (384-dim)
    let fake_embedding = vec![0.1_f32; 384];

    let memories = vec![
        CreateMemory {
            content: "We chose Postgres over MySQL for the billing service because of better JSON support and pgvector for future AI features.".into(),
            summary: Some("Postgres chosen for billing service".into()),
            memory_type: MemoryType::Decision,
            source: Source { system: "github".into(), identifier: "PR #847".into(), author: Some("steve".into()) },
            scope: Scope { organization: Some("roboticforce".into()), team: Some("backend".into()), project: Some("billing".into()) },
            tags: vec!["database".into(), "postgres".into(), "billing".into()],
            metadata: None,
            importance: Some(0.8),
            expires_at: None,
        },
        CreateMemory {
            content: "Never use Redis for session persistence until Q3 - it's not configured for durability yet. Use Postgres session store.".into(),
            summary: Some("Redis not ready for sessions until Q3".into()),
            memory_type: MemoryType::Pattern,
            source: Source { system: "slack".into(), identifier: "#backend-2024-03-19".into(), author: Some("alex".into()) },
            scope: Scope { organization: Some("roboticforce".into()), team: Some("backend".into()), project: None },
            tags: vec!["redis".into(), "sessions".into(), "warning".into()],
            metadata: None,
            importance: Some(0.9),
            expires_at: None,
        },
        CreateMemory {
            content: "Auth service migrated from JWT to session tokens for compliance. Token revocation was the driver.".into(),
            summary: Some("JWT to session token migration".into()),
            memory_type: MemoryType::Architecture,
            source: Source { system: "confluence".into(), identifier: "ADR-042".into(), author: Some("steve".into()) },
            scope: Scope { organization: Some("roboticforce".into()), team: None, project: Some("auth".into()) },
            tags: vec!["auth".into(), "jwt".into(), "compliance".into()],
            metadata: None,
            importance: Some(0.95),
            expires_at: None,
        },
    ];

    let t = Instant::now();
    let mut ids = vec![];
    for mem in &memories {
        // Vary embeddings slightly so similarity search is meaningful
        let mut emb = fake_embedding.clone();
        emb[0] += ids.len() as f32 * 0.1;
        let id = memory_store.store(mem.clone(), emb).await?;
        ids.push(id);
        println!("  Stored: {} ({})", mem.summary.as_deref().unwrap_or(""), id);
    }
    println!("  Stored {} memories in {:?}", ids.len(), t.elapsed());

    // Count
    let count = memory_store.count(None).await?;
    println!("  Total memories: {count}");

    // Retrieve
    let t = Instant::now();
    let mem = memory_store.get(ids[0]).await?;
    println!("  Get by ID: '{}' in {:?}", mem.summary.unwrap_or_default(), t.elapsed());

    // Semantic search
    let t = Instant::now();
    let results = memory_store
        .search_semantic(fake_embedding.clone(), 10, 0.0_f64, None)
        .await?;
    println!("  Semantic search: {} results in {:?}", results.len(), t.elapsed());

    // Full-text search
    let t = Instant::now();
    let results = memory_store.search_fulltext("postgres billing", 10).await?;
    println!("  Full-text 'postgres billing': {} results in {:?}", results.len(), t.elapsed());

    let t = Instant::now();
    let results = memory_store.search_fulltext("redis sessions", 10).await?;
    println!("  Full-text 'redis sessions': {} results in {:?}", results.len(), t.elapsed());

    // Dedup check
    let fingerprint_check = memory_store
        .find_by_fingerprint(&engram_core::memory::store::compute_fingerprint_pub(&memories[0].content))
        .await?;
    println!("  Fingerprint dedup check: {:?}", fingerprint_check);

    // --- Graph Store Tests ---
    println!("\n--- Code Graph ---");

    // Build a small graph simulating a real codebase
    let file_auth = Symbol {
        id: Uuid::new_v4(), name: "auth/session.py".into(), symbol_type: SymbolType::File,
        file_path: "auth/session.py".into(), start_line: None, end_line: None,
        language: "python".into(), project: "myapp".into(), signature: None, file_mtime: Utc::now(),
        layer: None,
    };
    let fn_validate = Symbol {
        id: Uuid::new_v4(), name: "validate_user".into(), symbol_type: SymbolType::Function,
        file_path: "auth/session.py".into(), start_line: Some(15), end_line: Some(42),
        language: "python".into(), project: "myapp".into(),
        signature: Some("def validate_user(token: str) -> User".into()), file_mtime: Utc::now(),
        layer: None,
    };
    let fn_login = Symbol {
        id: Uuid::new_v4(), name: "login".into(), symbol_type: SymbolType::Function,
        file_path: "api/routes.py".into(), start_line: Some(88), end_line: Some(120),
        language: "python".into(), project: "myapp".into(),
        signature: Some("async def login(request: Request) -> Response".into()), file_mtime: Utc::now(),
        layer: None,
    };
    let fn_middleware = Symbol {
        id: Uuid::new_v4(), name: "auth_middleware".into(), symbol_type: SymbolType::Function,
        file_path: "middleware/auth.py".into(), start_line: Some(10), end_line: Some(35),
        language: "python".into(), project: "myapp".into(),
        signature: Some("async def auth_middleware(request, call_next)".into()), file_mtime: Utc::now(),
        layer: None,
    };
    let fn_dashboard = Symbol {
        id: Uuid::new_v4(), name: "get_dashboard".into(), symbol_type: SymbolType::Function,
        file_path: "api/views.py".into(), start_line: Some(44), end_line: Some(72),
        language: "python".into(), project: "myapp".into(),
        signature: Some("async def get_dashboard(user: User) -> DashboardData".into()), file_mtime: Utc::now(),
        layer: None,
    };
    let fn_test = Symbol {
        id: Uuid::new_v4(), name: "test_validate_user".into(), symbol_type: SymbolType::Function,
        file_path: "tests/test_auth.py".into(), start_line: Some(10), end_line: Some(30),
        language: "python".into(), project: "myapp".into(),
        signature: Some("def test_validate_user()".into()), file_mtime: Utc::now(),
        layer: None,
    };

    let t = Instant::now();
    for sym in [&file_auth, &fn_validate, &fn_login, &fn_middleware, &fn_dashboard, &fn_test] {
        graph_store.upsert_symbol(sym).await?;
    }
    println!("  Inserted 6 symbols in {:?}", t.elapsed());

    // Relationships: login -> validate_user, middleware -> validate_user,
    // dashboard -> middleware, test -> validate_user
    let rels = vec![
        Relationship { source_id: fn_login.id, target_id: fn_validate.id, rel_type: RelationType::Calls, confidence: 1.0 },
        Relationship { source_id: fn_middleware.id, target_id: fn_validate.id, rel_type: RelationType::Calls, confidence: 1.0 },
        Relationship { source_id: fn_dashboard.id, target_id: fn_middleware.id, rel_type: RelationType::Calls, confidence: 0.9 },
        Relationship { source_id: fn_test.id, target_id: fn_validate.id, rel_type: RelationType::Calls, confidence: 1.0 },
        Relationship { source_id: file_auth.id, target_id: fn_validate.id, rel_type: RelationType::Defines, confidence: 1.0 },
    ];

    let t = Instant::now();
    for rel in &rels {
        graph_store.add_relationship(rel).await?;
    }
    println!("  Inserted {} relationships in {:?}", rels.len(), t.elapsed());

    // Impact analysis: "What breaks if I change validate_user?"
    println!("\n--- Impact Analysis: 'What calls validate_user?' ---");
    let t = Instant::now();
    let impact = graph_store
        .impact_analysis(fn_validate.id, Direction::Upstream, 3)
        .await?;
    println!("  Found {} affected symbols in {:?}:", impact.len(), t.elapsed());
    for result in &impact {
        println!(
            "    depth={} {} {}:{} (confidence: {:.2}, via {:?})",
            result.depth,
            result.symbol.name,
            result.symbol.file_path,
            result.symbol.start_line.unwrap_or(0),
            result.confidence,
            result.relationship,
        );
    }

    // Impact downstream: "What does dashboard depend on?"
    println!("\n--- Impact Analysis: 'What does get_dashboard depend on?' ---");
    let t = Instant::now();
    let impact = graph_store
        .impact_analysis(fn_dashboard.id, Direction::Downstream, 3)
        .await?;
    println!("  Found {} dependencies in {:?}:", impact.len(), t.elapsed());
    for result in &impact {
        println!(
            "    depth={} {} {}:{} (confidence: {:.2})",
            result.depth,
            result.symbol.name,
            result.symbol.file_path,
            result.symbol.start_line.unwrap_or(0),
            result.confidence,
        );
    }

    // Find symbol by name
    let t = Instant::now();
    let found = graph_store.find_symbol("validate_user", None, None).await?;
    println!("\n  Find 'validate_user': {} results in {:?}", found.len(), t.elapsed());

    // Incremental: remove a file's symbols
    let t = Instant::now();
    let removed = graph_store.remove_file("auth/session.py", "myapp").await?;
    println!("  Remove file 'auth/session.py': {removed} symbols removed in {:?}", t.elapsed());

    println!("\n=== Spike 1 Complete ===");
    println!("Memory store: OK");
    println!("Graph store: OK");
    println!("Impact analysis (recursive CTE): OK");
    println!("pgvector semantic search: OK");
    println!("Full-text search: OK");

    Ok(())
}
