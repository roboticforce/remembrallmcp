use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use engram_core::{
    embed::{Embedder, FastEmbedder},
    graph::{
        layers::detect_layer,
        store::GraphStore,
        types::{Direction, RelationType, Relationship, Symbol, SymbolType},
    },
    memory::{
        store::{compute_fingerprint_pub, MemoryStore},
        types::{CreateMemory, MemoryQuery, MemoryType, Scope, Source},
    },
    parser::{parse_python_file, parse_ts_file, TsLang},
};

// ---------------------------------------------------------------------------
// Test runner helpers
// ---------------------------------------------------------------------------

struct Runner {
    passed: usize,
    failed: usize,
}

impl Runner {
    fn new() -> Self {
        Self { passed: 0, failed: 0 }
    }

    fn record(&mut self, name: &str, result: Result<()>) {
        match result {
            Ok(()) => {
                println!("[PASS] {name}");
                self.passed += 1;
            }
            Err(e) => {
                println!("[FAIL] {name}: {e}");
                self.failed += 1;
            }
        }
    }

    fn summary(&self) {
        println!("\n{} passed, {} failed", self.passed, self.failed);
    }

    fn exit_code(&self) -> i32 {
        if self.failed > 0 { 1 } else { 0 }
    }
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn make_source() -> Source {
    Source {
        system: "test".to_string(),
        identifier: "test://integration".to_string(),
        author: Some("test-runner".to_string()),
    }
}

fn make_scope() -> Scope {
    Scope {
        organization: None,
        team: None,
        project: None,
    }
}

fn make_create_memory(content: &str, memory_type: MemoryType) -> CreateMemory {
    CreateMemory {
        content: content.to_string(),
        summary: None,
        memory_type,
        source: make_source(),
        scope: make_scope(),
        tags: vec![],
        metadata: None,
        importance: Some(0.5),
        expires_at: None,
    }
}

fn dummy_embedding(seed: f32) -> Vec<f32> {
    let mut v = vec![0.0_f32; 384];
    v[0] = seed;
    v[1] = seed * 0.5;
    v
}

fn make_symbol(name: &str, file_path: &str, symbol_type: SymbolType, project: &str) -> Symbol {
    Symbol {
        id: Uuid::new_v4(),
        name: name.to_string(),
        symbol_type,
        file_path: file_path.to_string(),
        start_line: Some(1),
        end_line: Some(10),
        language: "rust".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime: Utc::now(),
        layer: None,
    }
}

// ---------------------------------------------------------------------------
// Memory CRUD tests
// ---------------------------------------------------------------------------

async fn test_memory_store(memory: &MemoryStore) -> Result<Uuid> {
    let input = make_create_memory("We decided to use Rust for performance reasons.", MemoryType::Decision);
    let id = memory.store(input, dummy_embedding(0.1)).await?;
    anyhow::ensure!(id != Uuid::nil(), "store returned nil UUID");
    Ok(id)
}

async fn test_memory_get(memory: &MemoryStore, id: Uuid) -> Result<()> {
    let m = memory.get(id).await?;
    anyhow::ensure!(
        m.content == "We decided to use Rust for performance reasons.",
        "content mismatch: got '{}'", m.content
    );
    anyhow::ensure!(m.id == id, "id mismatch");
    Ok(())
}

async fn test_memory_update(memory: &MemoryStore, id: Uuid) -> Result<()> {
    let updated = memory
        .update(id, Some("Updated content via integration test.".to_string()), None, None, None, None)
        .await?;
    anyhow::ensure!(updated, "update returned false - row not found");
    let m = memory.get_readonly(id).await?;
    anyhow::ensure!(
        m.content == "Updated content via integration test.",
        "updated content mismatch: got '{}'", m.content
    );
    Ok(())
}

async fn test_memory_delete(memory: &MemoryStore, id: Uuid) -> Result<()> {
    let deleted = memory.delete(id).await?;
    anyhow::ensure!(deleted, "delete returned false");
    let result = memory.get(id).await;
    anyhow::ensure!(result.is_err(), "memory still exists after delete");
    Ok(())
}

async fn test_memory_count(memory: &MemoryStore) -> Result<()> {
    let before = memory.count(None).await?;
    let input = make_create_memory("Count test memory.", MemoryType::Pattern);
    let id = memory.store(input, dummy_embedding(0.9)).await?;
    let after = memory.count(None).await?;
    anyhow::ensure!(after == before + 1, "count did not increment: before={before}, after={after}");
    memory.delete(id).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Search tests
// ---------------------------------------------------------------------------

async fn seed_search_memories(memory: &MemoryStore, embedder: &FastEmbedder) -> Result<Vec<Uuid>> {
    let texts = [
        ("The Rust compiler prevents memory safety bugs at compile time.", MemoryType::Decision),
        ("PostgreSQL with pgvector enables fast similarity search.", MemoryType::Architecture),
        ("We use tokio for async runtime in all services.", MemoryType::Preference),
        ("The deployment pipeline runs on GitHub Actions.", MemoryType::Guideline),
        ("Database connection pooling reduced latency by 40%.", MemoryType::Outcome),
    ];
    let mut ids = Vec::new();
    for (text, mt) in texts {
        let emb = tokio::task::spawn_blocking({
            let text = text.to_string();
            let embedder_ref = unsafe {
                // SAFETY: we await immediately and the embedder outlives this scope
                &*(embedder as *const FastEmbedder)
            };
            move || embedder_ref.embed(&text)
        })
        .await??;
        let id = memory.store(make_create_memory(text, mt), emb).await?;
        ids.push(id);
    }
    Ok(ids)
}

async fn test_search_semantic(memory: &MemoryStore, embedder: &FastEmbedder) -> Result<()> {
    let query = "Rust memory safety and compiler";
    let emb = tokio::task::spawn_blocking({
        let q = query.to_string();
        let embedder_ref = unsafe { &*(embedder as *const FastEmbedder) };
        move || embedder_ref.embed(&q)
    })
    .await??;

    let results = memory.search_semantic(emb, 5, 0.0, None).await?;
    anyhow::ensure!(!results.is_empty(), "semantic search returned no results");
    Ok(())
}

async fn test_search_fulltext(memory: &MemoryStore) -> Result<()> {
    let results = memory.search_fulltext("pgvector similarity", 5).await?;
    anyhow::ensure!(!results.is_empty(), "fulltext search returned no results for 'pgvector similarity'");
    Ok(())
}

async fn test_search_hybrid(memory: &MemoryStore, embedder: &FastEmbedder) -> Result<()> {
    let query_text = "database connection pooling latency";
    let emb = tokio::task::spawn_blocking({
        let q = query_text.to_string();
        let embedder_ref = unsafe { &*(embedder as *const FastEmbedder) };
        move || embedder_ref.embed(&q)
    })
    .await??;

    let query = MemoryQuery {
        query: query_text.to_string(),
        memory_types: None,
        scope: None,
        tags: None,
        limit: Some(5),
        min_similarity: Some(0.0),
    };

    let results = memory.search_hybrid(emb, &query).await?;
    anyhow::ensure!(!results.is_empty(), "hybrid search returned no results");
    Ok(())
}

// ---------------------------------------------------------------------------
// Dedup tests
// ---------------------------------------------------------------------------

async fn test_dedup_exact(memory: &MemoryStore) -> Result<()> {
    let content = "Exact duplicate detection test content - unique string 7f3a9b.";
    let fp = compute_fingerprint_pub(content);

    let id1 = memory
        .store(make_create_memory(content, MemoryType::Guideline), dummy_embedding(0.3))
        .await?;

    let found = memory.find_by_fingerprint(&fp).await?;
    anyhow::ensure!(found == Some(id1), "fingerprint lookup did not return original id");

    // Clean up
    memory.delete(id1).await?;
    Ok(())
}

async fn test_dedup_different(memory: &MemoryStore) -> Result<()> {
    let content_a = "First unique content for dedup test A9k2m.";
    let content_b = "Second completely different content B7z1x.";

    let fp_a = compute_fingerprint_pub(content_a);
    let fp_b = compute_fingerprint_pub(content_b);

    anyhow::ensure!(fp_a != fp_b, "different content produced same fingerprint");

    let id_a = memory
        .store(make_create_memory(content_a, MemoryType::Pattern), dummy_embedding(0.4))
        .await?;

    let found = memory.find_by_fingerprint(&fp_b).await?;
    anyhow::ensure!(found.is_none(), "fingerprint B wrongly matched content A");

    memory.delete(id_a).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Fingerprinting tests (pure, no DB)
// ---------------------------------------------------------------------------

fn test_fingerprint_same() -> Result<()> {
    let fp1 = compute_fingerprint_pub("hello world");
    let fp2 = compute_fingerprint_pub("hello world");
    anyhow::ensure!(fp1 == fp2, "same content produced different fingerprints");
    Ok(())
}

fn test_fingerprint_different() -> Result<()> {
    let fp1 = compute_fingerprint_pub("content alpha");
    let fp2 = compute_fingerprint_pub("content beta");
    anyhow::ensure!(fp1 != fp2, "different content produced same fingerprint");
    Ok(())
}

fn test_fingerprint_whitespace() -> Result<()> {
    let fp1 = compute_fingerprint_pub("hello   world");
    let fp2 = compute_fingerprint_pub("hello world");
    anyhow::ensure!(
        fp1 == fp2,
        "whitespace normalization failed: '{fp1}' != '{fp2}'"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Graph CRUD tests
// ---------------------------------------------------------------------------

async fn test_graph_upsert_symbol(graph: &GraphStore) -> Result<Uuid> {
    let sym = make_symbol("main", "src/main.rs", SymbolType::Function, "test_proj");
    let id = graph.upsert_symbol(&sym).await?;
    anyhow::ensure!(id == sym.id, "upsert returned wrong id");
    Ok(id)
}

async fn test_graph_add_relationship(graph: &GraphStore) -> Result<(Uuid, Uuid)> {
    let sym_a = make_symbol("handler", "src/api/routes.rs", SymbolType::Function, "test_proj");
    let sym_b = make_symbol("user_service", "src/services/user.rs", SymbolType::Function, "test_proj");

    graph.upsert_symbol(&sym_a).await?;
    graph.upsert_symbol(&sym_b).await?;

    let rel = Relationship {
        source_id: sym_a.id,
        target_id: sym_b.id,
        rel_type: RelationType::Calls,
        confidence: 1.0,
    };
    graph.add_relationship(&rel).await?;
    Ok((sym_a.id, sym_b.id))
}

async fn test_graph_find_symbol(graph: &GraphStore) -> Result<()> {
    let sym = make_symbol("findable_fn", "src/lib.rs", SymbolType::Function, "test_proj");
    graph.upsert_symbol(&sym).await?;

    let found = graph.find_symbol("findable_fn", None, None).await?;
    anyhow::ensure!(!found.is_empty(), "find_symbol returned nothing");
    anyhow::ensure!(
        found.iter().any(|s| s.name == "findable_fn"),
        "find_symbol result missing expected symbol"
    );
    Ok(())
}

async fn test_graph_remove_file(graph: &GraphStore) -> Result<()> {
    let sym = make_symbol("ephemeral_fn", "src/ephemeral.rs", SymbolType::Function, "test_proj");
    graph.upsert_symbol(&sym).await?;

    let count = graph.remove_file("src/ephemeral.rs", "test_proj").await?;
    anyhow::ensure!(count > 0, "remove_file reported 0 rows affected");

    let found = graph.find_symbol("ephemeral_fn", None, Some("test_proj")).await?;
    anyhow::ensure!(found.is_empty(), "symbols still present after remove_file");
    Ok(())
}

// ---------------------------------------------------------------------------
// Impact analysis tests
// ---------------------------------------------------------------------------

async fn setup_impact_chain(graph: &GraphStore) -> Result<(Uuid, Uuid, Uuid)> {
    // Chain: A calls B calls C
    let sym_a = make_symbol("fn_a", "src/a.rs", SymbolType::Function, "impact_proj");
    let sym_b = make_symbol("fn_b", "src/b.rs", SymbolType::Function, "impact_proj");
    let sym_c = make_symbol("fn_c", "src/c.rs", SymbolType::Function, "impact_proj");

    graph.upsert_symbol(&sym_a).await?;
    graph.upsert_symbol(&sym_b).await?;
    graph.upsert_symbol(&sym_c).await?;

    // A -> B (A calls B)
    graph.add_relationship(&Relationship {
        source_id: sym_a.id,
        target_id: sym_b.id,
        rel_type: RelationType::Calls,
        confidence: 1.0,
    }).await?;

    // B -> C (B calls C)
    graph.add_relationship(&Relationship {
        source_id: sym_b.id,
        target_id: sym_c.id,
        rel_type: RelationType::Calls,
        confidence: 1.0,
    }).await?;

    Ok((sym_a.id, sym_b.id, sym_c.id))
}

async fn test_impact_upstream(graph: &GraphStore, sym_c_id: Uuid) -> Result<()> {
    // Upstream of C = who calls C? Should find B and A.
    let results = graph.impact_analysis(sym_c_id, Direction::Upstream, 10).await?;
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();
    anyhow::ensure!(
        names.contains(&"fn_b"),
        "upstream of fn_c should include fn_b, got: {names:?}"
    );
    anyhow::ensure!(
        names.contains(&"fn_a"),
        "upstream of fn_c should include fn_a, got: {names:?}"
    );
    Ok(())
}

async fn test_impact_downstream(graph: &GraphStore, sym_a_id: Uuid) -> Result<()> {
    // Downstream of A = what does A call? Should find B and C.
    let results = graph.impact_analysis(sym_a_id, Direction::Downstream, 10).await?;
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();
    anyhow::ensure!(
        names.contains(&"fn_b"),
        "downstream of fn_a should include fn_b, got: {names:?}"
    );
    anyhow::ensure!(
        names.contains(&"fn_c"),
        "downstream of fn_a should include fn_c, got: {names:?}"
    );
    Ok(())
}

async fn test_impact_depth_limit(graph: &GraphStore, sym_a_id: Uuid) -> Result<()> {
    // max_depth=1 from A should only reach B directly, not C.
    let results = graph.impact_analysis(sym_a_id, Direction::Downstream, 1).await?;
    let names: Vec<&str> = results.iter().map(|r| r.symbol.name.as_str()).collect();
    anyhow::ensure!(
        names.contains(&"fn_b"),
        "depth-1 downstream of fn_a should include fn_b, got: {names:?}"
    );
    anyhow::ensure!(
        !names.contains(&"fn_c"),
        "depth-1 downstream of fn_a should NOT include fn_c, got: {names:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Layer detection tests (pure, no DB)
// ---------------------------------------------------------------------------

fn test_layer_api() -> Result<()> {
    let layer = detect_layer("src/api/routes.rs");
    anyhow::ensure!(layer.as_deref() == Some("api"), "expected 'api', got: {layer:?}");
    Ok(())
}

fn test_layer_service() -> Result<()> {
    let layer = detect_layer("src/services/billing.rs");
    anyhow::ensure!(layer.as_deref() == Some("service"), "expected 'service', got: {layer:?}");
    Ok(())
}

fn test_layer_data() -> Result<()> {
    let layer = detect_layer("src/models/user.rs");
    anyhow::ensure!(layer.as_deref() == Some("data"), "expected 'data', got: {layer:?}");
    Ok(())
}

fn test_layer_none() -> Result<()> {
    let layer = detect_layer("src/main.rs");
    anyhow::ensure!(layer.is_none(), "expected None for src/main.rs, got: {layer:?}");
    Ok(())
}

fn test_layer_test() -> Result<()> {
    let layer = detect_layer("tests/integration.rs");
    anyhow::ensure!(layer.as_deref() == Some("test"), "expected 'test', got: {layer:?}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tour generation tests
// ---------------------------------------------------------------------------

async fn test_tour_order(graph: &GraphStore) -> Result<()> {
    // Build: main.rs imports lib.rs, lib.rs imports utils.rs
    // Tour order should be: main.rs first (entry point - nothing imports it),
    // then lib.rs, then utils.rs (most depended-upon).
    //
    // Note: generate_tour uses Kahn's BFS where in_degree = # files that import you.
    // Entry points (in_degree 0) go first. utils.rs is imported by lib.rs (in_degree 1),
    // lib.rs is imported by main.rs (in_degree 1). main.rs is imported by no one (in_degree 0).
    //
    // Expected order: main.rs (entry) -> lib.rs -> utils.rs

    let proj = "tour_proj";
    let now = Utc::now();

    let main_sym = Symbol {
        id: Uuid::new_v4(),
        name: "main.rs".to_string(),
        symbol_type: SymbolType::File,
        file_path: "main.rs".to_string(),
        start_line: None,
        end_line: None,
        language: "rust".to_string(),
        project: proj.to_string(),
        signature: None,
        file_mtime: now,
        layer: None,
    };

    let lib_sym = Symbol {
        id: Uuid::new_v4(),
        name: "lib.rs".to_string(),
        symbol_type: SymbolType::File,
        file_path: "lib.rs".to_string(),
        start_line: None,
        end_line: None,
        language: "rust".to_string(),
        project: proj.to_string(),
        signature: None,
        file_mtime: now,
        layer: None,
    };

    let utils_sym = Symbol {
        id: Uuid::new_v4(),
        name: "utils.rs".to_string(),
        symbol_type: SymbolType::File,
        file_path: "utils.rs".to_string(),
        start_line: None,
        end_line: None,
        language: "rust".to_string(),
        project: proj.to_string(),
        signature: None,
        file_mtime: now,
        layer: None,
    };

    graph.upsert_symbol(&main_sym).await?;
    graph.upsert_symbol(&lib_sym).await?;
    graph.upsert_symbol(&utils_sym).await?;

    // main.rs imports lib.rs
    graph.add_relationship(&Relationship {
        source_id: main_sym.id,
        target_id: lib_sym.id,
        rel_type: RelationType::Imports,
        confidence: 1.0,
    }).await?;

    // lib.rs imports utils.rs
    graph.add_relationship(&Relationship {
        source_id: lib_sym.id,
        target_id: utils_sym.id,
        rel_type: RelationType::Imports,
        confidence: 1.0,
    }).await?;

    let stops = graph.generate_tour(proj, 10).await?;
    anyhow::ensure!(!stops.is_empty(), "generate_tour returned no stops");

    let paths: Vec<&str> = stops.iter().map(|s| s.file_path.as_str()).collect();
    let main_pos = paths.iter().position(|&p| p == "main.rs")
        .ok_or_else(|| anyhow::anyhow!("main.rs not in tour: {paths:?}"))?;
    let utils_pos = paths.iter().position(|&p| p == "utils.rs")
        .ok_or_else(|| anyhow::anyhow!("utils.rs not in tour: {paths:?}"))?;

    anyhow::ensure!(
        main_pos == 0,
        "main.rs should be first (entry point), got position {main_pos}. Tour: {paths:?}"
    );
    anyhow::ensure!(
        utils_pos > main_pos,
        "utils.rs should appear after main.rs. Tour: {paths:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Embedder tests
// ---------------------------------------------------------------------------

fn test_embedder_new() -> Result<FastEmbedder> {
    let e = FastEmbedder::new()?;
    Ok(e)
}

fn test_embedder_single(embedder: &FastEmbedder) -> Result<()> {
    let v = embedder.embed("hello world")?;
    anyhow::ensure!(v.len() == 384, "expected 384-dim vector, got {}", v.len());
    Ok(())
}

fn test_embedder_batch(embedder: &FastEmbedder) -> Result<()> {
    let results = embedder.embed_batch(&["alpha", "beta"])?;
    anyhow::ensure!(results.len() == 2, "expected 2 results, got {}", results.len());
    anyhow::ensure!(results[0].len() == 384, "first result: expected 384-dim, got {}", results[0].len());
    anyhow::ensure!(results[1].len() == 384, "second result: expected 384-dim, got {}", results[1].len());
    Ok(())
}

fn test_embedder_empty(embedder: &FastEmbedder) -> Result<()> {
    // Should not panic - result is either Ok with a vector or a clean Err
    let _ = embedder.embed("");
    Ok(())
}

// ---------------------------------------------------------------------------
// Parser tests (pure, no DB)
// ---------------------------------------------------------------------------

fn test_parser_python() -> Result<()> {
    let source = r#"
def greet(name: str) -> str:
    return f"hello {name}"

class UserService:
    def get_user(self, user_id: int):
        pass
"#;

    let result = parse_python_file("test.py", source, "test_proj", Utc::now());
    anyhow::ensure!(
        !result.symbols.is_empty(),
        "Python parser returned no symbols"
    );

    let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
    anyhow::ensure!(
        names.contains(&"greet"),
        "Python parser missed 'greet' function, got: {names:?}"
    );
    anyhow::ensure!(
        names.contains(&"UserService"),
        "Python parser missed 'UserService' class, got: {names:?}"
    );
    Ok(())
}

fn test_parser_typescript() -> Result<()> {
    let source = r#"
export function formatDate(date: Date): string {
    return date.toISOString();
}

export class ApiClient {
    constructor(private baseUrl: string) {}

    async get(path: string): Promise<Response> {
        return fetch(this.baseUrl + path);
    }
}
"#;

    let result = parse_ts_file("test.ts", source, "test_proj", Utc::now(), TsLang::TypeScript);
    anyhow::ensure!(
        !result.symbols.is_empty(),
        "TypeScript parser returned no symbols"
    );

    let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
    anyhow::ensure!(
        names.contains(&"formatDate"),
        "TypeScript parser missed 'formatDate', got: {names:?}"
    );
    anyhow::ensure!(
        names.contains(&"ApiClient"),
        "TypeScript parser missed 'ApiClient', got: {names:?}"
    );
    Ok(())
}

fn test_parser_invalid() -> Result<()> {
    // Deliberately broken Python - parser must not crash
    let source = "def (((((unclosed broken syntax !!!";
    let result = parse_python_file("bad.py", source, "test_proj", Utc::now());
    // Result may be empty but must not panic
    let _ = result.symbols.len();
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5450/engram".to_string());

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to database at {database_url}: {e}"))?;

    let schema = "engram_integration_test";

    println!("=== Engram Integration Tests ===");
    println!("Schema: {schema}");
    println!();

    // Clean up any leftover schema from previous run
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&pool)
        .await?;

    let memory = MemoryStore::new(pool.clone(), schema.to_string())?;
    let graph = GraphStore::new(pool.clone(), schema.to_string())?;
    memory.init().await?;
    graph.init().await?;

    let mut runner = Runner::new();

    // --- Embedder (initialized once, reused across search tests) ---
    // We initialize the embedder first since search tests depend on it.
    // The embedder test itself also records pass/fail.
    let embedder = match test_embedder_new() {
        Ok(e) => {
            runner.record("embedder_new", Ok(()));
            e
        }
        Err(e) => {
            runner.record("embedder_new", Err(e));
            // Cannot continue search tests without the embedder
            runner.summary();
            std::process::exit(runner.exit_code());
        }
    };

    runner.record("embedder_single", test_embedder_single(&embedder));
    runner.record("embedder_batch", test_embedder_batch(&embedder));
    runner.record("embedder_empty", test_embedder_empty(&embedder));

    // --- Memory CRUD ---
    let memory_id = match test_memory_store(&memory).await {
        Ok(id) => {
            runner.record("memory_store", Ok(()));
            id
        }
        Err(e) => {
            runner.record("memory_store", Err(e));
            // Remaining memory CRUD tests depend on this ID
            Uuid::nil()
        }
    };

    if memory_id != Uuid::nil() {
        runner.record("memory_get", test_memory_get(&memory, memory_id).await);
        runner.record("memory_update", test_memory_update(&memory, memory_id).await);
        runner.record("memory_delete", test_memory_delete(&memory, memory_id).await);
    } else {
        runner.record("memory_get", Err(anyhow::anyhow!("skipped - memory_store failed")));
        runner.record("memory_update", Err(anyhow::anyhow!("skipped - memory_store failed")));
        runner.record("memory_delete", Err(anyhow::anyhow!("skipped - memory_store failed")));
    }

    runner.record("memory_count", test_memory_count(&memory).await);

    // --- Search (seed first, then test each strategy) ---
    let search_ids = seed_search_memories(&memory, &embedder).await;
    match &search_ids {
        Ok(_) => {
            runner.record("search_semantic", test_search_semantic(&memory, &embedder).await);
            runner.record("search_fulltext", test_search_fulltext(&memory).await);
            runner.record("search_hybrid", test_search_hybrid(&memory, &embedder).await);
        }
        Err(e) => {
            let msg = format!("skipped - seed failed: {e}");
            runner.record("search_semantic", Err(anyhow::anyhow!("{}", msg)));
            runner.record("search_fulltext", Err(anyhow::anyhow!("{}", msg)));
            runner.record("search_hybrid", Err(anyhow::anyhow!("{}", msg)));
        }
    }
    // Clean up search seed data
    if let Ok(ids) = search_ids {
        for id in ids {
            let _ = memory.delete(id).await;
        }
    }

    // --- Dedup ---
    runner.record("dedup_exact", test_dedup_exact(&memory).await);
    runner.record("dedup_different", test_dedup_different(&memory).await);

    // --- Graph CRUD ---
    let _graph_main_id = match test_graph_upsert_symbol(&graph).await {
        Ok(id) => {
            runner.record("graph_upsert_symbol", Ok(()));
            id
        }
        Err(e) => {
            runner.record("graph_upsert_symbol", Err(e));
            Uuid::nil()
        }
    };

    runner.record("graph_add_relationship", test_graph_add_relationship(&graph).await.map(|_| ()));
    runner.record("graph_find_symbol", test_graph_find_symbol(&graph).await);
    runner.record("graph_remove_file", test_graph_remove_file(&graph).await);

    // --- Impact analysis ---
    match setup_impact_chain(&graph).await {
        Ok((sym_a_id, _sym_b_id, sym_c_id)) => {
            runner.record("impact_upstream", test_impact_upstream(&graph, sym_c_id).await);
            runner.record("impact_downstream", test_impact_downstream(&graph, sym_a_id).await);
            runner.record("impact_depth_limit", test_impact_depth_limit(&graph, sym_a_id).await);
        }
        Err(e) => {
            let msg = format!("skipped - chain setup failed: {e}");
            runner.record("impact_upstream", Err(anyhow::anyhow!("{}", msg)));
            runner.record("impact_downstream", Err(anyhow::anyhow!("{}", msg)));
            runner.record("impact_depth_limit", Err(anyhow::anyhow!("{}", msg)));
        }
    }

    // --- Layer detection ---
    runner.record("layer_api", test_layer_api());
    runner.record("layer_service", test_layer_service());
    runner.record("layer_data", test_layer_data());
    runner.record("layer_none", test_layer_none());
    runner.record("layer_test", test_layer_test());

    // --- Tour ---
    runner.record("tour_order", test_tour_order(&graph).await);

    // --- Fingerprinting ---
    runner.record("fingerprint_same", test_fingerprint_same());
    runner.record("fingerprint_different", test_fingerprint_different());
    runner.record("fingerprint_whitespace", test_fingerprint_whitespace());

    // --- Parsers ---
    runner.record("parser_python", test_parser_python());
    runner.record("parser_typescript", test_parser_typescript());
    runner.record("parser_invalid", test_parser_invalid());

    // --- Clean up ---
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&pool)
        .await?;

    runner.summary();
    std::process::exit(runner.exit_code());
}
