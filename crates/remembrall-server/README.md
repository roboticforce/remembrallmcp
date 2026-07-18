# remembrall-server

MCP server for [RemembrallMCP](https://github.com/roboticforce/remembrallmcp) - whole-codebase knowledge for AI coding agents. A field-aware code graph (functions, classes, methods, fields, references) plus persistent memory, built on Rust, Postgres + pgvector, exposed over MCP.

## MCP Tools

**Memory:** `remembrall_store`, `remembrall_recall`, `remembrall_update`, `remembrall_delete`, `remembrall_ingest_github`, `remembrall_ingest_docs`

**Code Intelligence:** `remembrall_index`, `remembrall_impact`, `remembrall_lookup_symbol`

## Quick Start

```bash
# Install
cargo install remembrall-server

# Initialize (sets up Postgres + schema + embedding model)
remembrall init

# Add to .mcp.json
# { "mcpServers": { "remembrall": { "command": "remembrall" } } }
```

See the [full documentation](https://github.com/roboticforce/remembrallmcp) for Docker Compose setup, benchmarks, and configuration.

## License

MIT
