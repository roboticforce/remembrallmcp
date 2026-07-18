# RemembrallMCP Community Posts

Ready-to-post content for each platform. Do not cross-post these - each is written
for its specific community culture. Read the platform notes before posting.

---

## 1. Hacker News - Show HN

**Title (76 chars):**
```
Show HN: RemembrallMCP - whole-codebase knowledge for AI coding agents
```

**Top-level comment (post this as your first comment immediately after submitting):**

```
I built this because I got tired of watching Claude Code re-explore the same codebase
every session. On a 500-file project, the agent would spawn explore sub-agents, grep
through directories, and read dozens of files just to answer "what calls UserService?"
- burning 5,000-20,000 tokens on structure that doesn't change between sessions.

RemembrallMCP is a Postgres-backed MCP server that gives agents two things:

1. Persistent memory - decisions, patterns, and organizational knowledge indexed with
   hybrid semantic + full-text search (RRF fusion of pgvector cosine similarity and
   tsvector BM25). A contradiction detection pass runs at 0.75 similarity before
   storing, so you don't accumulate stale or conflicting knowledge.

2. A live code dependency graph - built with tree-sitter (Rust bindings, 8 languages).
   Two-phase resolution: collect all symbols first, then resolve cross-file imports and
   call relationships. Stored in Postgres with recursive CTEs for impact traversal.
   Confidence scores decay through call chains. "What breaks if I change AuthMiddleware?"
   returns in under 10ms regardless of codebase size.

The embedding side uses fastembed (ONNX Runtime, all-MiniLM-L6-v2, 384-dim) running
in-process. No OpenAI API calls, no external embedding service - the model downloads
once (~23 MB) and runs locally. The whole thing is a single Rust binary.

Benchmark on pallets/click (594 symbols, 1,589 relationships), 5 identical coding tasks
run with and without:

- Total tool calls: 112 without vs 5 with (-95.5%)
- Estimated tokens: ~56,000 without vs ~1,000 with (-98.2%)
- Memory recall: 31/31 ground truth queries, Recall@5 of 0.917, p95 latency 14ms

The savings compound on larger codebases. Click is ~90 files. On a 500+ file monorepo,
agents without RemembrallMCP need proportionally more exploration. Graph queries stay
under 10ms because the structure is pre-indexed in Postgres, not discovered at runtime.

Cold-start problem is real - a fresh instance knows nothing. Two ingestion tools solve
this: `remembrall_ingest_github` shells to the `gh` CLI and bulk-imports merged PR
descriptions (deduplicates by content fingerprint), and `remembrall_ingest_docs`
walks a directory for markdown files and splits on H2 headers.

MCP transport is stdio via rmcp. Works with Claude Code, Cursor, Codex, or any
MCP-compatible client.

GitHub: https://github.com/roboticforce/remembrallmcp
crates.io: remembrall-server, remembrall-core

Questions I'm genuinely curious about from this crowd:
- Better approaches to cross-file import resolution in languages with dynamic imports
  (JavaScript is the most painful right now)
- Whether anyone has done RRF vs learned fusion for this kind of hybrid retrieval
- Thoughts on confidence decay models for call-chain impact analysis
```

---

## 2. Reddit r/rust

**Title:**
```
RemembrallMCP: persistent memory and code dependency graph for AI agents (MCP server in Rust)
```

**Post body:**

```
Built this over the past few months as a practical experiment: can you give AI coding
agents persistent memory and structural codebase understanding using MCP?

The short answer is yes, and the token savings are substantial (95.5% fewer tool calls
on benchmarks). But the more interesting part to me was figuring out how to build it
well in Rust.

**Architecture**

Two crates: `remembrall-core` (library) and `remembrall-server` (MCP server + CLI).
The library has no MCP dependencies - it's just Postgres + pgvector + tree-sitter +
fastembed. The server wraps it with thin `#[tool]` methods via the `rmcp` crate.

**Tree-sitter parsing**

The code graph uses tree-sitter Rust bindings for 8 languages (Python, Rust, Go, Java,
TypeScript, JavaScript, Ruby, Kotlin). Two-phase resolution was the key design decision:

Phase 1: Walk the directory and collect all symbols (functions, classes, methods) into
a HashMap<String, Vec<SymbolId>> keyed by name.

Phase 2: Re-walk and resolve cross-file relationships. Import statements and function
calls are matched against the Phase 1 symbol table. Without the two-phase approach,
you miss most cross-file call relationships.

The `CodeParser` trait has `parse_file()` for Phase 1 and `resolve_relationships()`
for Phase 2. Each language implements both.

**Embeddings without an API**

Used `fastembed` crate (ONNX Runtime bindings). all-MiniLM-L6-v2, 384-dim, runs
in-process. Model downloads once on `remembrall init`, ~23 MB. No external API keys,
no network dependency at query time.

I had a moment of "is this going to be a pain to cross-compile?" - it wasn't, because
fastembed uses the ONNX Runtime static libraries. macOS arm64 and Linux x86_64 both
produce clean binaries.

**Hybrid search with RRF**

Both pgvector cosine similarity (for semantic search) and tsvector BM25 (for full-text)
run against the same query. Results merged with Reciprocal Rank Fusion. Implemented
directly in `sqlx` - parameterized queries only, no raw SQL in application code.

**Impact analysis**

Recursive CTE in Postgres with cycle detection (visited set passed through the recursion)
and confidence decay (multiply by 0.85 per hop). Returns the full upstream or downstream
dependency chain for any symbol. Runs 4-9ms on a 1,500+ relationship graph.

**Error handling**

`thiserror` for library errors, `anyhow` for the binary. No `unwrap()` in library code,
which I enforced with a Clippy deny. The MCP tool methods all return `Result<CallToolResult>`
and surface user-readable messages rather than panicking.

GitHub: https://github.com/roboticforce/remembrallmcp

Happy to dig into any part of the implementation. The tree-sitter parsing is probably
the most complex piece - JavaScript in particular has edge cases with dynamic imports
and computed property names that the current Phase 2 pass misses.
```

---

## 3. Reddit r/LocalLLaMA

**Title:**
```
RemembrallMCP: persistent memory for local AI agents - no cloud APIs, in-process ONNX embeddings
```

**Post body:**

```
One thing that's bugged me about AI coding agents: they're stateless. Every session
starts cold. Worse, the popular memory tools usually phone home to OpenAI or Anthropic
for embeddings - so you've set up a local LLM but your agent's memory still calls
an external API.

RemembrallMCP runs entirely local:

- Embeddings: all-MiniLM-L6-v2 via ONNX Runtime, in-process. 384-dim vectors,
  ~23 MB model download on first run. No API key, no network call after that.
- Database: Postgres + pgvector. Your data stays in your database.
- MCP server: single Rust binary, stdio transport. Works with any MCP-compatible
  client - Claude Code, Cursor, Codex, whatever you're using locally.

What it gives your agent:

1. Persistent memory across sessions - decisions, patterns, architecture notes.
   Hybrid search (semantic + full-text) finds relevant context without you having
   to re-explain your stack every time.

2. Code dependency graph - built from your actual source with tree-sitter.
   "What breaks if I change this function?" is a single tool call that returns
   in under 10ms. No more agents grepping through your codebase burning context.

Benchmarks on 5 coding tasks (pallets/click codebase):
- 95.5% reduction in tool calls
- 98.2% reduction in tokens consumed

Setup is Docker Compose or a single prebuilt binary. Point your MCP client at it,
run `remembrall init`, index your project, done.

GitHub: https://github.com/roboticforce/remembrallmcp

Not affiliated with any cloud provider. MIT license.
```

---

## 4. Reddit r/ClaudeAI

**Title:**
```
RemembrallMCP: solves the cold-start problem for Claude Code - persistent memory + code graph
```

**Post body:**

```
If you use Claude Code heavily, you've probably noticed it re-explores your codebase
every session. It reads files, runs grep searches, spawns sub-agents to understand
structure - all before doing any actual work. On a medium-sized project, that's
often 5,000-20,000 tokens just to answer "what calls this function?"

I built RemembrallMCP to fix this. It's an MCP server that gives Claude two new
capabilities:

**Persistent memory**

Store decisions, architecture notes, and patterns that survive between sessions.

```
> "Recall what we decided about authentication"
remembrall_recall: 3 relevant memories found
- "Switched from JWT to session tokens (2024-12) because..."
- "Auth middleware lives in middleware/auth.rb, not routes/"
- "Never store user IDs in the session directly, use..."
```

**Code dependency graph**

Index your project once. Claude can then answer structural questions instantly
instead of exploring from scratch.

```
Without RemembrallMCP:
Claude Code runs: Glob, Read (x4), Grep, Read (x8), Glob, Read (x3)...
~22 tool calls to answer "what breaks if I change UserService?"

With RemembrallMCP:
remembrall_impact("UserService", direction="upstream")
-> 1 tool call, returns in <10ms, lists all 12 dependent files with confidence scores
```

**Cold start**

A fresh instance has no knowledge. Two commands to bootstrap:

```
> remembrall_ingest_github repo="yourorg/yourrepo" limit=100
> remembrall_ingest_docs path="/path/to/project"
```

The GitHub ingestion pulls merged PR descriptions via `gh` CLI - immediately gives
Claude context on why things were built the way they were. Docs ingestion walks your
directory for markdown files and splits them into searchable memories.

**Benchmarks (5 tasks on pallets/click):**
- 95.5% fewer tool calls
- 98.2% fewer tokens
- Memory recall: 31/31 ground truth queries pass, Recall@5 of 0.917

Setup: add to `.mcp.json`, run `remembrall init`, done.

GitHub: https://github.com/roboticforce/remembrallmcp

Currently v0.1.x - works well, rough edges exist. MIT license.
```

---

## 5. Reddit r/selfhosted

**Title:**
```
RemembrallMCP: self-hosted persistent memory for AI coding agents - Postgres backend, MIT license
```

**Post body:**

```
Built a self-hosted memory layer for AI coding agents (Claude Code, Cursor, Codex, etc.).
No SaaS, no vendor lock-in, no cloud API dependencies.

**What it does**

AI coding agents are stateless - every session starts from zero. RemembrallMCP gives
them persistent memory (decisions, architecture notes, patterns) and a code dependency
graph that answers "what breaks if I change this?" in milliseconds.

**Stack**

- Postgres 16 + pgvector extension (the only backend, no alternatives)
- Single Rust binary - no runtime dependencies
- ONNX Runtime for embeddings, runs in-process
- stdio MCP protocol - just a process your client spawns

**Setup (Docker Compose)**

```bash
git clone https://github.com/roboticforce/remembrallmcp.git
cd remembrallmcp
docker compose up -d
```

That starts Postgres with pgvector, runs the schema migration, and downloads the
embedding model (~23 MB). The database and model cache live in named Docker volumes
so they persist across restarts.

Add to your MCP client config:

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

**If you already run Postgres**

```bash
remembrall init --database-url postgres://user:pass@localhost:5432/mydb
```

Creates a `remembrall` schema in your existing Postgres instance. No separate database
container needed.

**Data ownership**

Everything is in your Postgres database. Standard pg_dump backup. No external data
transmission - embeddings run in-process, no API calls.

**Prebuilt binaries** available for macOS arm64 and Linux x86_64. Or build from source
with `cargo build -p remembrall-server --release`.

GitHub: https://github.com/roboticforce/remembrallmcp
MIT license, Rust core, actively developed.
```

---

## 6. Reddit r/MachineLearning

**Title:**
```
RemembrallMCP: hybrid retrieval (cosine + BM25/RRF) + knowledge graph with confidence-weighted edges for AI agent memory
```

**Post body:**

```
[Project] Built a memory system for AI coding agents. The retrieval and knowledge
representation decisions are the interesting parts.

**Hybrid retrieval with RRF**

Two retrieval modes run in parallel on every query:

1. Semantic: pgvector HNSW index, cosine similarity, all-MiniLM-L6-v2 embeddings
   (384-dim, ONNX Runtime in-process)
2. Full-text: Postgres tsvector/tsquery with BM25-style ranking

Results merged with Reciprocal Rank Fusion: score = sum(1 / (k + rank_i)) where k=60.
RRF was chosen over learned fusion because it requires no training data and
generalizes well across query types. When semantic and lexical agree, the result
floats high. When they disagree, RRF naturally hedges.

Recall quality on 31 ground-truth queries:
- Recall@5: 0.917
- Precision@5: 0.619
- MRR: 0.908
- p95 latency: 14ms

**Contradiction detection**

Before storing a new memory, a similarity search runs at threshold 0.75. If near-
duplicates exist, the storing agent sees them in the response and can decide whether
to update, overwrite, or leave both. This is simple but catches the most common
failure mode: agents accumulating stale or conflicting facts over time.

No LLM-based contradiction resolution (too slow, non-deterministic). Just
similarity-gated storage with human-in-the-loop resolution via the MCP response.

**Knowledge graph with confidence-weighted edges**

The code dependency graph stores relationships with confidence scores (0.0-1.0).
High-confidence: direct function calls with resolved import. Lower-confidence:
inferred relationships from partial symbol resolution.

Impact traversal uses a recursive CTE in Postgres with:
- Cycle detection via accumulated visited set
- Confidence decay per hop (0.85 multiplier currently)
- Configurable direction (upstream callers or downstream callees)
- Depth limit to bound the traversal

The 0.85 decay is a placeholder - the right value probably depends on relationship
type (a direct call should decay less than an inferred import). That's a future
improvement.

**Retrieval quality by query type**

The ground truth test suite has 31 queries across categories: semantic similarity,
exact keyword match, tag filtering, scope filtering, and edge cases (empty query,
very long query, special characters). Hybrid retrieval consistently outperforms
either semantic or lexical alone, especially on queries with technical terminology
(symbol names, framework names) where BM25 has strong signal.

GitHub: https://github.com/roboticforce/remembrallmcp

Benchmark harness and ground truth fixtures are in the repo if you want to run your
own evaluations. The recall test harness is in `crates/remembrall-recall-test/`.
```

---

## 7. dev.to Article

**Title:**
```
Stop Re-Exploring Your Codebase: Persistent Memory for AI Coding Agents
```

**Tags:** `rust, ai, mcp, postgres`

**Body:**

```markdown
Every AI coding session starts from zero. Your agent doesn't know why you chose
Postgres over MongoDB three months ago. It doesn't know that `AuthMiddleware` has
12 callers across 6 modules. It doesn't know the team convention that config
always lives in `config/` and never gets imported from `lib/`.

So it explores. It greps. It reads files. It spawns sub-agents. On a 500-file
project, answering "what calls UserService?" can cost 15-20 tool calls and
thousands of tokens - before the agent has done a single thing you asked it to do.

RemembrallMCP solves this with two tools: persistent memory and a live code
dependency graph. Here's how it works.

## The cold-start problem

A stateless agent has to rediscover structure every session. That's fine for small
projects, but it scales badly. The bigger your codebase, the more expensive
exploration becomes - and graph queries stay constant because the structure is
pre-indexed in Postgres.

The benchmark numbers make this concrete. Five identical coding tasks on
[pallets/click](https://github.com/pallets/click) (a ~90-file Python project,
594 symbols, 1,589 relationships):

| | Without RemembrallMCP | With RemembrallMCP |
|---|---|---|
| Total tool calls (5 tasks) | 112 | 5 |
| Estimated tokens | ~56,000 | ~1,000 |
| "What calls `invoke()`?" | ~22 tool calls | 1 tool call |

On a 500+ file monorepo, the without-RemembrallMCP column grows proportionally.
The with-RemembrallMCP column stays flat.

## How it works

RemembrallMCP is an MCP server - a process your AI client spawns over stdio. It
exposes 9 tools across two categories.

### Persistent memory

```
remembrall_store("Switched from JWT to session tokens because our load balancer
doesn't support sticky sessions. Type: decision, tags: auth, architecture")

remembrall_recall("authentication decisions")
// Returns: the JWT/session token decision, any related auth patterns,
//          middleware documentation - ranked by relevance
```

Memories are stored with vector embeddings (all-MiniLM-L6-v2, 384-dim, running
in-process via ONNX Runtime). Search is hybrid: pgvector cosine similarity for
semantic matching plus Postgres full-text search, merged with Reciprocal Rank
Fusion. Recall@5 of 0.917 on the ground truth test suite.

Before storing, a similarity check at 0.75 threshold detects near-duplicates.
This prevents accumulating stale or contradictory facts over sessions.

### Code dependency graph

```
remembrall_index("/path/to/project", "myapp")
// Parses 847 symbols, 1,203 relationships. 2.3s for 89 Python files.

remembrall_impact("AuthMiddleware", direction="upstream")
// 12 files depend on AuthMiddleware
// -> routes/api.py (confidence: 0.95)
// -> routes/admin.py (confidence: 0.95)
// -> middleware/logging.py (confidence: 0.81)
// ...

remembrall_lookup_symbol("UserService")
// -> src/services/user_service.py, line 42
```

The graph is built with tree-sitter (Rust bindings) across 8 languages: Python,
Rust, Go, Java, TypeScript, JavaScript, Ruby, and Kotlin. The parsing uses a
two-phase approach - collect all symbols first, then resolve cross-file
relationships against the complete symbol table. Impact traversal is a recursive
CTE in Postgres with cycle detection and confidence decay per hop.

### Solving cold start

A fresh instance knows nothing. Two ingestion tools bootstrap it from existing
project history:

```
remembrall_ingest_github repo="myorg/myrepo" limit=100
// Pulls 100 merged PRs via `gh` CLI, digests titles and bodies as memories.
// Your PR history is institutional knowledge. Now the agent has it.

remembrall_ingest_docs path="/path/to/project"
// Walks the directory, finds all .md files, splits on H2 headers.
// README, ARCHITECTURE, ADRs - all searchable immediately.
```

Run both once per project. After that, `remembrall_recall` has immediate context.

## Getting started

**Option 1: Docker Compose (recommended)**

```bash
git clone https://github.com/roboticforce/remembrallmcp.git
cd remembrallmcp
docker compose up -d
```

Postgres with pgvector, schema migration, and embedding model download happen
automatically. Database persists in a named volume.

**Option 2: Prebuilt binary**

```bash
# macOS (Apple Silicon)
curl -fsSL https://github.com/roboticforce/remembrallmcp/releases/latest/download/remembrall-aarch64-apple-darwin.tar.gz | tar xz
sudo mv remembrall /usr/local/bin/
remembrall init
```

**Connect to Claude Code (or any MCP client)**

Add to `.mcp.json` in your project:

```json
{
  "mcpServers": {
    "remembrall": {
      "command": "remembrall"
    }
  }
}
```

Restart the client. All 9 tools load automatically.

**Index your project and try it**

```
> "Index this project at /path/to/myproject with project name myproject"

> "What would break if I changed the UserService class?"

> "Recall what we decided about database migrations"
```

## The technical stack

For those curious about the implementation:

- **Rust** core, two crates (`remembrall-core` library, `remembrall-server` binary)
- **Postgres 16 + pgvector** - HNSW index for vector search, tsvector for full-text
- **fastembed** - ONNX Runtime Rust bindings, all-MiniLM-L6-v2, 384-dim, in-process
- **tree-sitter** - Rust bindings for all 8 language parsers
- **sqlx** - compile-time verified queries, async, no raw SQL in application code
- **rmcp** - MCP stdio transport
- **Hybrid search** - RRF fusion, no learned ranking required

No external API dependencies at query time. Single binary. MIT license.

## Current state and roadmap

This is v0.1.x - functional and benchmarked, rough edges exist. TypeScript parsing
quality is lower than Python/Go/Rust due to JavaScript's dynamic import patterns.
The confidence decay model for impact analysis is a fixed 0.85 per hop, which should
eventually be relationship-type-aware.

GitHub: https://github.com/roboticforce/remembrallmcp

Benchmarks, ground truth test fixtures, and the recall harness are all in the repo.
```

---

## 8. Twitter/X Thread

**Tweet 1 (hook + link):**
```
AI coding agents re-explore your codebase every session.

On a 500-file project: 15-20 tool calls and ~15,000 tokens just to answer
"what calls UserService?"

Built something to fix this: https://github.com/roboticforce/remembrallmcp

thread on how it works
```

**Tweet 2 (the problem made concrete):**
```
Without persistent memory, every Claude Code session starts from zero.

No memory of why you chose Postgres over MongoDB.
No knowledge of what calls what.
No architecture context.

So the agent explores. Reads files. Spawns sub-agents.
Before doing a single thing you asked.
```

**Tweet 3 (the solution):**
```
RemembrallMCP is an MCP server (single Rust binary) that gives agents:

1. Persistent memory - decisions, patterns, docs
   Hybrid semantic + full-text search, 14ms p95 latency

2. Code dependency graph - built with tree-sitter (8 languages)
   "What breaks if I change AuthMiddleware?" - 1 call, <10ms
```

**Tweet 4 (the numbers):**
```
Benchmarked on 5 coding tasks (pallets/click, ~90 files):

Without: 112 tool calls, ~56,000 tokens
With: 5 tool calls, ~1,000 tokens

-95.5% tool calls
-98.2% tokens

The savings scale with codebase size. Graph queries stay flat regardless.
```

**Tweet 5 (cold start solution):**
```
Cold start problem: fresh instance knows nothing.

Two commands to bootstrap:

remembrall_ingest_github repo="yourorg/repo" limit=100
- Pulls 100 merged PRs via gh CLI
- Your PR history is institutional knowledge

remembrall_ingest_docs path="/path/to/project"
- Walks directory, ingests all markdown files
- README, ADRs, architecture docs - all searchable
```

**Tweet 6 (technical details for the technical audience):**
```
Technical stack:
- Postgres + pgvector (HNSW) for vector search
- tsvector for full-text, merged via RRF
- fastembed / ONNX Runtime for embeddings (in-process, no API key)
- tree-sitter for code parsing
- Recursive CTEs for impact analysis with confidence decay

No cloud API dependencies. MIT license.
```

**Tweet 7 (CTA):**
```
Setup is Docker Compose or a prebuilt binary + remembrall init.

If you use Claude Code, Cursor, or Codex on any project bigger than a few dozen
files, give it a try.

https://github.com/roboticforce/remembrallmcp

Feedback welcome - especially on languages where the parser is missing edge cases.
```

---

## 9. LinkedIn Post

**Post:**

```
AI coding agents have a productivity problem that's easy to miss: they're stateless.

Every session, the agent re-explores your codebase. It reads files, runs searches,
tries to understand structure. On a 50-file project, that's tolerable. On a 500-file
codebase, it becomes the bottleneck - the agent spends more time exploring than
building.

I benchmarked this concretely on 5 identical coding tasks:

- Without memory: 112 tool calls, ~56,000 tokens consumed
- With RemembrallMCP: 5 tool calls, ~1,000 tokens consumed
- Result: 95.5% fewer tool calls, 98.2% fewer tokens

That's not a tuning improvement. It's a structural one.

The core idea: pre-index the codebase once, then answer structural questions from
the index instead of re-exploring. "What calls UserService?" becomes a single
sub-10ms query instead of a multi-step grep session. Architecture decisions stored
from previous sessions are retrieved in ~14ms rather than lost between conversations.

RemembrallMCP is an open-source MCP server (Rust, Postgres + pgvector, MIT license)
that adds this capability to any MCP-compatible AI client - Claude Code, Cursor,
Codex, and others.

Two components:

1. Persistent memory - hybrid semantic + full-text search over decisions, patterns,
   and architectural knowledge stored across sessions.

2. Code dependency graph - built with tree-sitter across 8 languages. Blast-radius
   analysis shows exactly what would break before the agent touches anything.

For teams using AI agents on production codebases, the compounding effect matters.
Fewer exploration tokens means more tokens available for actual work, lower costs,
and agents that stay within context limits on larger tasks.

GitHub: https://github.com/roboticforce/remembrallmcp

Early release (v0.1.x), actively developed. Open to feedback from developers
already running AI agents on large codebases.
```

---

## Posting Notes

**Hacker News timing:** Post Tuesday-Thursday between 9am-12pm Pacific. Engage with
every comment in the first 2 hours - comment velocity is a ranking signal. Do not ask
anyone to upvote.

**Reddit general:** Do not cross-post the same text to multiple subreddits. Each post
above is written specifically for that community. Post to one subreddit per day at most.

**r/rust:** Read recent posts before submitting. The community values substance over
hype - architecture discussion and honest tradeoffs get traction. Avoid any language
that reads like a product announcement.

**r/LocalLLaMA:** Lead with the "no external API" angle immediately. This community
sees through marketing language fast. If you engage in comments, be direct about
current limitations.

**dev.to:** Schedule for Tuesday or Wednesday morning. Add the four tags exactly as
listed. The article is written for RSS readers, so the value has to be in the body,
not a teaser.

**Twitter/X:** Post the thread as a single connected thread, not individual tweets on
separate days. Engage with any replies within the first hour.

**LinkedIn:** Post Tuesday or Wednesday morning. Do not add hashtags to the body -
they look like SEO spam on LinkedIn in 2025/2026. The algorithm favors dwell time,
so the post is written to reward full reads.
```

Sources referenced for platform norms:
- [Show HN Guidelines](https://news.ycombinator.com/showhn.html)
- [How to crush your Hacker News launch](https://dev.to/dfarrell/how-to-crush-your-hacker-news-launch-10jk)
- [One Year of MCP: November 2025 Spec Release](https://blog.modelcontextprotocol.io/posts/2025-11-25-first-mcp-anniversary/)
- [Why the Model Context Protocol Won](https://thenewstack.io/why-the-model-context-protocol-won/)
