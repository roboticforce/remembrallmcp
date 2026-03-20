# Parser Architecture

## Overview

Engram's parser system converts source code from any supported language into a universal code graph. The graph has only 4 symbol types and 4 relationship types - regardless of whether the source is Python, Rust, Go, or anything else.

## The Pipeline

```
Source File (.py, .rs, .go, .rb, .java, .kt, .ts)
    |
    v
Tree-sitter Grammar (language-specific)
    |
    v
Abstract Syntax Tree (language-specific node types)
    |
    v
Language Parser (our code - one per language)
    |
    v
FileParseResult (universal: symbols + relationships + raw_imports)
    |
    v
Walker / Two-Phase Resolver (cross-file resolution)
    |
    v
GraphStore (Postgres: symbols + relationships tables)
```

## Why One Parser Per Language?

Tree-sitter is a parser generator - it produces a concrete syntax tree with node types specific to each language's grammar. The same concept has different AST representations:

### A function definition

| Language | AST Node | Name Child |
|----------|----------|------------|
| Python | `function_definition` | `name: identifier` |
| Rust | `function_item` | `name: identifier` |
| Go | `function_declaration` | `name: identifier` |
| Ruby | `method` | `name: identifier` |
| Java | `method_declaration` | `name: identifier` |
| Kotlin | `function_declaration` | `simple_identifier` |
| TypeScript | `function_declaration` | `name: identifier` |

### An import statement

| Language | Syntax | AST Node |
|----------|--------|----------|
| Python | `from x.y import Z` | `import_from_statement` |
| Rust | `use crate::x::y::Z;` | `use_declaration` |
| Go | `import "x/y"` | `import_declaration` |
| Ruby | `require_relative '../y'` | `call` (it's a method call) |
| Java | `import com.x.y.Z;` | `import_declaration` |
| Kotlin | `import com.x.y.Z` | `import_header` |
| TypeScript | `import { Z } from './y'` | `import_statement` |

### Inheritance

| Language | Syntax | AST Pattern |
|----------|--------|-------------|
| Python | `class Foo(Bar):` | `class_definition` -> `argument_list` |
| Rust | `impl Trait for Struct` | `impl_item` -> `trait` child |
| Go | `type Foo struct { Bar }` | Embedded field in struct |
| Ruby | `class Foo < Bar` | `superclass` child |
| Java | `class Foo extends Bar implements Baz` | `superclass` + `interfaces` |
| Kotlin | `class Foo : Bar(), Baz` | `delegation_specifier` list |
| TypeScript | `class Foo extends Bar` | `class_heritage` |

Each language has unique patterns that don't map to a generic rule:
- **Python**: `self.x.method()` dotted attribute chains require special resolution
- **Rust**: `impl` blocks associate methods with structs without nesting
- **Go**: Interface satisfaction is implicit - no `implements` keyword
- **Ruby**: `include ModuleName` is a method call that means inheritance
- **Kotlin**: Extension functions (`fun String.isEmail()`) are top-level but act like methods

## What Each Parser Produces

Every parser returns the same `FileParseResult`:

```rust
pub struct FileParseResult {
    pub symbols: Vec<Symbol>,         // Functions, classes, methods, files
    pub relationships: Vec<Relationship>, // Calls, imports, defines, inherits
    pub raw_imports: Vec<RawImport>,  // Unresolved import metadata for phase 2
}
```

The universal types:

```
Symbols:  File | Function | Class | Method
Edges:    Calls | Imports | Defines | Inherits
```

This is deliberately minimal. We don't model constants, variables, type aliases, or other fine-grained constructs. 4+4 covers the relationships that matter for impact analysis.

## Two-Phase Resolution (walker.rs)

Cross-file relationships can't be resolved within a single file parse. The walker handles this:

**Phase 1: Parse all files independently**
- Each file produces symbols with UUIDs and raw import metadata
- Import targets get placeholder UUIDs (deterministic v5 UUIDs from the import string)
- Call targets that don't match local symbols also get placeholder UUIDs

**Phase 2: Resolve across files**
- Build a map of all File symbols: `path_stem -> UUID`
- Build a map of all named symbols: `name -> UUID`
- Rewrite placeholder import UUIDs to real file symbol UUIDs
- Rewrite placeholder call UUIDs to real function/method symbol UUIDs
- Unresolvable references (stdlib, third-party) keep their placeholders and are skipped at DB insert

This two-phase approach is what makes `from ..storage.work_queue import WorkQueue` resolve to the actual file, and what makes `self.queue.get_next_work()` match the real method.

## Adding a New Language

To add language X:

1. Add `tree-sitter-x` crate to `Cargo.toml`
2. Create `parser/x.rs` implementing `parse_x_file()` returning `FileParseResult`
3. Export from `parser/mod.rs`
4. Add extension dispatch in `walker.rs`

The parser needs to handle:
- Symbol extraction (functions, classes/structs, methods)
- Call detection (function calls, method calls)
- Import detection (with raw import metadata for phase 2)
- Inheritance detection (extends, implements, mixins, embedding)
- Method-to-class association (Defines relationships)

Typical size: 200-400 lines per language. Most of the boilerplate is the same - the unique logic is in AST node type mapping and language-specific patterns like dotted calls or implicit interfaces.

## Shared Code vs Per-Language Code

**Shared (in walker.rs):**
- Directory walking and file filtering
- Two-phase cross-file resolution
- Import path resolution (relative + absolute)
- Path variant matching (.py, .ts, /index.ts, etc.)
- UUID generation and placeholder management

**Per-language (in parser/x.rs):**
- AST node type names
- Symbol extraction logic
- Call expression parsing
- Import statement parsing
- Inheritance/trait detection
- Language-specific quirks (dotted calls, implicit interfaces, etc.)

## Why Tree-sitter (and Not LSP)

Tree-sitter operates at the syntax level - it doesn't know types. When we see `self.queue.get_next_work()`, we don't know `queue` is type `WorkQueue`. We match by method name heuristically, which produces false positives.

The alternative is LSP servers (rust-analyzer, pyright, gopls) which have full type resolution. But LSP requires:
- A running language server per language
- Project compilation/build setup
- Much slower analysis (seconds, not milliseconds)
- Language-specific configuration

For a memory layer that needs to index any repo instantly without setup, tree-sitter is the right trade. Syntax-level analysis covers ~80% of impact analysis questions correctly.

**When to add LSP:** If fuzzy name matching produces too many false positives in practice and agents are making bad decisions based on noisy impact results.

## Why 4 Symbol Types and 4 Relationship Types

Current model: `File | Function | Class | Method` + `Calls | Imports | Defines | Inherits`

This is deliberately minimal. Known gaps:
- **Constants/config** - "What uses DATABASE_URL?" is not answerable
- **Properties/fields** - "What reads user.email?" not tracked
- **References (reads)** - distinct from Calls, not captured

These can be added incrementally without schema changes (just new enum values). We're waiting for real usage data to show which gaps actually matter before expanding.

## Performance

Parsing is synchronous and CPU-bound (tree-sitter is compiled C code called via FFI). Typical performance on Apple Silicon:

| Project | Files | Symbols | Relationships | Time |
|---------|-------|---------|---------------|------|
| Sugar (Python, 89 files) | 89 | 1,157 | 9,297 | 2.3s |
| Revsup (Django, 92 files) | 92 | 771 | 1,602 | 1.2s |
| NomadSignal (TypeScript, 8 files) | 8 | 40 | 327 | 72ms |

Tree-sitter parsing itself is fast (~1-5ms per file). Most time is spent on file I/O and UUID resolution.
