# Awesome List PR Submissions

Ready-to-use content for submitting RemembrallMCP to community lists.

---

## 1. punkpeye/awesome-mcp-servers

**Section:** `## 🧠 Knowledge & Memory`

**Line to add (alphabetical by repo name under that section):**

```
- [roboticforce/remembrallmcp](https://github.com/roboticforce/remembrallmcp) 🦀 🏠 🍎 🪟 🐧 - Whole-codebase knowledge for AI coding agents. A field-aware code graph (functions, classes, methods, fields, references) plus persistent memory, with blast-radius impact analysis and incremental indexing for 8 languages. In-process ONNX embeddings via fastembed, no external API required.
```

Emoji key used:
- 🦀 Rust
- 🏠 Local (self-hosted)
- 🍎 macOS, 🪟 Windows, 🐧 Linux

**PR title:**

```
Add RemembrallMCP - Rust MCP server for persistent agent memory and code graph
```

**PR description body:**

```
## Summary

Adds RemembrallMCP to the Knowledge & Memory section.

## About the server

- **Repo:** https://github.com/roboticforce/remembrallmcp
- **Language:** Rust
- **Hosting:** Self-hosted (Docker + Postgres/pgvector)

RemembrallMCP gives AI coding agents the whole codebase, not a few pages. It builds a field-aware code graph (functions, classes, methods, fields, and references) across 8 languages (Python, TypeScript, JavaScript, Rust, Go, Java, Kotlin, Ruby) with incremental indexing and blast-radius impact analysis, plus persistent memory that survives sessions using hybrid semantic + full-text search (pgvector + tsvector).

Embeddings run in-process via fastembed/ONNX Runtime - no external API call or API key needed.

MCP tools: remembrall_store, remembrall_recall, remembrall_update, remembrall_delete, remembrall_index, remembrall_impact, remembrall_lookup_symbol, remembrall_ingest_github, remembrall_ingest_docs.

## Checklist

- [x] Entry is in the correct section (Knowledge & Memory)
- [x] Entry follows the list's formatting conventions
- [x] Description is accurate and not promotional
- [x] Repo is public and functional
```

---

## 2. wong2/awesome-mcp-servers

**Note:** This list does not accept GitHub PRs. Submissions go through the form at https://mcpservers.org/submit

Submit there with:
- Name: RemembrallMCP
- URL: https://github.com/roboticforce/remembrallmcp
- Category: Memory / Knowledge
- Description: Whole-codebase knowledge for AI coding agents. A field-aware code graph (functions, classes, methods, fields, references) plus persistent memory. Rust, Postgres/pgvector backend, hybrid semantic + full-text search, incremental indexing for 8 languages. In-process ONNX embeddings.

---

## 3. modelcontextprotocol/servers

**Section:** `## 🌎 Community Servers`

The community servers list is alphabetical. Add under the section with this format (matching existing entries):

**Line to add:**

```
- **[RemembrallMCP](https://github.com/roboticforce/remembrallmcp)** - Whole-codebase knowledge for AI coding agents. A field-aware code graph (functions, classes, methods, fields, and references) plus persistent memory, with blast-radius impact analysis and incremental indexing across Python, TypeScript, JavaScript, Rust, Go, Java, Kotlin, and Ruby. In-process ONNX embeddings - no external API required.
```

**PR title:**

```
Add RemembrallMCP to community servers - whole-codebase knowledge for AI coding agents
```

**PR description body:**

```
## Summary

Adds RemembrallMCP to the Community Servers section.

**Repository:** https://github.com/roboticforce/remembrallmcp

## What it does

RemembrallMCP is a self-hosted MCP server that gives AI coding agents the whole codebase - a field-aware code graph (functions, classes, methods, fields, and references) plus persistent memory.

**Memory tools:**
- `remembrall_store` - store decisions, patterns, knowledge with tags and scope
- `remembrall_recall` - hybrid semantic + full-text search across stored memories
- `remembrall_update` / `remembrall_delete` - maintain memory accuracy over time
- `remembrall_ingest_github` - bulk import merged PRs from a GitHub repo
- `remembrall_ingest_docs` - ingest markdown files from a project directory

**Code graph tools:**
- `remembrall_index` - build a field-aware code graph from a project directory (8 languages)
- `remembrall_impact` - blast-radius analysis: what breaks if this symbol changes
- `remembrall_lookup_symbol` - find where a function, class, method, or field is defined

**Implementation:**
- Written in Rust
- Postgres + pgvector backend (Docker provided)
- In-process ONNX embeddings via fastembed - no external embedding API needed
- Supports Python, TypeScript, JavaScript, Rust, Go, Java, Kotlin, Ruby

## Checklist

- [x] Server is functional and publicly available
- [x] Entry follows the formatting of existing community server entries
- [x] Description is factual and accurate
```

---

## 4. rust-unofficial/awesome-rust

**Status: NOT YET READY - requires 50+ GitHub stars or 2,000+ crates.io downloads.**

Check current numbers before submitting:
- Stars: https://github.com/roboticforce/remembrallmcp
- Downloads: https://crates.io/crates/remembrall-server

**When eligible, section to target:** `### Applications > MLOps` or create a case for `### Development tools`

**Line to add (when eligible):**

```
- [roboticforce/remembrallmcp](https://github.com/roboticforce/remembrallmcp) [[remembrall-server](https://crates.io/crates/remembrall-server)] - Whole-codebase knowledge for AI coding agents. A field-aware code graph plus persistent memory, backed by Postgres/pgvector with hybrid semantic + full-text search and incremental code indexing for 8 languages.
```

**PR title (for when ready):**

```
Add remembrallmcp - MCP memory server with code graph for AI agents
```

**PR description body (for when ready):**

```
## Summary

Adds remembrallmcp to the MLOps section (or Development tools if maintainers prefer).

- **GitHub:** https://github.com/roboticforce/remembrallmcp (X stars at time of submission)
- **crates.io:** https://crates.io/crates/remembrall-server (X downloads at time of submission)

RemembrallMCP is a Rust MCP server that gives AI coding agents the whole codebase: a field-aware code graph (functions, classes, methods, fields, and references) plus persistent memory. It stores knowledge with pgvector embeddings, performs hybrid semantic + full-text search, and builds incremental code graphs for Python, TypeScript, JavaScript, Rust, Go, Java, Kotlin, and Ruby projects with blast-radius impact analysis.

Meets the 50-star / 2,000-download threshold as noted in CONTRIBUTING.md.

## Checklist

- [x] Meets popularity threshold
- [x] Entry is alphabetically ordered within its section
- [x] Entry includes crates.io link
- [x] Description is accurate and useful to Rust developers
```

---

## 5. mahseema/awesome-ai-tools

**Section:** Under `## 👩‍💻 Code with AI`, there is a `### Developer tools` subsection.

**Line to add (alphabetical by name):**

```
- [RemembrallMCP](https://github.com/roboticforce/remembrallmcp) - Whole-codebase knowledge for AI coding agents. A field-aware code graph (functions, classes, methods, fields, references) plus persistent memory. MCP protocol, Postgres/pgvector backend, hybrid semantic + full-text search, incremental code indexing for 8 languages with blast-radius impact analysis. Self-hosted, no external API needed.
```

**PR title:**

```
Add RemembrallMCP to Developer tools - MCP memory layer for AI agents
```

**PR description body:**

```
## Summary

Adds RemembrallMCP to the Developer tools subsection under "Code with AI".

**URL:** https://github.com/roboticforce/remembrallmcp

RemembrallMCP is an open-source MCP server that gives AI coding agents the whole codebase - a field-aware code graph plus persistent memory across sessions. It stores decisions and context using Postgres/pgvector, supports hybrid semantic + full-text search, and builds a field-aware code graph across 8 programming languages. Designed for developers using Claude Code, Cursor, or other MCP-compatible AI tools.

- Self-hosted (Docker + Postgres)
- Written in Rust, in-process ONNX embeddings
- MCP protocol compatible

## Checklist

- [x] Entry follows the existing format (dash, bracketed link, description)
- [x] Entry is accurate and not promotional
- [x] Fits the Developer tools subsection
```

---

## 6. This Week in Rust

**How to submit:** Open a PR to https://github.com/rust-lang/this-week-in-rust adding your entry to the current draft file in `content/`. The draft for the upcoming issue is usually the most recently dated file.

**Section to add to:** `## Project/Tooling Updates`

**Line to add:**

```
* [RemembrallMCP: whole-codebase knowledge for AI coding agents](https://github.com/roboticforce/remembrallmcp)
```

**PR title:**

```
TWiR: Add RemembrallMCP to Project/Tooling Updates
```

**Blurb (2-3 sentences for the PR description or if they ask for context):**

```
RemembrallMCP is a Rust MCP server that gives AI coding agents the whole codebase, not a few pages. It builds a field-aware code graph (functions, classes, methods, fields, and references) across Python, TypeScript, JavaScript, Rust, Go, Java, Kotlin, and Ruby with incremental indexing and blast-radius impact analysis, plus persistent memory across sessions backed by Postgres and pgvector. Embeddings run in-process via fastembed/ONNX Runtime with no external API dependency.
```

**Note on timing:** TWiR issues are assembled on a weekly cadence. Submit your PR early in the week (Monday/Tuesday) to catch the current issue. If you miss the cutoff, the PR will roll to the following week. Check the current draft filename at https://github.com/rust-lang/this-week-in-rust/tree/master/content to confirm the right file to edit.
