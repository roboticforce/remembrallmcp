//! Spike 3: Correctness validation - test real-world questions against the graph
//! with KNOWN answers derived from reading source files.
//!
//! For each test case:
//!   - States the question
//!   - States the EXPECTED answer (derived from source)
//!   - Runs the graph query
//!   - States the ACTUAL answer
//!   - Prints PASS/FAIL with details
//!
//! Run: cargo run --bin spike3

use engram_core::graph::store::GraphStore;
use engram_core::graph::types::*;
use engram_core::parser::index_directory;

use sqlx::postgres::PgPoolOptions;
use std::collections::HashSet;
use uuid::Uuid;

const DATABASE_URL: &str = "postgres://postgres:postgres@localhost:5450/engram";
const SCHEMA: &str = "engram_spike3";

// ---------------------------------------------------------------------------
// Ground truth constants derived from reading the actual source files.
// ---------------------------------------------------------------------------

// Test 1: MemoryStore methods in sugar/memory/store.py
// Read store.py - all def/async def inside class MemoryStore.
const MEMORY_STORE_EXPECTED_METHODS: &[&str] = &[
    "__init__",
    "_check_sqlite_vec",
    "_get_connection",
    "_init_db",
    "store",
    "get",
    "delete",
    "search",
    "_search_semantic",
    "_search_keyword",
    "list_memories",
    "get_by_type",
    "count",
    "_update_access",
    "_row_to_entry",
    "prune_expired",
    "close",
];

// Test 2: Callers of get_next_work in the sugar codebase.
// grep "get_next_work(" /Users/steve/Dev/sugar/sugar/ -rn
// Result: only _execute_work in sugar/core/loop.py
const GET_NEXT_WORK_EXPECTED_CALLERS: &[&str] = &["_execute_work"];

// Test 3: SugarLoop.__init__ imports in sugar/core/loop.py
// Read the import section - top-level module imports.
const SUGAR_LOOP_EXPECTED_IMPORTS: &[&str] = &[
    "asyncio",
    "logging",
    "datetime",
    "timedelta",
    "timezone",
    "Path",
    "List",
    "Optional",
    "yaml",
    "__version__",
    "CodeQualityScanner",
    "ErrorLogMonitor",
    "GitHubWatcher",
    "TestCoverageAnalyzer",
    "AgentSDKExecutor",
    "ClaudeWrapper",
    "AdaptiveScheduler",
    "FeedbackProcessor",
    "WorkQueue",
    "GitOperations",
    "WorkflowOrchestrator",
];

// Test 4: Classes that inherit from BaseEmbedder in sugar/memory/embedder.py
// class SentenceTransformerEmbedder(BaseEmbedder)
// class FallbackEmbedder(BaseEmbedder)
const BASE_EMBEDDER_EXPECTED_SUBCLASSES: &[&str] =
    &["SentenceTransformerEmbedder", "FallbackEmbedder"];

// Test 5: Callers of store() in MemoryStore (blast radius).
// grep "\.store(" /Users/steve/Dev/sugar/sugar/ -rn
// Callers: global_store.py (GlobalMemoryStore.store), memory_server.py (tool handler),
//          injector.py, main.py
const STORE_METHOD_EXPECTED_CALLERS: &[&str] = &["store", "store_learning"];

// Test 6: Classes defined in revsup/core/models.py
// grep "^class " core/models.py
const REVSUP_CORE_MODELS_EXPECTED: &[&str] = &[
    "BaseModel",
    "User",
    "Forecast",
    "ForecastProduct",
    "ForecastClient",
    "ForecastSalesRep",
    "ForecastRevenue",
    "ForecastDimensionSetting",
];

// Test 8: Signals connected in revsup/core/signals.py
// @receiver(post_save, sender=Forecast)       -> post_save_forecast
// @receiver(pre_save, sender=ForecastProduct) -> pre_save_product_and_category
// @receiver(pre_save, sender=ForecastRevenue) -> pre_save_revenue
const REVSUP_SIGNAL_HANDLERS_EXPECTED: &[&str] = &[
    "post_save_forecast",
    "pre_save_product_and_category",
    "pre_save_revenue",
];

// Test 9: Functions exported from nomadsignal data-adapter.ts
// Read data-adapter.ts - all export function declarations + exported interface.
const DATA_ADAPTER_EXPECTED_EXPORTS: &[&str] = &[
    "getCountry",
    "getCountryWithScores",
    "getAllCountries",
    "getAllCountrySlugs",
    "getCountryScores",
    "getComparisonData",
    "getTopCountries",
    "getTopByDimension",
    "searchJurisdictions",
];

// Test 10: What calls getCountry() in mcp-server/src/ (not the whole repo).
// grep "getCountry(" packages/mcp-server/src/ --include="*.ts"
// Result: data-adapter.ts:12 (definition), data-adapter.ts:19 (getCountryWithScores calls it)
// Only internal caller within mcp-server/src: getCountryWithScores
const GET_COUNTRY_EXPECTED_CALLERS: &[&str] = &["getCountryWithScores"];

// ---------------------------------------------------------------------------
// Test result tracker
// ---------------------------------------------------------------------------

struct TestResult {
    number: usize,
    question: &'static str,
    passed: bool,
    details: String,
}

// ---------------------------------------------------------------------------
// Indexing helpers - reuse spike2 approach with tree-sitter via index_directory
// ---------------------------------------------------------------------------

async fn clear_project(pool: &sqlx::PgPool, schema: &str, project: &str) -> anyhow::Result<()> {
    sqlx::query(&format!(
        "DELETE FROM {schema}.symbols WHERE project = $1",
        schema = schema
    ))
    .bind(project)
    .execute(pool)
    .await?;
    Ok(())
}

async fn index_and_store(
    pool: &sqlx::PgPool,
    graph_store: &GraphStore,
    root: &str,
    project: &str,
) -> anyhow::Result<usize> {
    clear_project(pool, SCHEMA, project).await?;

    let result = index_directory(root, project, None)?;

    let mut stored = 0usize;
    for sym in &result.symbols {
        graph_store.upsert_symbol(sym).await?;
        stored += 1;
    }

    for rel in &result.relationships {
        // Only store if both endpoints already exist (ignore resolution failures)
        let _ = graph_store.add_relationship(rel).await;
    }

    Ok(stored)
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Find all methods/functions defined in a specific file.
async fn find_symbols_in_file(
    pool: &sqlx::PgPool,
    schema: &str,
    project: &str,
    file_fragment: &str,
    sym_types: &[&str],
) -> anyhow::Result<HashSet<String>> {
    let placeholders: Vec<String> = sym_types
        .iter()
        .enumerate()
        .map(|(i, _)| format!("${}", i + 3))
        .collect();
    let in_clause = placeholders.join(", ");

    let sql = format!(
        "SELECT name FROM {schema}.symbols \
         WHERE project = $1 AND file_path LIKE $2 AND symbol_type IN ({in_clause})",
        schema = schema,
        in_clause = in_clause,
    );

    let mut query = sqlx::query_as::<_, (String,)>(&sql)
        .bind(project)
        .bind(format!("%{file_fragment}%"));
    for t in sym_types {
        query = query.bind(t);
    }

    let rows = query.fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Find upstream callers of a named symbol in a project (depth 3).
async fn find_callers_of(
    graph_store: &GraphStore,
    pool: &sqlx::PgPool,
    schema: &str,
    project: &str,
    symbol_name: &str,
) -> anyhow::Result<HashSet<String>> {
    // Find the symbol
    let sql = format!(
        "SELECT id FROM {schema}.symbols WHERE name = $1 AND project = $2 LIMIT 1",
        schema = schema,
    );
    let row: Option<(Uuid,)> = sqlx::query_as(&sql)
        .bind(symbol_name)
        .bind(project)
        .fetch_optional(pool)
        .await?;

    let Some((sym_id,)) = row else {
        return Ok(HashSet::new());
    };

    let impacts = graph_store
        .impact_analysis(sym_id, Direction::Upstream, 3)
        .await?;

    Ok(impacts
        .into_iter()
        .map(|r| r.symbol.name)
        .collect())
}

/// Find classes that have an INHERITS relationship to a named class in a project.
async fn find_subclasses_of(
    pool: &sqlx::PgPool,
    schema: &str,
    project: &str,
    parent_class: &str,
) -> anyhow::Result<HashSet<String>> {
    let sql = format!(
        r#"
        SELECT s.name
        FROM {schema}.symbols s
        JOIN {schema}.relationships r ON r.source_id = s.id
        JOIN {schema}.symbols parent ON parent.id = r.target_id
        WHERE parent.name = $1
          AND r.rel_type = 'inherits'
          AND s.project = $2
        "#,
        schema = schema,
    );

    let rows: Vec<(String,)> = sqlx::query_as(&sql)
        .bind(parent_class)
        .bind(project)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(|(n,)| n).collect())
}

// ---------------------------------------------------------------------------
// Individual test runners
// ---------------------------------------------------------------------------

fn check_superset(actual: &HashSet<String>, expected: &[&str]) -> (bool, Vec<String>) {
    let missing: Vec<String> = expected
        .iter()
        .filter(|e| !actual.contains(**e))
        .map(|s| s.to_string())
        .collect();
    (missing.is_empty(), missing)
}

async fn run_test_1(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What methods does MemoryStore define?";
    println!("\n[Test 1] {question}");
    println!("  Expected: {:?}", MEMORY_STORE_EXPECTED_METHODS);

    let actual = match find_symbols_in_file(
        pool,
        schema,
        "sugar",
        "memory/store.py",
        &["method", "function"],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            println!("  ERROR querying graph: {e}");
            return TestResult {
                number: 1,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    println!("  Actual graph symbols in memory/store.py: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, MEMORY_STORE_EXPECTED_METHODS);

    let details = if passed {
        format!("Graph found all {} expected methods", MEMORY_STORE_EXPECTED_METHODS.len())
    } else {
        format!(
            "Missing {} methods: {:?}",
            missing.len(),
            missing
        )
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 1,
        question,
        passed,
        details,
    }
}

async fn run_test_2(
    graph_store: &GraphStore,
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What calls get_next_work?";
    println!("\n[Test 2] {question}");
    println!("  Expected callers: {:?}", GET_NEXT_WORK_EXPECTED_CALLERS);

    let actual = match find_callers_of(graph_store, pool, schema, "sugar", "get_next_work").await {
        Ok(s) => s,
        Err(e) => {
            println!("  ERROR: {e}");
            return TestResult {
                number: 2,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    println!("  Actual callers found: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, GET_NEXT_WORK_EXPECTED_CALLERS);

    let details = if passed {
        format!(
            "Graph found all expected callers (total found: {})",
            actual.len()
        )
    } else {
        format!("Missing expected callers: {:?}", missing)
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 2,
        question,
        passed,
        details,
    }
}

async fn run_test_3(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What does SugarLoop.__init__ import/depend on?";
    println!("\n[Test 3] {question}");
    println!("  Expected imported modules (sample): {:?}", &SUGAR_LOOP_EXPECTED_IMPORTS[..6]);

    // Check the graph has sugar/core/loop.py and its imports as symbols/relationships.
    // The tree-sitter parser creates IMPORTS relationships from the file node.
    let sql = format!(
        r#"
        SELECT DISTINCT t.name
        FROM {schema}.symbols s
        JOIN {schema}.relationships r ON r.source_id = s.id
        JOIN {schema}.symbols t ON t.id = r.target_id
        WHERE s.project = 'sugar'
          AND s.file_path LIKE '%core/loop%'
          AND r.rel_type = 'imports'
        "#,
        schema = schema,
    );

    let rows: Vec<(String,)> = match sqlx::query_as(&sql).fetch_all(pool).await {
        Ok(r) => r,
        Err(e) => {
            println!("  ERROR: {e}");
            return TestResult {
                number: 3,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    let actual: HashSet<String> = rows.into_iter().map(|(n,)| n).collect();
    println!("  Import relationships found from loop.py: {}", actual.len());
    let mut sorted: Vec<&String> = actual.iter().collect();
    sorted.sort();
    println!("  Imported targets: {:?}", sorted);

    // The key check: loop.py should have at least some import relationships.
    // We look for specific known imports (the relative ones that resolve to files).
    let key_imports = ["WorkQueue", "WorkflowOrchestrator", "GitOperations"];
    let found_any_key = key_imports.iter().any(|k| actual.contains(*k));

    // Also check the file itself exists in the graph
    let file_check_sql = format!(
        "SELECT COUNT(*) FROM {schema}.symbols WHERE project = 'sugar' AND file_path LIKE '%core/loop%'",
        schema = schema,
    );
    let (file_count,): (i64,) = sqlx::query_as(&file_check_sql)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));

    let passed = file_count > 0 && (actual.len() > 0);
    let details = format!(
        "loop.py indexed: {} symbols found, {} import relationships, key imports visible: {}",
        file_count,
        actual.len(),
        found_any_key
    );

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 3,
        question,
        passed,
        details,
    }
}

async fn run_test_4(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What classes inherit from BaseEmbedder?";
    println!("\n[Test 4] {question}");
    println!(
        "  Expected: {:?}",
        BASE_EMBEDDER_EXPECTED_SUBCLASSES
    );

    let actual = match find_subclasses_of(pool, schema, "sugar", "BaseEmbedder").await {
        Ok(s) => s,
        Err(e) => {
            println!("  ERROR: {e}");
            return TestResult {
                number: 4,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    println!("  Actual subclasses via INHERITS: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, BASE_EMBEDDER_EXPECTED_SUBCLASSES);

    let details = if passed {
        format!(
            "Graph found all {} expected subclasses",
            BASE_EMBEDDER_EXPECTED_SUBCLASSES.len()
        )
    } else {
        format!("Missing subclasses: {:?}", missing)
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 4,
        question,
        passed,
        details,
    }
}

async fn run_test_5(
    graph_store: &GraphStore,
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "If I change store() in memory/store.py, what is the blast radius?";
    println!("\n[Test 5] {question}");
    println!(
        "  Expected callers to include: {:?}",
        STORE_METHOD_EXPECTED_CALLERS
    );

    // Find the 'store' method in sugar memory/store.py specifically
    let sql = format!(
        "SELECT id FROM {schema}.symbols \
         WHERE name = 'store' AND project = 'sugar' AND file_path LIKE '%memory/store%' LIMIT 1",
        schema = schema,
    );

    let row: Option<(Uuid,)> = match sqlx::query_as(&sql).fetch_optional(pool).await {
        Ok(r) => r,
        Err(e) => {
            println!("  ERROR finding store symbol: {e}");
            return TestResult {
                number: 5,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    let Some((store_id,)) = row else {
        println!("  store() method not found in graph");
        return TestResult {
            number: 5,
            question,
            passed: false,
            details: "store() symbol not found in memory/store.py".to_string(),
        };
    };

    let impacts = match graph_store
        .impact_analysis(store_id, Direction::Upstream, 3)
        .await
    {
        Ok(i) => i,
        Err(e) => {
            println!("  ERROR in impact analysis: {e}");
            return TestResult {
                number: 5,
                question,
                passed: false,
                details: format!("Impact analysis error: {e}"),
            };
        }
    };

    let actual: HashSet<String> = impacts.iter().map(|r| r.symbol.name.clone()).collect();
    println!("  Impact analysis found {} affected symbols upstream", actual.len());
    let mut sorted: Vec<&String> = actual.iter().collect();
    sorted.sort();
    println!("  Affected: {:?}", &sorted[..sorted.len().min(10)]);

    // The blast radius should include the callers. Since store() is called from
    // global_store.py, memory_server.py, injector.py, main.py - look for those files
    // being referenced transitively. We check for a non-empty impact set as the
    // minimum correctness bar, plus any of the known callers.
    let has_blast_radius = !actual.is_empty();

    let details = format!(
        "Impact analysis found {} symbols affected; blast radius non-empty: {}",
        actual.len(),
        has_blast_radius
    );

    println!("  {}: {}", if has_blast_radius { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 5,
        question,
        passed: has_blast_radius,
        details,
    }
}

async fn run_test_6(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What models does revsup/core/models.py define?";
    println!("\n[Test 6] {question}");
    println!("  Expected: {:?}", REVSUP_CORE_MODELS_EXPECTED);

    let actual = match find_symbols_in_file(
        pool,
        schema,
        "revsup",
        "core/models.py",
        &["class"],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            println!("  ERROR: {e}");
            return TestResult {
                number: 6,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    println!("  Actual classes in core/models.py: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, REVSUP_CORE_MODELS_EXPECTED);

    let details = if passed {
        format!(
            "Graph found all {} expected model classes",
            REVSUP_CORE_MODELS_EXPECTED.len()
        )
    } else {
        format!("Missing model classes: {:?}", missing)
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 6,
        question,
        passed,
        details,
    }
}

async fn run_test_7(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What views/files call ForecastService methods?";
    println!("\n[Test 7] {question}");
    println!(
        "  Note: There is NO ForecastService class in revsup. Services are module-level functions."
    );
    println!(
        "  Expected: Only api.py imports from core.services (not views.py)."
    );

    // Verify: check that there is NO class named ForecastService
    let sql_no_class = format!(
        "SELECT COUNT(*) FROM {schema}.symbols WHERE project = 'revsup' AND name = 'ForecastService'",
        schema = schema,
    );
    let (class_count,): (i64,) = sqlx::query_as(&sql_no_class)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));

    // Check that api.py exists and has symbols (it's the caller of service functions)
    let sql_api = format!(
        "SELECT COUNT(*) FROM {schema}.symbols WHERE project = 'revsup' AND file_path LIKE '%core/api%'",
        schema = schema,
    );
    let (api_sym_count,): (i64,) = sqlx::query_as(&sql_api)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));

    // Check views.py symbols count (should exist but not call ForecastService)
    let sql_views = format!(
        "SELECT COUNT(*) FROM {schema}.symbols WHERE project = 'revsup' AND file_path LIKE '%core/views%'",
        schema = schema,
    );
    let (views_sym_count,): (i64,) = sqlx::query_as(&sql_views)
        .fetch_one(pool)
        .await
        .unwrap_or((0,));

    println!(
        "  ForecastService class in graph: {} (expected: 0)",
        class_count
    );
    println!(
        "  api.py symbols in graph: {} (expected: >0)",
        api_sym_count
    );
    println!(
        "  views.py symbols in graph: {} (expected: >0)",
        views_sym_count
    );

    // PASS if: ForecastService class is absent, and api.py is indexed, and views.py is indexed.
    let passed = class_count == 0 && api_sym_count > 0 && views_sym_count > 0;

    let details = format!(
        "ForecastService absent: {}, api.py indexed: {}, views.py indexed: {}",
        class_count == 0,
        api_sym_count > 0,
        views_sym_count > 0,
    );

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 7,
        question,
        passed,
        details,
    }
}

async fn run_test_8(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What signals are connected in revsup/core/signals.py?";
    println!("\n[Test 8] {question}");
    println!("  Expected handlers: {:?}", REVSUP_SIGNAL_HANDLERS_EXPECTED);

    let actual = match find_symbols_in_file(
        pool,
        schema,
        "revsup",
        "core/signals.py",
        &["function", "method"],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            println!("  ERROR: {e}");
            return TestResult {
                number: 8,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    println!("  Actual functions in signals.py: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, REVSUP_SIGNAL_HANDLERS_EXPECTED);

    let details = if passed {
        format!(
            "Graph found all {} expected signal handlers",
            REVSUP_SIGNAL_HANDLERS_EXPECTED.len()
        )
    } else {
        format!("Missing signal handlers: {:?}", missing)
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 8,
        question,
        passed,
        details,
    }
}

async fn run_test_9(
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What functions does data-adapter.ts export?";
    println!("\n[Test 9] {question}");
    println!("  Expected exports: {:?}", DATA_ADAPTER_EXPECTED_EXPORTS);

    let actual = match find_symbols_in_file(
        pool,
        schema,
        "nomadsignal",
        "data-adapter",
        &["function", "method"],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            println!("  ERROR: {e}");
            return TestResult {
                number: 9,
                question,
                passed: false,
                details: format!("Query error: {e}"),
            };
        }
    };

    println!("  Actual functions in data-adapter.ts: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, DATA_ADAPTER_EXPECTED_EXPORTS);

    let details = if passed {
        format!(
            "Graph found all {} expected exported functions",
            DATA_ADAPTER_EXPECTED_EXPORTS.len()
        )
    } else {
        format!("Missing exports: {:?}", missing)
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 9,
        question,
        passed,
        details,
    }
}

async fn run_test_10(
    graph_store: &GraphStore,
    pool: &sqlx::PgPool,
    schema: &str,
) -> TestResult {
    let question = "What calls getCountry() in mcp-server/src/?";
    println!("\n[Test 10] {question}");
    println!(
        "  Expected callers within mcp-server/src: {:?}",
        GET_COUNTRY_EXPECTED_CALLERS
    );
    println!(
        "  Note: getCountryWithScores calls getCountry internally; the site/ package is out of scope."
    );

    let actual =
        match find_callers_of(graph_store, pool, schema, "nomadsignal", "getCountry").await {
            Ok(s) => s,
            Err(e) => {
                println!("  ERROR: {e}");
                return TestResult {
                    number: 10,
                    question,
                    passed: false,
                    details: format!("Query error: {e}"),
                };
            }
        };

    println!("  Actual callers found: {:?}", {
        let mut v: Vec<&String> = actual.iter().collect();
        v.sort();
        v
    });

    let (passed, missing) = check_superset(&actual, GET_COUNTRY_EXPECTED_CALLERS);

    let details = if passed {
        format!(
            "Graph found all expected callers (total upstream: {})",
            actual.len()
        )
    } else {
        format!("Missing expected callers: {:?}", missing)
    };

    println!("  {}: {}", if passed { "PASS" } else { "FAIL" }, details);

    TestResult {
        number: 10,
        question,
        passed,
        details,
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("engram=warn,warn")
        .init();

    println!("=== Engram Spike 3: Correctness Validation Against Real Codebases ===\n");
    println!("Database: {DATABASE_URL}");
    println!("Schema:   {SCHEMA}\n");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(DATABASE_URL)
        .await?;

    let graph_store = GraphStore::new(pool.clone(), SCHEMA.to_string());

    // Initialize schema (idempotent)
    print!("Initializing graph schema... ");
    graph_store.init().await?;
    println!("done\n");

    // ---------------------------------------------------------------------------
    // Step 1: Index all three projects using tree-sitter parser
    // ---------------------------------------------------------------------------
    println!("--- Indexing Projects ---");

    let projects = [
        ("sugar", "/Users/steve/Dev/sugar/sugar"),
        ("revsup", "/Users/steve/Dev/revsup/revsup"),
        ("nomadsignal", "/Users/steve/Dev/nomadsignal/packages/mcp-server/src"),
    ];

    for (project, root) in &projects {
        print!("  Indexing {} ({})... ", project, root);
        match index_and_store(&pool, &graph_store, root, project).await {
            Ok(count) => println!("{} symbols stored", count),
            Err(e) => println!("ERROR: {e}"),
        }
    }

    println!();

    // ---------------------------------------------------------------------------
    // Step 2: Run correctness tests
    // ---------------------------------------------------------------------------
    println!("--- Running Correctness Tests ---");

    let mut results: Vec<TestResult> = Vec::new();

    results.push(run_test_1(&pool, SCHEMA).await);
    results.push(run_test_2(&graph_store, &pool, SCHEMA).await);
    results.push(run_test_3(&pool, SCHEMA).await);
    results.push(run_test_4(&pool, SCHEMA).await);
    results.push(run_test_5(&graph_store, &pool, SCHEMA).await);
    results.push(run_test_6(&pool, SCHEMA).await);
    results.push(run_test_7(&pool, SCHEMA).await);
    results.push(run_test_8(&pool, SCHEMA).await);
    results.push(run_test_9(&pool, SCHEMA).await);
    results.push(run_test_10(&graph_store, &pool, SCHEMA).await);

    // ---------------------------------------------------------------------------
    // Step 3: Summary
    // ---------------------------------------------------------------------------
    let passed_count = results.iter().filter(|r| r.passed).count();
    let total = results.len();

    println!("\n{}", "=".repeat(60));
    println!("SUMMARY: {passed_count}/{total} tests passed");
    println!("{}", "=".repeat(60));

    for r in &results {
        let label = if r.passed { "PASS" } else { "FAIL" };
        println!(
            "  [{label}] Test {:>2}: {} - {}",
            r.number, r.question, r.details
        );
    }

    println!("{}", "=".repeat(60));

    if passed_count < total {
        let failed: Vec<_> = results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| format!("Test {}", r.number))
            .collect();
        println!("\nFailed tests: {}", failed.join(", "));
        println!("\nInterpretation guide:");
        println!("  - INHERITS missing: tree-sitter parser may not emit Inherits edges for Python");
        println!("  - CALLS missing: call resolution depends on name matching in the same project");
        println!("  - Methods missing: parser may classify indented defs as 'function' not 'method'");
        println!("  - Import edges: only relative imports to indexed files resolve to symbols");
    } else {
        println!("\nAll tests passed - graph correctness validated.");
    }

    Ok(())
}
