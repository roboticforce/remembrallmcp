use anyhow::Result;
use clap::{Parser, Subcommand};

mod config;
use config::RemembrallConfig;

#[derive(Parser)]
#[command(name = "remembrall", about = "Knowledge memory layer for AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the MCP server (default when no subcommand given)
    Serve,

    /// Set up RemembrallMCP (database, config, embedding model)
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

    /// Show RemembrallMCP status (database, memories, schema)
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

    tracing::info!("Starting RemembrallMCP server");

    let config = RemembrallConfig::load();
    let server =
        remembrall_server::RemembrallServer::from_config(&config.database.url, &config.database.schema)
            .await?;

    use rmcp::{ServiceExt, transport::stdio};
    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {:?}", e))?;

    service.waiting().await?;
    Ok(())
}

/// Initialize RemembrallMCP - set up database, schema, and embedding model.
async fn cmd_init(database_url: Option<String>, port: u16) -> Result<()> {
    println!("Setting up RemembrallMCP...\n");

    let mut config = RemembrallConfig::default();

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
        let memory_store = remembrall_core::memory::store::MemoryStore::new(
            pool.clone(),
            config.database.schema.clone(),
        )?;
        memory_store.init().await?;
        let graph_store = remembrall_core::graph::store::GraphStore::new(
            pool.clone(),
            config.database.schema.clone(),
        )?;
        graph_store.init().await?;

        println!("Database ready.\n");
    } else {
        // Docker path
        config.mode = "local".to_string();
        config.docker.port = port;
        config.database.url =
            format!("postgres://postgres:postgres@localhost:{}/remembrall", port);

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
                "  2. Use existing Postgres: remembrall init --database-url postgres://..."
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
                    "remembrall-db-data:/var/lib/postgresql/data",
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

        // Create the remembrall database if it doesn't exist
        let _ = std::process::Command::new("docker")
            .args([
                "exec",
                &config.docker.container_name,
                "psql",
                "-U",
                "postgres",
                "-c",
                "CREATE DATABASE remembrall;",
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

        let memory_store = remembrall_core::memory::store::MemoryStore::new(
            pool.clone(),
            config.database.schema.clone(),
        )?;
        memory_store.init().await?;
        let graph_store = remembrall_core::graph::store::GraphStore::new(
            pool.clone(),
            config.database.schema.clone(),
        )?;
        graph_store.init().await?;

        println!("Database ready.\n");
    }

    // Pre-download embedding model
    println!("Downloading embedding model...");
    let _ = tokio::task::spawn_blocking(|| remembrall_core::embed::FastEmbedder::new()).await?;
    println!("Model ready.\n");

    // Save config
    config.save()?;
    println!(
        "Config saved to {}\n",
        RemembrallConfig::config_path().display()
    );

    println!("RemembrallMCP is ready! Add this to your project's .mcp.json:\n");
    println!(
        r#"{{
  "mcpServers": {{
    "remembrall": {{
      "command": "remembrall"
    }}
  }}
}}"#
    );
    println!();

    Ok(())
}

async fn cmd_start() -> Result<()> {
    let config = RemembrallConfig::load();
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
        eprintln!("Failed to start container. Run 'remembrall doctor' to diagnose.");
    }
    Ok(())
}

async fn cmd_stop() -> Result<()> {
    let config = RemembrallConfig::load();
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
    let config = RemembrallConfig::load();
    let config_path = RemembrallConfig::config_path();

    println!("RemembrallMCP Status\n");
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
    println!("RemembrallMCP Doctor\n");
    let config = RemembrallConfig::load();
    let mut issues = 0;

    // Check config file
    let config_path = RemembrallConfig::config_path();
    if config_path.exists() {
        println!("[OK] Config file: {}", config_path.display());
    } else {
        println!("[!!] Config file not found. Run 'remembrall init' first.");
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
                "[!!] Container '{}' is not running. Try 'remembrall start'",
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

            let has_schema: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = $1)",
            )
            .bind(&config.database.schema)
            .fetch_one(&pool)
            .await
            .unwrap_or(false);

            if has_schema {
                println!("[OK] Schema '{}' exists", config.database.schema);
            } else {
                println!(
                    "[!!] Schema '{}' not found. Run 'remembrall init'",
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
        println!("[!!] Embedding model not downloaded. Run 'remembrall init'");
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
        eprintln!("Run with --force to confirm: remembrall reset --force");
        return Ok(());
    }

    let config = RemembrallConfig::load();

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

    let memory_store = remembrall_core::memory::store::MemoryStore::new(
        pool.clone(),
        config.database.schema.clone(),
    )?;
    memory_store.init().await?;
    let graph_store = remembrall_core::graph::store::GraphStore::new(
        pool.clone(),
        config.database.schema.clone(),
    )?;
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

    // Validate the whole replacement scope before connecting or publishing any graph.
    let project_dirs = resolve_watch_projects(&paths, project.as_deref())?;
    if project_dirs.len() > 1 && project.is_some() {
        tracing::warn!("--project is ignored for multiple distinct roots; using directory basenames");
    }

    let config = RemembrallConfig::load();

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&config.database.url)
        .await
        .map_err(|e| anyhow::anyhow!(
            "Cannot connect to database. Is Postgres running? Error: {}", e
        ))?;

    let graph = std::sync::Arc::new(
        remembrall_core::graph::store::GraphStore::new(pool, config.database.schema.clone())?,
    );
    graph.init().await?;

    // Initial full index of every registered project.
    for (root, proj) in &project_dirs {
        tracing::info!("initial index: {} (project={})", root.display(), proj);
        let index_result = remembrall_core::graph::publication::refresh_project(&graph, root, proj).await?;
        tracing::info!(
            "indexed {} - {} files, {} symbols, {} relationships",
            root.display(),
            index_result.files_parsed,
            index_result.symbols_stored,
            index_result.relationships_stored,
        );
    }

    // Build the watcher and register all project directories.
    let fw = remembrall_server::watcher::FileWatcher::new(std::sync::Arc::clone(&graph));
    for (root, proj) in project_dirs {
        fw.add_project(root, proj).await?;
    }

    tracing::info!("watching for changes (press Ctrl+C to stop)");

    // Run the watcher loop. This blocks until Ctrl+C / process exit.
    fw.run().await;

    Ok(())
}

/// Resolve every requested root before performing any graph side effects.
fn resolve_watch_projects(paths: &[String], project: Option<&str>) -> Result<Vec<(std::path::PathBuf, String)>> {
    anyhow::ensure!(!paths.is_empty(), "watch requires at least one directory root");
    if let Some(name) = project {
        anyhow::ensure!(!name.trim().is_empty(), "project name must not be empty");
    }
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in paths {
        anyhow::ensure!(!raw.trim().is_empty(), "watch directory root must not be empty");
        let root = std::path::Path::new(raw).canonicalize()
            .map_err(|error| anyhow::anyhow!("cannot resolve watch root '{}': {}", raw, error))?;
        anyhow::ensure!(root.is_dir(), "watch root '{}' is not a directory", root.display());
        if seen.insert(root.clone()) { roots.push(root); }
    }
    let single = roots.len() == 1;
    let mut owners = std::collections::HashMap::new();
    let mut resolved = Vec::new();
    for root in roots {
        let name = if single { project.map(str::to_owned).unwrap_or_else(|| basename(&root)) }
            else { basename(&root) };
        anyhow::ensure!(!name.trim().is_empty(), "watch project name must not be empty");
        if let Some(previous) = owners.insert(name.clone(), root.clone()) {
            anyhow::bail!("watch project '{}' has conflicting roots '{}' and '{}'; run each root separately with distinct explicit --project names", name, previous.display(), root.display());
        }
        resolved.push((root, name));
    }
    Ok(resolved)
}

/// Return the last path component as a string, falling back to the full path.
fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("unknown"))
        .to_string()
}

fn cmd_version() -> Result<()> {
    println!("remembrall {}", env!("CARGO_PKG_VERSION"));
    println!("target: {}", std::env::consts::ARCH);
    println!("os:     {}", std::env::consts::OS);

    let config_path = RemembrallConfig::config_path();
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

#[cfg(test)]
mod watch_scope_tests {
    use super::resolve_watch_projects;
    use std::path::PathBuf;

    struct Roots(PathBuf);
    impl Roots {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("watch-scope-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn directory(&self, name: &str) -> String {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.to_string_lossy().into_owned()
        }
    }
    impl Drop for Roots {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn duplicate_basenames_refused_before_any_database_effects() {
        let roots = Roots::new();
        let a = roots.directory("a/src");
        let b = roots.directory("b/src");
        let error = resolve_watch_projects(&[a.clone(), b.clone()], None).unwrap_err().to_string();
        assert!(error.contains("conflicting roots"));
        assert!(error.contains("--project"));
        assert!(error.contains("a/src") && error.contains("b/src"));
        assert!(resolve_watch_projects(&[a, b], Some("ignored-for-multiple-roots")).is_err());
    }

    #[test]
    fn distinct_roots_keep_distinct_projects() {
        let roots = Roots::new();
        let result = resolve_watch_projects(&[roots.directory("first"), roots.directory("second")], None).unwrap();
        assert_eq!(result.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(), ["first", "second"]);
    }

    #[test]
    fn repeated_canonical_root_deduplicated_and_explicit_name_preserved() {
        let roots = Roots::new();
        let path = roots.directory("source");
        let result = resolve_watch_projects(&[path.clone(), format!("{path}/."), path], Some("explicit-owner")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "explicit-owner");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_is_one_canonical_root() {
        let roots = Roots::new();
        let path = roots.directory("source");
        let alias = roots.0.join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let result = resolve_watch_projects(&[path, alias.to_string_lossy().into_owned()], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "source");
    }

    #[test]
    fn explicit_name_applies_to_single_root() {
        let roots = Roots::new();
        let result = resolve_watch_projects(&[roots.directory("src")], Some("service-a")).unwrap();
        assert_eq!(result[0].1, "service-a");
    }

    #[test]
    fn empty_missing_and_file_roots_are_refused() {
        let roots = Roots::new();
        let file = roots.0.join("file.ts");
        std::fs::write(&file, "export const a = 1;").unwrap();
        assert!(resolve_watch_projects(&[], None).is_err());
        assert!(resolve_watch_projects(&["".into()], None).is_err());
        assert!(resolve_watch_projects(&[roots.0.join("missing").to_string_lossy().into_owned()], None).is_err());
        assert!(resolve_watch_projects(&[file.to_string_lossy().into_owned()], None).is_err());
        assert!(resolve_watch_projects(&[roots.directory("valid")], Some(" ")).is_err());
    }
}
