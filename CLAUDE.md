# Engram

Knowledge memory layer for AI agents. Rust core, Postgres + pgvector backend, MCP protocol.

## Quick Reference

- **Language:** Rust 1.94+, edition 2024
- **Workspace:** `crates/engram-core` (library), `crates/engram-server` (MCP server)
- **Database:** `postgres://postgres:postgres@localhost:5450/engram` (Docker: `cocoindex-postgres`)
- **Schema:** `engram` (configurable via `ENGRAM_SCHEMA`)
- **Architecture doc:** `ARCHITECTURE.md` in project root

## Build & Run

```bash
# Start database
docker start cocoindex-postgres

# Build everything
cargo build

# Build MCP server (release)
cargo build -p engram-server --release

# Run MCP server manually (stdio)
DATABASE_URL="postgres://postgres:postgres@localhost:5450/engram" ./target/release/engram-mcp

# Run ground truth tests (validates everything works)
DATABASE_URL="postgres://postgres:postgres@localhost:5450/engram" cargo run --bin spike3
```

## MCP Server

Binary: `target/release/engram-mcp` (38 MB). Configured in `.mcp.json` for Claude Code.

**Tools:** `engram_store`, `engram_impact`, `engram_lookup_symbol`, `engram_index`, `engram_delete`

**Embedding:** fastembed (ONNX Runtime, all-MiniLM-L6-v2, 384-dim). In-process, no external API. Model downloads on first run (~23 MB).

## Project Structure

```
crates/
  engram-core/src/
    memory/store.rs      # MemoryStore - CRUD + semantic/fulltext search
    memory/types.rs      # Memory, Source, Scope, MemoryType enums
    graph/store.rs       # GraphStore - symbols, relationships, impact analysis (recursive CTEs)
    graph/types.rs       # Symbol, Relationship, ImpactResult, Direction
    parser/python.rs     # Tree-sitter Python parser
    parser/typescript.rs # Tree-sitter TypeScript/JS parser
    parser/walker.rs     # Directory walker + two-phase cross-file resolution
    indexer.rs           # Incremental indexer with mtime tracking + CodeParser trait
    config.rs            # Config from env vars
    embed.rs             # Embedder trait + FastEmbedder (fastembed/ONNX, 384-dim)
    search.rs            # Hybrid search (TODO)
    ingest.rs            # Webhook ingestion (TODO)
  engram-server/
    src/lib.rs           # MCP server - 5 tools (store, impact, lookup, index, delete)
    src/main.rs          # Binary entry point (stdio transport)
  engram-python/         # PyO3 bindings (deferred)
```

## Database Tables (all in `engram` schema)

- `memories` - text knowledge with pgvector embeddings, scope, tags, fingerprint dedup
- `symbols` - code symbols (File, Function, Class, Method) with file/line/language/project
- `relationships` - edges (Calls, Imports, Defines, Inherits) with confidence scores
- `file_index` - mtime tracking for incremental reindexing

## Key Patterns

- **Two-phase resolution:** Parser collects all files first, then resolves imports and cross-file calls against the full symbol set
- **Impact analysis:** Recursive CTEs with cycle detection, confidence decay through the chain
- **Incremental indexing:** Compare disk mtime vs stored mtime, only reparse changed files
- **Content fingerprinting:** Normalized hash for memory deduplication

## Conventions

- No `unwrap()` in library code - use `Result<T>` with `thiserror`/`anyhow`
- All database operations go through `MemoryStore` or `GraphStore` - no raw SQL elsewhere
- Schema name is never hardcoded - always use `self.schema` with format strings
- Spike binaries (`src/bin/spike*.rs`) are throwaway validation code, not production
- Tree-sitter parsing is all Rust - no Python in the pipeline
