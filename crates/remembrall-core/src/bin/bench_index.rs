//! Index a directory into RemembrallMCP for benchmarking.
//! Usage: bench_index /path/to/project project_name

use remembrall_core::graph::store::GraphStore;
use remembrall_core::parser::index_directory;
use sqlx::postgres::PgPoolOptions;
use std::time::Instant;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: bench_index <path> <project_name>");
        std::process::exit(1);
    }
    let path = &args[1];
    let project = &args[2];

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5450/remembrall".into());
    let schema = std::env::var("REMEMBRALL_SCHEMA").unwrap_or_else(|_| "remembrall".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    let graph = GraphStore::new(pool, schema)?;
    graph.init().await?;

    println!("Indexing {path} as project '{project}'...");
    let start = Instant::now();
    let result = index_directory(path, project, None)?;
    let parse_time = start.elapsed();

    println!(
        "Parsed in {:.1}s: {} symbols, {} relationships",
        parse_time.as_secs_f64(),
        result.symbols.len(),
        result.relationships.len()
    );

    let store_start = Instant::now();
    for symbol in &result.symbols {
        graph.upsert_symbol(symbol).await?;
    }
    let mut rels_stored = 0u64;
    for rel in &result.relationships {
        if graph.add_relationship(rel).await.is_ok() {
            rels_stored += 1;
        }
    }
    println!("Relationships stored: {rels_stored}/{}", result.relationships.len());
    let store_time = store_start.elapsed();

    println!("Stored in {:.1}s. Done.", store_time.as_secs_f64());
    Ok(())
}
