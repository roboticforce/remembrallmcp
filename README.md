# RemembrallMCP

![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg) [![Crates.io](https://img.shields.io/crates/v/remembrall-server.svg)](https://crates.io/crates/remembrall-server) [![CI](https://github.com/roboticforce/remembrallmcp/actions/workflows/ci.yml/badge.svg)](https://github.com/roboticforce/remembrallmcp/actions/workflows/ci.yml) [![Docker](https://img.shields.io/docker/pulls/cdnsteve/remembrallmcp.svg)](https://hub.docker.com/r/cdnsteve/remembrallmcp)

Whole-codebase knowledge for AI coding agents. A field-aware code graph plus persistent memory, built on Rust, Postgres + pgvector, and exposed over MCP.

**The problem:** AI coding agents see a few pages out of the book each session. They grep, read, and re-derive how the codebase fits together from scratch - no map of what calls what, no way to know what breaks when something changes, and no memory of decisions made in past sessions.

**The solution:** RemembrallMCP gives the agent the whole codebase - a field-aware dependency graph (functions, classes, methods, **fields**, and the references between them) across 8 languages, plus persistent memory that survives between sessions.

**1. Field-Aware Code Graph** - A live map of your codebase built with tree-sitter. Functions, classes, methods, and data fields, plus call, import, defines, inherits, and field-reference relationships across 8 languages. Ask "what breaks if I change this?" - down to a single struct field - and get an answer in milliseconds, before the agent touches anything.

**2. Persistent Memory** - Decisions, patterns, and organizational knowledge that survive between sessions. Hybrid semantic + full-text search finds relevant context instantly.

```
remembrall_recall("authentication middleware patterns")
-> 3 relevant memories from past sessions

remembrall_index("/path/to/project", "myapp")
-> Builds dependency graph: 847 symbols, 1,203 relationships

remembrall_impact("AuthMiddleware", direction="upstream")
-> 12 files depend on AuthMiddleware (with confidence scores)

remembrall_impact("amount", direction="upstream")
-> methods that read self.amount, across the whole codebase

remembrall_store("Switched from JWT to session tokens because...")
-> Decision stored for future sessions
```

### Why the code graph matters

Without RemembrallMCP, agents explore your codebase from scratch every session. Claude Code spawns `Explore` agents, Codex reads dozens of files, Cursor greps through directories - all burning tokens and time just to understand what calls what. A single "find all callers of this function" task can cost thousands of tokens across multiple tool calls.

With RemembrallMCP, that same query is a single `remembrall_impact` call that returns in <1ms with zero exploration tokens. The dependency graph is already built and waiting.

| | Without RemembrallMCP | With RemembrallMCP |
|---|---|---|
| "What calls UserService?" | Agent greps, reads 8-15 files, spawns sub-agents | `remembrall_impact` - 1 call, <1ms |
| "Where is auth middleware defined?" | Agent globs, reads matches, filters | `remembrall_lookup_symbol` - 1 call, <1ms |
| "Who references the `amount` field?" | Agent greps for `self.amount`, misses ORM and cross-module usages | `remembrall_impact` - 1 call, <1ms |
| "What did we decide about caching?" | Agent has no context, asks you | `remembrall_recall` - 1 call, ~25ms |
| Typical exploration cost | 5,000-20,000 tokens per question | ~200 tokens (tool call + response) |

The savings scale with codebase size. On a small project, an agent can grep and read its way through. On a 500-file monorepo, that exploration becomes the bottleneck - agents hit context limits, spawn multiple sub-agents, or miss cross-module dependencies entirely. RemembrallMCP's graph queries stay under 10ms regardless of project size because the structure is pre-indexed in Postgres, not discovered at runtime.

This is the difference between an agent that reads a few pages out of the book every time and one that already holds the whole codebase.

### Benchmarks

RemembrallMCP is currently benchmarked on two surfaces:

- **Agent productivity on code tasks** - Tested on [pallets/click](https://github.com/pallets/click) v8.1.7 (594 symbols, 1,589 relationships). Five identical coding tasks run with and without RemembrallMCP. [Full report](benchmarks/reports/benchmark-2026-04-02.md).
- **Memory recall quality** - Local recall harness run against 31 ground-truth queries covering search quality, filtering, edge cases, ranking, and latency.

| Metric | Without RemembrallMCP | With RemembrallMCP | Delta |
|--------|----------------------|---------------------|-------|
| Total tool calls (5 tasks) | 112 | 5 | **-95.5%** |
| Estimated tokens | ~56,000 | ~1,000 | **-98.2%** |
| Avg tool calls per question | 22.4 | 1.0 | **-95.5%** |

The savings compound on larger codebases. Click is ~90 files - on a 500+ file monorepo, agents without RemembrallMCP need proportionally more exploration calls, while graph queries stay under 10ms regardless of size.

| Memory Recall Metric | Result |
|---|---|
| Queries passed | **31 / 31** |
| Recall@5 | **0.917** |
| Precision@5 | **0.619** |
| MRR | **0.908** |
| p95 latency | **14ms** |

Run the benchmarks yourself: see [`benchmarks/`](benchmarks/) for the harness and task definitions.

For the broader benchmark strategy across memory retrieval, long-horizon memory, code graph correctness, and agent productivity, see [`docs/benchmark-roadmap.md`](docs/benchmark-roadmap.md).

## Requirements

- Docker (for the easiest setup) or PostgreSQL 16 with [pgvector](https://github.com/pgvector/pgvector)
- For GitHub ingestion: [GitHub CLI](https://cli.github.com/) (`gh`) installed and authenticated

## Quick Start

### Option 1: Docker Compose (easiest)

```bash
git clone https://github.com/roboticforce/remembrallmcp.git
cd remembrallmcp

# Start Postgres + initialize schema + download embedding model
docker compose up -d

# Verify it's running
docker compose exec remembrall remembrall status
```

That's it. Postgres with pgvector, the schema, and the embedding model are all set up automatically. The database and model cache persist across restarts.

To run the MCP server:

```bash
docker compose run --rm remembrall
```

### Option 2: Download prebuilt binary

```bash
# macOS (Apple Silicon)
curl -fsSL https://github.com/roboticforce/remembrallmcp/releases/latest/download/remembrall-aarch64-apple-darwin.tar.gz | tar xz
sudo mv remembrall /usr/local/bin/

# Linux (x86_64)
curl -fsSL https://github.com/roboticforce/remembrallmcp/releases/latest/download/remembrall-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv remembrall /usr/local/bin/

# Initialize (sets up Postgres via Docker, creates schema, downloads model)
remembrall init
```

### Option 3: Build from source (requires Rust 1.94+)

```bash
cargo build -p remembrall-server --release
# Binary is at target/release/remembrall

remembrall init
```

### Connect to your MCP client

#### Codex

Codex uses the same MCP server definition format. Register the server as `remembrall` and point it at either the installed binary or your local release build.

**If `remembrall` is installed in `PATH`:**

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "remembrall"
    }
  }
}
```

**If running from a local source checkout:**

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

**If using Docker Compose from Codex:**

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "docker",
      "args": ["compose", "-f", "/path/to/remembrallmcp/docker-compose.yml", "run", "--rm", "-T", "remembrall"]
    }
  }
}
```

Restart Codex after adding the server so it reconnects and loads the tools.

#### Claude Code, Cursor, and other MCP clients

Add to your project's `.mcp.json` (works with Claude Code, Cursor, and any MCP-compatible client).

**If using a prebuilt binary or built from source:**

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "remembrall"
    }
  }
}
```

**If using Docker Compose:**

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "docker",
      "args": ["compose", "-f", "/path/to/remembrallmcp/docker-compose.yml", "run", "--rm", "-T", "remembrall"]
    }
  }
}
```

**If running from source (not installed to PATH):**

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

### Memory

| Tool | Description |
|------|-------------|
| `remembrall_recall` | Search memories - hybrid semantic + full-text with RRF fusion |
| `remembrall_store` | Store decisions, patterns, knowledge with vector embeddings |
| `remembrall_update` | Update an existing memory (content, summary, tags, or importance) |
| `remembrall_delete` | Remove a memory by UUID |
| `remembrall_ingest_github` | Bulk-import merged PR descriptions from a GitHub repo |
| `remembrall_ingest_docs` | Scan a directory for markdown files and ingest them as memories |

### Code Intelligence

| Tool | Description |
|------|-------------|
| `remembrall_index` | Parse a project directory into a field-aware code graph (functions, classes, methods, and fields across 8 languages) |
| `remembrall_impact` | Blast radius analysis - "what breaks if I change this?" Works on functions, classes, methods, and fields |
| `remembrall_lookup_symbol` | Find where a function, class, method, or field is defined across the project |

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
|  symbols (functions, classes, methods, fields)    |
|  relationships (calls, imports, defines,          |
|                 inherits, references)             |
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
