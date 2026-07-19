# remembrall-core

Core library for [RemembrallMCP](https://github.com/roboticforce/remembrallmcp) - a field-aware code graph (functions, classes, methods, fields, references) plus persistent memory for AI agents.

## What's included

- **MemoryStore** - CRUD + hybrid semantic/full-text search with RRF fusion
- **GraphStore** - code symbol storage, relationship tracking, impact analysis via recursive CTEs
- **Parsers** - tree-sitter based parsers for Python, TypeScript, JavaScript, Rust, Go, Java, Kotlin, Ruby
- **Embedder** - fastembed (all-MiniLM-L6-v2, 384-dim, in-process ONNX Runtime)
- **Ingestion** - GitHub PR import and markdown docs ingestion

## Usage

This crate is the foundation layer. For the full MCP server, see [remembrall-server](https://crates.io/crates/remembrall-server).

```rust
use remembrall_core::memory::store::MemoryStore;
use remembrall_core::graph::store::GraphStore;
use remembrall_core::parser::index_directory;
```

## Requirements

- PostgreSQL 16 with [pgvector](https://github.com/pgvector/pgvector) extension

## License

MIT
