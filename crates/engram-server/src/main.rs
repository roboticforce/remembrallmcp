use anyhow::Result;
use clap::{Parser, Subcommand};

mod config;
use config::EngramConfig;

#[derive(Parser)]
#[command(name = "engram", about = "Knowledge memory layer for AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the MCP server (default when no subcommand given)
    Serve,

    /// Set up Engram (database, config, embedding model)
    Init {
        /// Connect to an existing Postgres instead of using Docker
        #[arg(long)]
        database_url: Option<String>,

        /// Port for the Docker Postgres container (default: 5450)
        #[arg(long, default_value = "5450")]
        port: u16,
    },

    /// Start the Docker database container
    Start,

    /// Stop the Docker database container
    Stop,

    /// Show Engram status (database, memories, schema)
    Status,

    /// Check for common problems
    Doctor,

    /// Reset all data (requires confirmation)
    Reset {
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },

    /// Print version information
    Version,

    /// Watch project directories and auto-reindex on file changes
    Watch {
        /// Directories to watch (can specify multiple)
        #[arg(required = true)]
        paths: Vec<String>,

        /// Project name override (used when only one path is given; otherwise
        /// the directory basename is used for each path)
        #[arg(long)]
        project: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Serve) => cmd_serve().await,
        Some(Commands::Init { database_url, port }) => cmd_init(database_url, port).await,
        Some(Commands::Start) => cmd_start().await,
        Some(Commands::Stop) => cmd_stop().await,
        Some(Commands::Status) => cmd_status().await,
        Some(Commands::Doctor) => cmd_doctor().await,
        Some(Commands::Reset { force }) => cmd_reset(force).await,
        Some(Commands::Version) => cmd_version(),
        Some(Commands::Watch { paths, project }) => cmd_watch(paths, project).await,
    }
}

/// Run the MCP server (the default behavior - no args or `serve` subcommand).
async fn cmd_serve() -> Result<()> {
    // Log to stderr - stdout is reserved for MCP protocol messages.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting Engram MCP server");

    let config = EngramConfig::load();
    let server =
        engram_server::EngramServer::from_config(&config.database.url, &config.database.schema)
            .await?;

    use rmcp::{ServiceExt, transport::stdio};
    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {:?}", e))?;

    service.waiting().await?;
    Ok(())
}

/// Initialize Engram - set up database, schema, and embedding model.
async fn cmd_init(database_url: Option<String>, port: u16) -> Result<()> {
    println!("Setting up Engram...\n");

    let mut config = EngramConfig::default();

    if let Some(url) = database_url {
        // BYO Postgres path
        config.mode = "external".to_string();
        config.database.url = url;

        println!("Connecting to database...");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&config.database.url)
            .await?;

        println!("Checking pgvector extension...");
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&pool)
            .await?;

        println!("Creating schema...");
        let memory_store = engram_core::memory::store::MemoryStore::new(
            pool.clone(),
            config.database.schema.clone(),
        );
        memory_store.init().await?;
        let graph_store = engram_core::graph::store::GraphStore::new(
            pool.clone(),
            config.database.schema.clone(),
        );
        graph_store.init().await?;

        println!("Database ready.\n");
    } else {
        // Docker path
        config.mode = "local".to_string();
        config.docker.port = port;
        config.database.url =
            format!("postgres://postgres:postgres@localhost:{}/engram", port);

        println!("Checking Docker...");
        let docker_ok = std::process::Command::new("docker")
            .args(["info"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !docker_ok {
            eprintln!("Docker is not running or not installed.");
            eprintln!("Options:");
            eprintln!("  1. Install Docker: https://docker.com/get-started");
            eprintln!(
                "  2. Use existing Postgres: engram init --database-url postgres://..."
            );
            std::process::exit(1);
        }

        // Check if container already exists
        let exists = std::process::Command::new("docker")
            .args(["inspect", &config.docker.container_name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !exists {
            println!("Pulling {}...", config.docker.image);
            let pull = std::process::Command::new("docker")
                .args(["pull", &config.docker.image])
                .status()?;
            if !pull.success() {
                anyhow::bail!("Failed to pull Docker image");
            }

            println!("Starting database container...");
            let run = std::process::Command::new("docker")
                .args([
                    "run",
                    "-d",
                    "--name",
                    &config.docker.container_name,
                    "-e",
                    "POSTGRES_PASSWORD=postgres",
                    "-p",
                    &format!("{}:5432", port),
                    "-v",
                    "engram-db-data:/var/lib/postgresql/data",
                    &config.docker.image,
                ])
                .status()?;
            if !run.success() {
                anyhow::bail!("Failed to start Docker container");
            }
        } else {
            // Container exists - make sure it's running
            let _ = std::process::Command::new("docker")
                .args(["start", &config.docker.container_name])
                .status()?;
        }

        // Wait for Postgres to be ready
        println!("Waiting for database...");
        for i in 0..30 {
            let ready = std::process::Command::new("docker")
                .args([
                    "exec",
                    &config.docker.container_name,
                    "pg_isready",
                    "-U",
                    "postgres",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ready {
                break;
            }
            if i == 29 {
                anyhow::bail!("Database failed to start after 30 seconds");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        // Create the engram database if it doesn't exist
        let _ = std::process::Command::new("docker")
            .args([
                "exec",
                &config.docker.container_name,
                "psql",
                "-U",
                "postgres",
                "-c",
                "CREATE DATABASE engram;",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Connect and init schema
        println!("Initializing schema...");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&config.database.url)
            .await?;

        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&pool)
            .await?;

        let memory_store = engram_core::memory::store::MemoryStore::new(
            pool.clone(),
            config.database.schema.clone(),
        );
        memory_store.init().await?;
        let graph_store = engram_core::graph::store::GraphStore::new(
            pool.clone(),
            config.database.schema.clone(),
        );
        graph_store.init().await?;

        println!("Database ready.\n");
    }

    // Pre-download embedding model
    println!("Downloading embedding model...");
    let _ = tokio::task::spawn_blocking(|| engram_core::embed::FastEmbedder::new()).await?;
    println!("Model ready.\n");

    // Save config
    config.save()?;
    println!(
        "Config saved to {}\n",
        EngramConfig::config_path().display()
    );

    println!("Engram is ready! Add this to your project's .mcp.json:\n");
    println!(
        r#"{{
  "mcpServers": {{
    "engram": {{
      "command": "engram"
    }}
  }}
}}"#
    );
    println!();

    Ok(())
}

async fn cmd_start() -> Result<()> {
    let config = EngramConfig::load();
    if config.mode != "local" {
        println!(
            "Database is managed externally (mode: {}). Nothing to start.",
            config.mode
        );
        return Ok(());
    }

    let status = std::process::Command::new("docker")
        .args(["start", &config.docker.container_name])
        .status()?;

    if status.success() {
        println!("Database started.");
    } else {
        eprintln!("Failed to start container. Run 'engram doctor' to diagnose.");
    }
    Ok(())
}

async fn cmd_stop() -> Result<()> {
    let config = EngramConfig::load();
    if config.mode != "local" {
        println!(
            "Database is managed externally (mode: {}). Nothing to stop.",
            config.mode
        );
        return Ok(());
    }

    let status = std::process::Command::new("docker")
        .args(["stop", &config.docker.container_name])
        .status()?;

    if status.success() {
        println!("Database stopped.");
    } else {
        eprintln!("Failed to stop container.");
    }
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let config = EngramConfig::load();
    let config_path = EngramConfig::config_path();

    println!("Engram Status\n");
    println!(
        "Config: {}",
        if config_path.exists() {
            config_path.display().to_string()
        } else {
            "not found".to_string()
        }
    );
    println!("Mode:   {}", config.mode);
    println!("Schema: {}", config.database.schema);

    if config.mode == "local" {
        let running = std::process::Command::new("docker")
            .args([
                "inspect",
                "-f",
                "{{.State.Running}}",
                &config.docker.container_name,
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        println!(
            "Docker: {} ({})",
            if running { "running" } else { "stopped" },
            config.docker.container_name
        );
    }

    match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&config.database.url)
        .await
    {
        Ok(pool) => {
            println!("Database: connected");

            let count: (i64,) = sqlx::query_as(&format!(
                "SELECT COUNT(*) FROM {}.memories",
                config.database.schema
            ))
            .fetch_one(&pool)
            .await
            .unwrap_or((0,));
            println!("Memories: {}", count.0);

            let syms: (i64,) = sqlx::query_as(&format!(
                "SELECT COUNT(*) FROM {}.symbols",
                config.database.schema
            ))
            .fetch_one(&pool)
            .await
            .unwrap_or((0,));
            println!("Symbols:  {}", syms.0);
        }
        Err(e) => {
            println!("Database: not reachable ({})", e);
        }
    }

    Ok(())
}

async fn cmd_doctor() -> Result<()> {
    println!("Engram Doctor\n");
    let config = EngramConfig::load();
    let mut issues = 0;

    // Check config file
    let config_path = EngramConfig::config_path();
    if config_path.exists() {
        println!("[OK] Config file: {}", config_path.display());
    } else {
        println!("[!!] Config file not found. Run 'engram init' first.");
        issues += 1;
    }

    // Check Docker (local mode only)
    if config.mode == "local" {
        let docker_ok = std::process::Command::new("docker")
            .args(["info"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if docker_ok {
            println!("[OK] Docker is running");
        } else {
            println!("[!!] Docker is not running or not installed");
            issues += 1;
        }

        let running = std::process::Command::new("docker")
            .args([
                "inspect",
                "-f",
                "{{.State.Running}}",
                &config.docker.container_name,
            ])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);

        if running {
            println!(
                "[OK] Container '{}' is running",
                config.docker.container_name
            );
        } else {
            println!(
                "[!!] Container '{}' is not running. Try 'engram start'",
                config.docker.container_name
            );
            issues += 1;
        }
    }

    // Check database connection
    match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&config.database.url)
        .await
    {
        Ok(pool) => {
            println!("[OK] Database connection");

            let has_vector: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'vector')",
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(false);

            if has_vector {
                println!("[OK] pgvector extension installed");
            } else {
                println!("[!!] pgvector extension not installed");
                issues += 1;
            }

            let has_schema: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = '{}')",
                config.database.schema
            ))
            .fetch_one(&pool)
            .await
            .unwrap_or(false);

            if has_schema {
                println!("[OK] Schema '{}' exists", config.database.schema);
            } else {
                println!(
                    "[!!] Schema '{}' not found. Run 'engram init'",
                    config.database.schema
                );
                issues += 1;
            }
        }
        Err(e) => {
            println!("[!!] Cannot connect to database: {}", e);
            issues += 1;
        }
    }

    // Check embedding model cache
    let model_cached_hf = dirs::home_dir()
        .map(|h| {
            h.join(".cache/huggingface/hub/models--Qdrant--all-MiniLM-L6-v2-onnx")
                .exists()
        })
        .unwrap_or(false);
    let model_cached_fe =
        std::path::Path::new(".fastembed_cache/models--Qdrant--all-MiniLM-L6-v2-onnx").exists();

    if model_cached_hf || model_cached_fe {
        println!("[OK] Embedding model cached");
    } else {
        println!("[!!] Embedding model not downloaded. Run 'engram init'");
        issues += 1;
    }

    println!();
    if issues == 0 {
        println!("All checks passed.");
    } else {
        println!("{} issue(s) found.", issues);
    }

    Ok(())
}

async fn cmd_reset(force: bool) -> Result<()> {
    if !force {
        eprintln!("This will delete ALL memories and code graph data.");
        eprintln!("Run with --force to confirm: engram reset --force");
        return Ok(());
    }

    let config = EngramConfig::load();

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&config.database.url)
        .await?;

    sqlx::query(&format!(
        "DROP SCHEMA IF EXISTS {} CASCADE",
        config.database.schema
    ))
    .execute(&pool)
    .await?;

    let memory_store = engram_core::memory::store::MemoryStore::new(
        pool.clone(),
        config.database.schema.clone(),
    );
    memory_store.init().await?;
    let graph_store = engram_core::graph::store::GraphStore::new(
        pool.clone(),
        config.database.schema.clone(),
    );
    graph_store.init().await?;

    println!("All data reset. Schema recreated.");
    Ok(())
}

async fn cmd_watch(paths: Vec<String>, project: Option<String>) -> Result<()> {
    // Set up logging to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let config = EngramConfig::load();

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&config.database.url)
        .await
        .map_err(|e| anyhow::anyhow!(
            "Cannot connect to database. Is Postgres running? Error: {}", e
        ))?;

    let graph = std::sync::Arc::new(
        engram_core::graph::store::GraphStore::new(pool, config.database.schema.clone()),
    );
    graph.init().await?;

    // Resolve each path to an absolute PathBuf and derive a project name.
    let mut project_dirs: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (i, raw_path) in paths.iter().enumerate() {
        let abs = std::path::Path::new(raw_path)
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("cannot resolve path '{}': {}", raw_path, e))?;

        let name = if paths.len() == 1 {
            // Single path: use --project override if given, else basename.
            project
                .clone()
                .unwrap_or_else(|| basename(&abs))
        } else {
            // Multiple paths: ignore --project (ambiguous) and use basename of each.
            if i == 0 && project.is_some() {
                tracing::warn!("--project is ignored when multiple paths are specified; using directory basenames");
            }
            basename(&abs)
        };

        project_dirs.push((abs, name));
    }

    // Initial full index of every registered project.
    for (root, proj) in &project_dirs {
        tracing::info!("initial index: {} (project={})", root.display(), proj);
        let root_clone = root.clone();
        let proj_clone = proj.clone();
        let index_result = tokio::task::spawn_blocking(move || {
            engram_core::parser::index_directory(&root_clone, &proj_clone, None)
        })
        .await??;

        for symbol in &index_result.symbols {
            if let Err(e) = graph.upsert_symbol(symbol).await {
                tracing::warn!("upsert_symbol failed: {e}");
            }
        }
        for rel in &index_result.relationships {
            if let Err(e) = graph.add_relationship(rel).await {
                tracing::debug!("skipping relationship: {e}");
            }
        }
        tracing::info!(
            "indexed {} - {} files, {} symbols, {} relationships",
            root.display(),
            index_result.files_parsed,
            index_result.symbols.len(),
            index_result.relationships.len(),
        );
    }

    // Build the watcher and register all project directories.
    let fw = engram_server::watcher::FileWatcher::new(std::sync::Arc::clone(&graph));
    for (root, proj) in project_dirs {
        fw.add_project(root, proj).await;
    }

    tracing::info!("watching for changes (press Ctrl+C to stop)");

    // Run the watcher loop. This blocks until Ctrl+C / process exit.
    fw.run().await;

    Ok(())
}

/// Return the last path component as a string, falling back to the full path.
fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("unknown"))
        .to_string()
}

fn cmd_version() -> Result<()> {
    println!("engram {}", env!("CARGO_PKG_VERSION"));
    println!("target: {}", std::env::consts::ARCH);
    println!("os:     {}", std::env::consts::OS);

    let config_path = EngramConfig::config_path();
    println!(
        "config: {}",
        if config_path.exists() {
            config_path.display().to_string()
        } else {
            "not configured".to_string()
        }
    );

    Ok(())
}
