# RemembrallMCP

Repo instructions for Codex and other agents working in this repository.

## Scope

- This file applies to the entire repository.
- Follow direct user, system, and developer instructions over this file.

## Project Overview

- Rust workspace for a whole-codebase knowledge MCP server: a field-aware code graph (functions, classes, methods, fields, and references) plus persistent memory for AI coding agents.
- Main crates:
- `crates/remembrall-core`: core memory, graph, parser, indexing, and ingestion logic.
- `crates/remembrall-server`: MCP server and CLI.
- Database: Postgres + pgvector.
- Default local database URL: `postgres://postgres:postgres@localhost:5450/remembrall`.
- Default schema: `remembrall`, configurable via `REMEMBRALL_SCHEMA`.

## Common Commands

- Start database: `docker start cocoindex-postgres`
- Build workspace: `cargo build`
- Build release server binary: `cargo build -p remembrall-server --release`
- Run server over stdio: `DATABASE_URL="postgres://postgres:postgres@localhost:5450/remembrall" ./target/release/remembrall`
- Run validation spike: `DATABASE_URL="postgres://postgres:postgres@localhost:5450/remembrall" cargo run --bin spike3`

## CLI Reference

- `remembrall init`
- `remembrall init --database-url <url>`
- `remembrall serve`
- `remembrall start`
- `remembrall stop`
- `remembrall status`
- `remembrall doctor`
- `remembrall reset --force`
- `remembrall version`

## MCP Setup

- Local MCP binary: `target/release/remembrall`
- Codex global MCP registration on this machine currently points to `/Users/steve/Dev/remembrallmcp/target/release/remembrall`.
- Claude Code config lives in `.mcp.json`.
- If changing launch paths or required env vars, keep Codex and Claude MCP configs aligned.

## Config

- Primary config file: `~/.remembrall/config.toml`
- Env vars override config:
- `REMEMBRALL_DATABASE_URL` or `DATABASE_URL`
- `REMEMBRALL_SCHEMA`

## Architecture Notes

- Ingestion logic belongs in `remembrall-core/src/ingest.rs` and should remain reusable outside MCP.
- MCP tool wrappers in `remembrall-server` should stay thin and delegate to implementation modules.
- Database access should go through `MemoryStore` or `GraphStore`.
- Schema names must not be hardcoded; use configured schema values.
- `supported_extensions()` in `indexer.rs` is the single source of truth for supported file extensions.

## Code Conventions

- Avoid `unwrap()` in library code; prefer explicit error handling with `Result`.
- Keep raw SQL isolated to the store layers.
- Do not treat spike binaries in `src/bin/spike*.rs` as production code.
- Keep parser and indexing logic in Rust; do not introduce Python into the core pipeline unless explicitly requested.

## Release Notes

- Branch model: `develop` for active work, `main` for releases, `feature/*` off `develop`, `hotfix/*` off `main`.
- Shared crate version is defined in the workspace `Cargo.toml`.
- `remembrall-server` must pin `remembrall-core` to the exact matching version.

## Practical Guidance

- Before changing indexing, parsing, or graph logic, read the relevant crate files first; many behaviors rely on two-phase resolution and incremental indexing.
- If you change MCP tool behavior, verify both CLI and MCP-facing expectations still hold.
- Prefer focused patches over broad refactors unless the user asks for larger structural changes.
