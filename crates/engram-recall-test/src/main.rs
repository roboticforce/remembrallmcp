//! Engram recall test harness.
//!
//! Validates engram_recall search quality against a ground truth TOML file.
//!
//! Usage:
//!   engram-recall-test \
//!     --database-url postgres://postgres:postgres@localhost:5450/engram \
//!     --seed /path/to/seed_memories.toml \
//!     --ground-truth /path/to/ground_truth.toml \
//!     [--schema engram_test] \
//!     [--no-seed]   # skip seeding if data is already present
//!
//! Environment variables:
//!   DATABASE_URL  - overrides --database-url if set
//!   TEST_SCHEMA   - overrides --schema if set (default: engram_test)
//!
//! Exit codes:
//!   0 - all assertions passed
//!   1 - one or more assertions failed
//!   2 - setup/configuration error

mod ground_truth;
mod report;
mod runner;
mod scorer;
mod seed;

use std::path::PathBuf;
use std::process;

use anyhow::{Context, Result};
use engram_core::embed::FastEmbedder;
use engram_core::memory::store::MemoryStore;
use scorer::AggregateScores;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "engram_recall_test=info,engram_core=warn".into()),
        )
        .init();

    match run().await {
        Ok(exit_code) => process::exit(exit_code),
        Err(e) => {
            eprintln!("error: {e:#}");
            process::exit(2);
        }
    }
}

async fn run() -> Result<i32> {
    let args = parse_args()?;

    // Resolve DATABASE_URL
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| args.database_url.clone());
    let schema = std::env::var("TEST_SCHEMA").unwrap_or_else(|_| args.schema.clone());

    tracing::info!("Connecting to database...");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .context("connecting to Postgres")?;

    // Initialize memory store
    let store = MemoryStore::new(pool.clone(), schema.clone());
    store.init().await.context("initializing memory store")?;

    // Load embedding model
    tracing::info!("Loading embedding model (all-MiniLM-L6-v2)...");
    let embedder = FastEmbedder::new().context("loading FastEmbedder")?;

    // Seed data
    if !args.no_seed {
        tracing::info!("Seeding test memories from {}...", args.seed_path.display());
        let seed_raw = std::fs::read_to_string(&args.seed_path)
            .with_context(|| format!("reading {}", args.seed_path.display()))?;
        let seed_file: seed::SeedFile =
            toml::from_str(&seed_raw).context("parsing seed TOML")?;

        let count = seed::seed(&store, &pool, &schema, &embedder, &seed_file)
            .await
            .context("seeding test data")?;
        tracing::info!("Seeded {} memories", count.len());
    } else {
        tracing::info!("Skipping seed (--no-seed)");
    }

    // Load ground truth
    tracing::info!(
        "Loading ground truth from {}...",
        args.ground_truth_path.display()
    );
    let gt_raw = std::fs::read_to_string(&args.ground_truth_path)
        .with_context(|| format!("reading {}", args.ground_truth_path.display()))?;
    let gt: ground_truth::GroundTruth =
        toml::from_str(&gt_raw).context("parsing ground truth TOML")?;

    tracing::info!("Running {} queries...", gt.queries.len());

    // Filter by category if specified
    let queries_to_run: Vec<&ground_truth::QueryCase> = match &args.category_filter {
        Some(cat) => gt.queries.iter().filter(|q| q.category == *cat).collect(),
        None => gt.queries.iter().collect(),
    };

    // Run each query
    let mut per_query: Vec<scorer::QueryScore> = Vec::new();

    for case in &queries_to_run {
        tracing::debug!("running query {} - {}", case.id, case.description);
        let score = runner::run_query(case, &store, &embedder)
            .await
            .with_context(|| format!("running query {}", case.id))?;

        let status = if score.passed { "PASS" } else { "FAIL" };
        tracing::info!(
            "[{}] {} R@5={:.2} P@5={:.2} MRR={:.2} {}ms",
            status,
            case.id,
            score.recall_at_5,
            score.precision_at_5,
            score.mrr,
            score.latency_ms,
        );
        per_query.push(score);
    }

    let agg = AggregateScores::compute(&per_query);
    report::print_report(&gt, &per_query, &agg);

    let any_failed = per_query.iter().any(|q| !q.passed);
    Ok(if any_failed { 1 } else { 0 })
}

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

struct Args {
    database_url: String,
    schema: String,
    seed_path: PathBuf,
    ground_truth_path: PathBuf,
    no_seed: bool,
    category_filter: Option<String>,
}

fn parse_args() -> Result<Args> {
    let args: Vec<String> = std::env::args().collect();

    let mut database_url = String::from("postgres://postgres:postgres@localhost:5450/engram");
    let mut schema = String::from("engram_test");
    let mut seed_path: Option<PathBuf> = None;
    let mut ground_truth_path: Option<PathBuf> = None;
    let mut no_seed = false;
    let mut category_filter: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--database-url" => {
                i += 1;
                database_url = args
                    .get(i)
                    .context("--database-url requires a value")?
                    .clone();
            }
            "--schema" => {
                i += 1;
                schema = args.get(i).context("--schema requires a value")?.clone();
            }
            "--seed" => {
                i += 1;
                seed_path = Some(PathBuf::from(
                    args.get(i).context("--seed requires a value")?,
                ));
            }
            "--ground-truth" => {
                i += 1;
                ground_truth_path = Some(PathBuf::from(
                    args.get(i).context("--ground-truth requires a value")?,
                ));
            }
            "--no-seed" => {
                no_seed = true;
            }
            "--category" => {
                i += 1;
                category_filter =
                    Some(args.get(i).context("--category requires a value")?.clone());
            }
            other => {
                anyhow::bail!("Unknown argument: {other}");
            }
        }
        i += 1;
    }

    Ok(Args {
        database_url,
        schema,
        seed_path: seed_path.unwrap_or_else(|| {
            // Default to the tests/recall directory relative to crate root
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("tests/recall/seed_memories.toml")
        }),
        ground_truth_path: ground_truth_path.unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("tests/recall/ground_truth.toml")
        }),
        no_seed,
        category_filter,
    })
}
