# Graph publication and relationship growth repair

This repair addresses duplicate index generations and speculative relationship growth. It does not reclaim existing PostgreSQL allocation, rebuild live projects, or modify memories.

## Publication contract

Production graph indexing through MCP, the watch CLI, the file watcher, and the reusable `Indexer` acquires a PostgreSQL transaction-scoped lock for the configured schema and project **before reading source files**. Publication replaces that project's symbols and relationships in one transaction. Readers see a committed generation; a failed replacement rolls back. Different projects and schemas have independent lock keys.

The reusable `Indexer` retains its configured file filters and supplied `CodeParser`. Its file-index metadata is published in the same transaction as its graph. It reads metadata through the leased connection, so a single-connection pool remains usable.

The watcher batches source events by project and re-resolves the complete project. Replacing only one file with fresh symbol IDs previously cascaded away incoming edges from unchanged callers. Full refresh preserves those links, at the cost of parsing the project again. Pending work is bounded by the registered project set, with a single wake token; access-only events do not cause refresh loops.

A strict scan refuses publication on walk, read or syntax errors and reports the offending source. The previous graph remains available. Repositories containing intentionally invalid fixtures or syntax unsupported by the installed grammar can therefore retain a stale generation until their indexing scope or source is corrected. A successful empty directory scan is different from an unsuccessful scan. The ordinary parser inspection API remains permissive; publication uses the strict API.

## Relationship contract

The directory walker no longer expands a bare method/type name into every matching symbol in the project. Unknown calls remain unresolved placeholders rather than guesses that become stored graph edges. In-parser resolved relationships survive; TypeScript/JavaScript uses receiver and lexical scope evidence, and explicit supported import bindings provide cross-file resolution.

This is a precision tradeoff, not a claim of complete language-server resolution. Tested positives include TypeScript import aliases/default/namespace bindings, local and same-class calls, type use, Python `from` aliases, and Rust imported functions and qualified methods. TypeScript re-export traversal, arbitrary typed receivers, inherited methods, and Python namespace-module calls are not newly resolved. Java, Kotlin, C#, Go and Ruby retain explicit file imports and parser-resolved local edges; unsupported project-wide name guesses remain unresolved. Existing Python/Rust local scope heuristics are not a complete type checker.

Persisted relationship counts exclude unresolved placeholders and duplicate or self edges; parser smoke counts are attempted output and must not be reported as stored rows. Database publication is atomic, but a filesystem traversal is not: edits during the scan may yield a mixed source view. Queued watcher events request a subsequent refresh.

## Validation and rollout

Use the database-free preflight before a rebuild:

```sh
cargo run -p remembrall-core --bin parser_smoke -- --strict /absolute/source/root preflight
```

Database integration tests require explicit `REMEMBRALL_GRAPH_TEST_DATABASE_URL`, `REMEMBRALL_GRAPH_TEST_DATABASE_NAME` (ending in `_test`) and `REMEMBRALL_GRAPH_TEST_DATA_DIRECTORY`. The fixture verifies the connected database and exact data directory before creating a unique test schema. There is no fallback to the application database URL and missing test configuration fails explicitly. Provision a disposable PostgreSQL instance first; never point these tests at an application instance.

```sh
cargo test -p remembrall-core --test graph_publication
```

Rollout remains a separate operation:

1. Back up the database and identify the exact projects and owning indexer processes.
2. Stop or fence every old writer for the affected project. Old binaries and direct low-level `GraphStore` callers do not participate in the new publication protocol.
3. Adopt the repaired server and the separately reviewed Yolokitten daemon startup serialization patch together. Run strict source preflights before rebuilding selected projects.
4. Verify stored identities, links, counts and application queries. Measure actual relation/index allocation and reclaimable space before proposing PostgreSQL maintenance.

No amount of passing tests establishes that already-running indexers have adopted the new binary. No live adoption or reclaimed-space claim is made here.

## Verification record

Parent reruns on the isolated feature checkout passed: 67 existing parser unit tests, 11 grounded-resolution regression tests, 5 watcher tests, 6 watch CLI scope tests, and 6 disposable-PostgreSQL publication tests. The latter verify serialization, reader visibility, rollback after delete, connection-pool size one, project/schema independence, incoming-edge preservation, and parse-failure preservation. The regression population is defined by those five explicit Cargo test selectors; it is not the entire workspace suite. The disposable PostgreSQL instance was stopped after verification.

Database-free strict scans also passed for the core source directory and Yolokitten's worktree scripts. These probes establish parsing compatibility for those selected source trees, not full-repository compatibility or production adoption.

## Review finding: project identity versus replacement scope

The independent review found that the watch CLI's basename-derived names can collide for different roots. The new replacement operation would then alternate which root occupies the project. Why-chain: graph disappearance follows whole-project deletion; deletion is keyed by a name derived from a non-unique basename; the producer's root scope was not checked against the replacement boundary. The actionable principle is to validate ownership keys when widening an update from additive/per-file work to complete replacement.

Mechanism finding: canonical roots and project names must be checked before initial publication. Prevention finding: collision rejection needs both a conflicting-roots negative test and valid distinct-roots/repeated-root controls. Transfer boundary: the same constraint applies to watcher registration; separate processes and explicit project names still require operator-coordinated ownership. Over-restricting names would break intentional refreshes of the same root, so identical canonical roots should be deduplicated, not treated as conflicting owners. The review is a local code finding; no affected live database operation was run. Policy lookup returned related ownership rules but no exact Remembrall watch rule.

The incident analysis is retained here with the code. Exhaust memory-carrier completion is not claimed: this task explicitly excludes writes to the live Remembrall database.

The correction resolves and validates all CLI roots before database connection. Repeated paths and symlink aliases of one canonical root are deduplicated. Different roots producing the same project name fail with an actionable error. `FileWatcher::add_project` now returns `Result<()>`, canonicalizes its root, and refuses conflicting names or reassignment of an existing root; callers must handle the result. The application callsites do so. This validation is per watcher instance; independent explicit index calls and processes remain responsible for choosing compatible project names.

Independent review by `/root/graph_repair_review`: initial changes requested for that collision; follow-up **ACCEPT** after inspecting the correction and independently rerunning the six CLI and five watcher tests (exit 0). No other material finding was reported in the scoped review. Authors were quiescent before parent integration. Runtime logs and detailed handoffs remain in the checkout's untracked `.wt/graph-repair/`; this document preserves the acceptance and limits in the committed source tree.
