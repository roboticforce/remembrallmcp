# RemembrallMCP

Knowledge memory layer for AI agents. Rust core, Postgres + pgvector backend, MCP protocol.

## Quick Reference

- **Language:** Rust 1.94+, edition 2024
- **Workspace:** `crates/remembrall-core` (library), `crates/remembrall-server` (MCP server + CLI)
- **Database:** `postgres://postgres:postgres@localhost:5450/remembrall` (Docker: `cocoindex-postgres`)
- **Schema:** `remembrall` (configurable via `REMEMBRALL_SCHEMA`)
- **Architecture doc:** `docs/architecture.md`

## Build & Run

```bash
# Start database
docker start cocoindex-postgres

# Build everything
cargo build

# Build MCP server + CLI (release)
cargo build -p remembrall-server --release

# Run MCP server manually (stdio)
DATABASE_URL="postgres://postgres:postgres@localhost:5450/remembrall" ./target/release/remembrall

# Run ground truth tests (validates everything works)
DATABASE_URL="postgres://postgres:postgres@localhost:5450/remembrall" cargo run --bin spike3
```

## CLI Commands

```bash
remembrall init                          # Set up Docker DB, schema, embedding model, write config
remembrall init --database-url <url>     # Init with existing Postgres
remembrall serve                         # Run MCP server (explicit form; no-arg is same)
remembrall start                         # Start Docker database container
remembrall stop                          # Stop Docker database container
remembrall status                        # Memory count, symbol count, connection status
remembrall doctor                        # Check Docker, pgvector, schema, and model cache
remembrall reset --force                 # Drop and recreate schema (deletes all data)
remembrall version                       # Print version, arch, OS, and config path
```

## MCP Server

Binary: `target/release/remembrall`. Configured in `.mcp.json` for Claude Code.

**Tools (9 total):**
- `remembrall_store` - store decisions, patterns, knowledge
- `remembrall_recall` - hybrid semantic + full-text search
- `remembrall_update` - partial update of an existing memory
- `remembrall_delete` - remove a memory by UUID
- `remembrall_ingest_github` - bulk-import merged PRs from a GitHub repo (via `gh` CLI)
- `remembrall_ingest_docs` - ingest markdown files from a directory
- `remembrall_impact` - blast radius analysis on code symbols
- `remembrall_lookup_symbol` - find where a function/class is defined
- `remembrall_index` - index a project directory to build the code graph

**Embedding:** fastembed (ONNX Runtime, all-MiniLM-L6-v2, 384-dim). In-process, no external API. Model downloads on first run (~23 MB), or pre-downloaded by `remembrall init`.

## Project Structure

```
crates/
  remembrall-core/src/
    memory/store.rs      # MemoryStore - CRUD + semantic/fulltext/hybrid search
    memory/types.rs      # Memory, Source, Scope, MemoryType enums
    graph/store.rs       # GraphStore - symbols, relationships, impact analysis (recursive CTEs)
    graph/types.rs       # Symbol, Relationship, ImpactResult, Direction
    parser/python.rs     # Tree-sitter Python parser
    parser/typescript.rs # Tree-sitter TypeScript/JS parser
    parser/rust.rs       # Tree-sitter Rust parser
    parser/go.rs         # Tree-sitter Go parser
    parser/java.rs       # Tree-sitter Java parser
    parser/ruby.rs       # Tree-sitter Ruby parser
    parser/kotlin.rs     # Tree-sitter Kotlin parser
    parser/walker.rs     # Directory walker + two-phase cross-file resolution
    indexer.rs           # Incremental indexer with mtime tracking + CodeParser trait
    config.rs            # Config from env vars
    embed.rs             # Embedder trait + FastEmbedder (fastembed/ONNX, 384-dim)
    search.rs            # Hybrid search stub (logic lives in memory/store.rs)
    ingest.rs            # GitHub PR and markdown docs ingestion (reusable without MCP)
  remembrall-server/
    src/lib.rs           # MCP server - RemembrallServer struct + thin #[tool] wrappers
    src/tools/memory.rs  # Store, recall, update, delete implementation
    src/tools/graph.rs   # Index, impact, lookup_symbol implementation
    src/tools/ingest.rs  # GitHub and docs ingestion tool wrappers
    src/main.rs          # CLI entry point (init, serve, start, stop, status, doctor, reset, version)
    src/config.rs        # RemembrallConfig - loads ~/.remembrall/config.toml with env var overrides
  remembrall-python/         # PyO3 bindings (deferred)
install.sh               # curl installer script
dist/                    # Prebuilt release binaries
```

## Config File

`~/.remembrall/config.toml` - written by `remembrall init`, loaded by all subcommands. Env vars override:
- `REMEMBRALL_DATABASE_URL` or `DATABASE_URL` overrides `database.url`
- `REMEMBRALL_SCHEMA` overrides `database.schema`

## Database Tables (all in `remembrall` schema)

- `memories` - text knowledge with pgvector embeddings, scope, tags, fingerprint dedup
- `symbols` - code symbols (File, Function, Class, Method) with file/line/language/project
- `relationships` - edges (Calls, Imports, Defines, Inherits) with confidence scores
- `file_index` - mtime tracking for incremental reindexing

## Key Patterns

- **Two-phase resolution:** Parser collects all files first, then resolves imports and cross-file calls against the full symbol set
- **Impact analysis:** Recursive CTEs with cycle detection, confidence decay through the chain
- **Incremental indexing:** Compare disk mtime vs stored mtime, only reparse changed files
- **Content fingerprinting:** Normalized hash for memory deduplication
- **Contradiction detection:** `remembrall_store` searches at 0.75 similarity before storing; near-duplicates are returned in the response
- **Ingestion:** `remembrall_ingest_github` shells to `gh` CLI; `remembrall_ingest_docs` walks directories for `.md` files, splits on H2 headers

## Branching & Releases

Gitflow model:
- `develop` - default branch, all PRs target here
- `main` - releases only, merges from develop
- `feature/*` - feature branches off develop
- `hotfix/*` - urgent fixes off main

### Cutting a release

```bash
# 1. Ensure develop is clean and CI passes
git checkout develop
cargo test --workspace

# 2. Merge to main
git checkout main
git merge develop
git push origin main

# 3. Tag and push (triggers release pipeline)
git tag v0.X.0
git push origin v0.X.0

# 4. Bump develop to next dev version
git checkout develop
# Update version in Cargo.toml if needed
```

### What the release pipeline does (on v* tag)

1. Builds macOS arm64 + Linux x86_64 release binaries
2. Creates GitHub Release with downloadable tarballs
3. Publishes `remembrall-core` + `remembrall-server` to crates.io
4. Builds and pushes Docker image to `cdnsteve/remembrallmcp:<version>` + `:latest`

### Required GitHub secrets

| Secret | Source |
|---|---|
| `CARGO_REGISTRY_TOKEN` | https://crates.io/settings/tokens |
| `DOCKERHUB_USERNAME` | `cdnsteve` |
| `DOCKERHUB_TOKEN` | https://hub.docker.com/settings/security |

### Package registries

| Registry | Package | URL |
|---|---|---|
| GitHub | roboticforce/remembrallmcp | https://github.com/roboticforce/remembrallmcp |
| crates.io | remembrall-core | https://crates.io/crates/remembrall-core |
| crates.io | remembrall-server | https://crates.io/crates/remembrall-server |
| npm | remembrallmcp | https://www.npmjs.com/package/remembrallmcp |
| PyPI | remembrallmcp | https://pypi.org/project/remembrallmcp |
| Docker Hub | cdnsteve/remembrallmcp | https://hub.docker.com/r/cdnsteve/remembrallmcp |

### Version numbering

`remembrall-core` and `remembrall-server` share a version via `[workspace.package]` in the root `Cargo.toml`. Bump it there, not in individual crate Cargo.tomls. The `remembrall-server` dependency on `remembrall-core` must be pinned to the exact version (`version = "=X.Y.Z"`).

## Conventions

- No `unwrap()` in library code - use `Result<T>` with `thiserror`/`anyhow`
- All database operations go through `MemoryStore` or `GraphStore` - no raw SQL elsewhere
- Schema name is never hardcoded - always use `self.schema` with format strings
- Spike binaries (`src/bin/spike*.rs`) are throwaway validation code, not production
- Tree-sitter parsing is all Rust - no Python in the pipeline
- Ingestion logic (GitHub PRs, markdown docs) lives in `remembrall-core/src/ingest.rs` - reusable without MCP. Server tools are thin wrappers.
- MCP tool implementations use a delegation pattern: `#[tool]` methods in `lib.rs` call `*_impl()` functions in `tools/` modules
- `supported_extensions()` in `indexer.rs` is the single source of truth for all 13 supported file extensions
