# Engram Architecture

*Last updated: March 2026*

## What is Engram?

Engram is a knowledge memory layer for AI agents. It gives any MCP-compatible agent persistent memory - decisions, patterns, code relationships, and organizational context that survives between sessions.

**The problem:** Every AI agent tool (Copilot, Cursor, Devin) is stateless. Every session starts from zero. Agents have no memory of past decisions, team preferences, error patterns, or how the codebase fits together.

**The solution:** Engram is a Rust-native memory engine backed by Postgres + pgvector. It stores two kinds of knowledge:

1. **Text memories** - decisions, patterns, error fixes, preferences, guidelines
2. **Code graph** - functions, classes, imports, call chains, and impact analysis

Agents query Engram via MCP to get relevant context before acting.

---

## System Overview

```
Source Code                   Organizational Knowledge
    |                                 |
    v                                 v
+-------------------+    +------------------------+
| Tree-sitter       |    | Ingestion Pipeline     |
| Parser            |    | (GitHub, Slack, etc.)  |
| (Python, TS, JS)  |    |                        |
+--------+----------+    +-----------+------------+
         |                            |
         v                            v
+--------------------------------------------------+
|              Postgres + pgvector                  |
|                                                   |
|  engram.memories          engram.symbols          |
|  - content + embedding    - name, type, file      |
|  - semantic search (HNSW) - language, project     |
|  - full-text search       - line numbers          |
|  - scope (org/team/proj)  - signature             |
|  - fingerprint dedup                              |
|                           engram.relationships    |
|  engram.file_index        - source -> target      |
|  - mtime tracking         - Calls, Imports,       |
|  - incremental reindex      Defines, Inherits     |
|                           - confidence scoring    |
+-------------------------+------------------------+
                          |
                          v
              +-----------------------+
              |    MCP Server         |
              |    (engram-server)    |
              |                       |
              |  Tools:               |
              |  - store_memory       |
              |  - search_memory      |
              |  - impact_analysis    |
              |  - find_symbol        |
              |  - index_project      |
              +-----------+-----------+
                          |
              +-----------+-----------+
              | Any MCP Client        |
              | Claude Code, Cursor,  |
              | Copilot, custom agents|
              +------------------------+
```

---

## Crate Structure

```
engram/
  Cargo.toml                    # Workspace root
  crates/
    engram-core/                # Library - all logic lives here
      src/
        lib.rs                  # Module exports
        config.rs               # Config from env vars
        error.rs                # Error types
        memory/
          types.rs              # Memory, Source, Scope, MemoryType
          store.rs              # MemoryStore - CRUD + search
        graph/
          types.rs              # Symbol, Relationship, ImpactResult
          store.rs              # GraphStore - upsert + recursive CTE impact analysis
        parser/
          mod.rs                # Public API: parse_python_file, parse_ts_file, index_directory
          python.rs             # Tree-sitter Python parser
          typescript.rs         # Tree-sitter TypeScript/JS parser
          walker.rs             # Directory walker + two-phase cross-file resolution
        indexer.rs              # Incremental indexer with mtime tracking + CodeParser trait
        search.rs               # Hybrid search (TODO)
        ingest.rs               # Webhook ingestion pipeline (TODO)
      src/bin/
        spike.rs                # Spike 1: memory + graph benchmarks
        spike2.rs               # Spike 2: multi-project regex indexing
        spike3.rs               # Ground truth: 10 real-world correctness tests
        parser_smoke.rs         # Tree-sitter parser validation
    engram-server/              # MCP server (TODO - next to build)
      src/lib.rs
    engram-python/              # PyO3 bindings (deferred until PyO3 supports Python 3.14)
      src/lib.rs
```

---

## Core Components

### MemoryStore (`memory/store.rs`)

Postgres-backed storage for text memories with pgvector embeddings.

**Tables:** `engram.memories` (content, embedding, scope, tags, metadata, fingerprint, importance, expiry)

**Key operations:**
- `store(input, embedding)` - store a memory with its vector embedding
- `search_semantic(embedding, limit, min_similarity, scope)` - cosine similarity via HNSW index
- `search_fulltext(query, limit)` - Postgres tsvector search
- `get(id)` - fetch by ID, auto-increments access_count
- `find_by_fingerprint(hash)` - deduplication check
- `delete(id)`, `count(scope)`

**Indexes:** HNSW (vector cosine), GIN (full-text, tags), B-tree (type, scope, fingerprint)

### GraphStore (`graph/store.rs`)

Code relationship graph stored as Postgres adjacency tables.

**Tables:**
- `engram.symbols` - code symbols (File, Function, Class, Method) with file path, line numbers, language, project
- `engram.relationships` - edges between symbols (Calls, Imports, Defines, Inherits) with confidence scores

**Key operations:**
- `upsert_symbol(symbol)` - insert or update a symbol
- `add_relationship(rel)` - insert or update an edge
- `impact_analysis(symbol_id, direction, max_depth)` - recursive CTE traversal
- `find_symbol(name, type)` - lookup by name
- `remove_file(path, project)` - cascade delete for reindexing

**Impact analysis** uses recursive CTEs with cycle detection. Traverses upstream (who calls me?), downstream (what do I call?), or both. Confidence decays multiplicatively through the chain.

### Parser (`parser/`)

Tree-sitter based source code analysis. Pure Rust, no Python involved.

**Supported languages:** Python (.py), TypeScript (.ts, .tsx), JavaScript (.js, .jsx)

**What it extracts:**
- Symbols: functions, classes, methods, files
- Relationships: function calls, imports, class inheritance, method definitions
- Metadata: signatures, line numbers, decorators

**Two-phase resolution (`walker.rs`):**
1. Parse all files independently, collect symbols and raw import metadata
2. Build path-to-UUID map from all File symbols, then resolve:
   - Relative imports (`from ..storage import X`) by walking up directories
   - Absolute imports (`import sugar.memory.store`) by suffix matching
   - Dotted method calls (`self.queue.get_next()`) by extracting final method name
   - Cross-file calls by rewriting synthetic UUIDs to real symbol UUIDs

### Indexer (`indexer.rs`)

Incremental code indexing with mtime tracking.

**Table:** `engram.file_index` (file_path, project, mtime, indexed_at)

**How it works:**
1. Walk directory, collect files with disk mtimes
2. Compare against stored mtimes in `file_index`
3. Parse + store only new/changed files
4. Delete symbols for files that no longer exist on disk

**`CodeParser` trait** - plug-in interface so the indexer doesn't own parsing logic. Any language can be added by implementing `parse(file_path, source, language)`.

---

## Database

**Engine:** PostgreSQL 16 + pgvector 0.8.2 (via Docker: `cocoindex-postgres`)

**Connection:** `postgres://postgres:postgres@localhost:5450/engram`

**Schema:** Everything lives in the `engram` schema. Configurable via `ENGRAM_SCHEMA` env var.

**Tables:**
| Table | Purpose | Key indexes |
|-------|---------|-------------|
| `memories` | Text knowledge with embeddings | HNSW (vector), GIN (full-text, tags), B-tree (scope) |
| `symbols` | Code symbols (functions, classes, etc.) | B-tree (file_path, name+type) |
| `relationships` | Edges between symbols | B-tree (source_id, target_id) |
| `file_index` | Mtime tracking for incremental indexing | PK (file_path, project) |

---

## Configuration

All config via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `ENGRAM_DATABASE_URL` or `DATABASE_URL` | (required) | Postgres connection string |
| `ENGRAM_POOL_SIZE` | 25 | Connection pool size |
| `ENGRAM_SCHEMA` | `engram` | Postgres schema name |

---

## Running It

### Prerequisites
- Rust 1.94+
- Docker (for Postgres + pgvector)
- The `cocoindex-postgres` container running on port 5450

### Start the database
```bash
docker start cocoindex-postgres
```

### Run the ground truth tests (validates everything works)
```bash
DATABASE_URL="postgres://postgres:postgres@localhost:5450/engram" cargo run --bin spike3
```

Expected output: 10/10 tests pass across Sugar (Python), Revsup (Django), NomadSignal (TypeScript).

### Run the parser smoke test
```bash
DATABASE_URL="postgres://postgres:postgres@localhost:5450/engram" cargo run --bin parser_smoke -- /path/to/project project_name
```

### Run spike1 (memory + graph benchmarks)
```bash
DATABASE_URL="postgres://postgres:postgres@localhost:5450/engram" cargo run --bin spike
```

---

## Validated Performance

| Operation | Time |
|-----------|------|
| Schema init (tables + HNSW index) | 83ms |
| Store 3 memories | 7ms |
| Get by ID | 809us |
| Semantic search (pgvector HNSW) | 942us |
| Full-text search | 573-907us |
| Impact analysis (realistic) | 4-9ms |
| Impact analysis (stress, 3,698 nodes) | 33ms |
| Find symbol by name | 476us |
| Index Sugar (89 files, 1,157 symbols, 9,297 rels) | 2.3s |
| Index Revsup (92 files, 771 symbols, 1,602 rels) | 1.2s |

---

## Ground Truth (10/10)

Real questions answered correctly against real codebases:

| # | Question | Project |
|---|----------|---------|
| 1 | What methods does MemoryStore have? (17) | Sugar |
| 2 | What calls get_next_work()? (dotted method resolution) | Sugar |
| 3 | What does loop.py import? (relative import resolution) | Sugar |
| 4 | What inherits BaseEmbedder? | Sugar |
| 5 | Blast radius of store()? | Sugar |
| 6 | What are all Django models? (8 classes) | Revsup |
| 7 | What views call ForecastService? (none - correctly identified) | Revsup |
| 8 | What Django signal handlers exist? (3) | Revsup |
| 9 | What does data-adapter.ts export? (9 functions) | NomadSignal |
| 10 | What calls getCountry()? | NomadSignal |

---

## MCP Server (`engram-server`)

The MCP server exposes Engram's capabilities to any MCP-compatible agent (Claude Code, Cursor, etc.) over stdio transport.

**Binary:** `target/release/engram-mcp` (38 MB, includes ONNX Runtime for embeddings)

### Tools

| Tool | Description | Key params |
|------|-------------|------------|
| `engram_store` | Store knowledge, decisions, patterns | `content`, `memory_type`, `tags`, `importance` |
| `engram_impact` | Blast radius analysis - what breaks if you change a symbol | `symbol_name`, `direction`, `max_depth` |
| `engram_lookup_symbol` | Find where a function/class is defined | `name`, `symbol_type` |
| `engram_index` | Index a project directory to build the code graph | `path`, `project` |
| `engram_delete` | Remove a memory by UUID | `id` |

### Embedding

Uses `fastembed` (ONNX Runtime) with `all-MiniLM-L6-v2` (384-dim) for in-process embedding. No external API or Python dependency. Model downloads on first run (~23 MB).

Pluggable via the `Embedder` trait in `engram-core/src/embed.rs`.

### Setup for Claude Code

The `.mcp.json` in the project root configures Claude Code to use the Engram MCP server:

```json
{
  "mcpServers": {
    "engram": {
      "command": "/Users/steve/Dev/engram/target/release/engram-mcp",
      "env": {
        "DATABASE_URL": "postgres://postgres:postgres@localhost:5450/engram"
      }
    }
  }
}
```

To use in other projects, copy the `.mcp.json` or add the engram entry to the project's existing `.mcp.json`.

### Running manually

```bash
# Build release binary
cargo build -p engram-server --release

# Test initialization
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | ./target/release/engram-mcp
```

---

## What's Next

| Item | Status | Description |
|------|--------|-------------|
| MCP server | Done | 5 tools over stdio, fastembed embeddings, Postgres backend |
| `engram_recall` (search) | Next | Hybrid semantic + full-text search with RRF score fusion |
| GitHub webhook ingestion | TODO | PR merge -> digest -> memory store |
| Concurrent load test | TODO | 50 simulated agents hitting the engine |
| Incremental indexing | TODO | Wire the Indexer to tree-sitter (CodeParser impl exists, not connected yet) |
