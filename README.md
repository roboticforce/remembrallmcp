# RemembrallMCP

Persistent knowledge memory layer for AI agents. Rust core, Postgres + pgvector backend, MCP protocol.

**The problem:** AI coding agents (Copilot, Cursor, Claude Code, Devin) are stateless. Every session starts from zero - no memory of past decisions, team preferences, error patterns, or how the codebase fits together.

**The solution:** RemembrallMCP gives agents persistent memory through the [Model Context Protocol](https://modelcontextprotocol.io). Decisions, patterns, code relationships, and organizational context survive between sessions and are available to any MCP-compatible client.

```
Agent starts a task
  |
  remembrall_recall("authentication middleware patterns")
  -> Returns 3 relevant memories from past sessions
  |
  remembrall_index("/path/to/project", "myapp")
  -> Builds code dependency graph
  |
  remembrall_impact("AuthMiddleware", direction="upstream")
  -> Shows 12 files that depend on AuthMiddleware
  |
  Agent makes the change with full context
  |
  remembrall_store("Switched from JWT to session tokens because...")
  -> Stores the decision for future agents
```

## Requirements

- Rust 1.94+ (for building from source)
- PostgreSQL 16 with [pgvector](https://github.com/pgvector/pgvector) extension (or let `remembrall init` set up Docker for you)
- For GitHub ingestion: [GitHub CLI](https://cli.github.com/) (`gh`) installed and authenticated

## Quick Start

### Install

```bash
# Build from source
cargo build -p remembrall-server --release

# Binary is at target/release/remembrall
```

### Initialize

```bash
remembrall init
```

This sets up a Docker-managed Postgres container with pgvector, creates the schema, and pre-downloads the embedding model (~23 MB). Config is written to `~/.remembrall/config.toml`.

To use an existing Postgres instead:

```bash
remembrall init --database-url postgres://user:pass@host/dbname
```

### Connect to your MCP client

Add to your project's `.mcp.json` (works with Claude Code, Cursor, and any MCP-compatible client):

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "remembrall"
    }
  }
}
```

If running from source (not installed to PATH):

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "/path/to/remembrallmcp/target/release/remembrall",
      "env": {
        "DATABASE_URL": "postgres://postgres:postgres@localhost:5450/remembrall"
      }
    }
  }
}
```

Restart your MCP client. All 9 tools will be available automatically.

### Try it

```
> "Store a memory: We chose Postgres over MongoDB because our query patterns
   are relational. Type: decision, tags: database, architecture"

> "Recall what we know about database decisions"

> "Index this project and show me the impact of changing UserService"
```

## MCP Tools

| Tool | Description |
|------|-------------|
| `remembrall_recall` | Search memories - hybrid semantic + full-text with RRF fusion |
| `remembrall_store` | Store decisions, patterns, knowledge with vector embeddings |
| `remembrall_update` | Update an existing memory (content, summary, tags, or importance) |
| `remembrall_delete` | Remove a memory by UUID |
| `remembrall_ingest_github` | Bulk-import merged PR descriptions from a GitHub repo |
| `remembrall_ingest_docs` | Scan a directory for markdown files and ingest them as memories |
| `remembrall_impact` | Blast radius analysis - "what breaks if I change this?" |
| `remembrall_lookup_symbol` | Find where a function or class is defined |
| `remembrall_index` | Index a project directory to build the code graph |

## Supported Languages

| Language | Extensions | Quality Score |
|----------|-----------|---------------|
| Python | .py | A (94.1) |
| Java | .java | A (92.6) |
| JavaScript | .js, .jsx | A (92.0) |
| Rust | .rs | A (91.0) |
| Go | .go | A (90.7) |
| Ruby | .rb | B (87.9) |
| TypeScript | .ts, .tsx | B (84.3) |
| Kotlin | .kt, .kts | B (82.9) |

Scores measured against real open-source projects (Click, Gson, Axios, bat, Cobra, Sidekiq, Hono, Exposed) using automated ground truth tests.

## Cold Start

A new RemembrallMCP instance has no knowledge. Use the ingestion tools to bootstrap from existing project history.

**From GitHub PR history:**

```
> remembrall_ingest_github repo="myorg/myrepo" limit=100
```

Fetches merged PRs via `gh`, digests titles and bodies into memories, and tags them by project. PRs with less than 50 characters of body are skipped. Deduplication by content fingerprint prevents re-ingestion on repeat runs.

**From markdown docs:**

```
> remembrall_ingest_docs path="/path/to/project"
```

Walks the directory tree, finds all `.md` files, splits them by H2 section headers, and stores each section as a searchable memory. Skips `node_modules`, `.git`, `target`, and similar directories. Good for README, ARCHITECTURE, ADRs, and any written docs.

Run both once per project. After ingestion, `remembrall_recall` has immediate context.

## Architecture

```
Source Code                   Organizational Knowledge
    |                                 |
    v                                 v
Tree-sitter Parsers           Ingestion Pipeline
(8 languages)                 (GitHub PRs, Markdown docs)
    |                                 |
    v                                 v
+--------------------------------------------------+
|              Postgres + pgvector                  |
|                                                   |
|  memories (text + embeddings + metadata)          |
|  symbols (functions, classes, methods)            |
|  relationships (calls, imports, inherits)         |
+--------------------------------------------------+
                          |
                    MCP Server (stdio)
                          |
              Any MCP-compatible AI agent
```

- **Parsing:** tree-sitter (Rust bindings, no Python in the pipeline)
- **Embeddings:** fastembed (all-MiniLM-L6-v2, 384-dim, in-process ONNX Runtime)
- **Search:** Hybrid RRF (semantic cosine similarity + full-text tsvector)
- **Graph queries:** Recursive CTEs with cycle detection and confidence decay
- **Transport:** stdio via rmcp

## CLI Commands

| Command | Description |
|---------|-------------|
| `remembrall init` | Set up database, schema, and embedding model |
| `remembrall serve` | Run the MCP server (default when no subcommand given) |
| `remembrall start` | Start the Docker database container |
| `remembrall stop` | Stop the Docker database container |
| `remembrall status` | Show memory count, symbol count, connection status |
| `remembrall doctor` | Check for common problems (Docker, pgvector, schema, model) |
| `remembrall reset --force` | Drop and recreate the schema (deletes all data) |
| `remembrall version` | Print version and config path |

## Configuration

Config file: `~/.remembrall/config.toml` (created by `remembrall init`)

Environment variables override config file values:

| Variable | Description |
|----------|-------------|
| `REMEMBRALL_DATABASE_URL` or `DATABASE_URL` | PostgreSQL connection string |
| `REMEMBRALL_SCHEMA` | Database schema name (default: `remembrall`) |

## Project Structure

```
crates/
  remembrall-core/          # Library - parsers, memory store, graph store, embedder
  remembrall-server/        # MCP server + CLI binary
  remembrall-test-harness/  # Parser quality testing against ground truth
  remembrall-recall-test/   # Search quality testing
docs/                       # Architecture and test plan docs
test-fixtures/              # Ground truth TOML files for 8 languages
tests/                      # Recall test fixtures
```

## Performance

| Operation | Time |
|-----------|------|
| Memory store | 7ms |
| Semantic search (HNSW) | <1ms |
| Full-text search | <1ms |
| Hybrid recall (end-to-end) | ~25ms |
| Impact analysis | 4-9ms |
| Symbol lookup | <1ms |
| Index 89 Python files | 2.3s |

## License

MIT
