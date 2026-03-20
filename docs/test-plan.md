# Parser Test Plan

## Overview

Validate that Engram's parsers correctly extract symbols and relationships from real-world codebases across all 8 supported languages. The goal is a repeatable, automated quality score per language.

## Scoring Rubric (per language, out of 100)

| Dimension | Weight | Pass (100%) | Partial (75%) | Fail (50%) |
|-----------|--------|-------------|---------------|------------|
| Symbol Recall | 15% | >= 95% found | 80-94% | < 80% |
| Symbol Precision | 15% | >= 95% correct | 80-94% | < 80% |
| Relationship Recall | 15% | >= 85% found | 65-84% | < 65% |
| Relationship Precision | 10% | >= 90% correct | 75-89% | < 75% |
| Import Resolution | 15% | >= 90% resolve | 70-89% | < 70% |
| Impact Analysis | 15% | >= 95% direct correct | 80-94% | < 80% |
| Edge Cases | 15% | >= 80% patterns pass | 50-79% | < 50% |

**Grades:** A (90+), B (80-89), C (70-79), D (60-69), F (<60)

**Key principle:** False negatives in impact analysis are worse than false positives. Missing a real caller means an agent makes a breaking change. An extra caller just makes the agent cautious.

## Test Categories

### A: Symbol Discovery
- Find all public functions/methods in a file
- Find all classes/structs/interfaces
- Correctly distinguish methods from free functions
- File symbol exists for every source file

### B: Relationship Accuracy
- Direct function calls within same file
- Cross-file calls (requires import resolution)
- Import statements resolve to correct files
- Inheritance chains are correct
- Methods have Defines edges from their class

### C: Impact Analysis
- "What calls function X?" (1 hop) - must match known callers
- "If I change class Y, what files are affected?" (2-3 hops)
- Negative cases: "Does anything call unused_helper?" - expected: nothing

### D: Language-Specific Edge Cases (10 per language)

**Python:** decorators, self.x.method(), relative imports, __init__.py, multiple inheritance, comprehension calls, *args/**kwargs, Protocol classes, type-annotated calls, dynamic imports

**TypeScript:** implements, generics, type imports, re-exports, barrel files, decorators, arrow functions, this.method(), namespace imports, overloads

**JavaScript:** CommonJS/ESM, module.exports, destructured require, prototype methods, class fields, dynamic require, default/named exports, callbacks

**Rust:** impl blocks, impl Trait for Struct, use crate::, mod hierarchy, pub use re-exports, derive macros, generics, trait method calls, Self::new() vs self.method(), nested modules

**Go:** receiver methods, implicit interfaces, struct embedding, package functions, init(), goroutine calls, internal packages, dot/blank imports, embedded method sets

**Ruby:** include/extend mixins, require/require_relative, attr_accessor, blocks/procs, method_missing, class reopening, module namespaces, Rails DSL, super calls, singleton methods

**Java:** extends/implements, inner classes, anonymous classes, annotations, generics, wildcard imports, static imports, interface defaults, enum methods, lambdas

**Kotlin:** extension functions, companion objects, data classes, object declarations, sealed classes, delegated properties, coroutine builders, inline functions, type aliases, when expressions

## Test Corpus

### Small (validate basics)

| Language | Project | Files | Tag | Why |
|----------|---------|-------|-----|-----|
| Python | Click | 17 | 8.3.1 | Deep class hierarchy (5+ levels), decorators |
| TypeScript | Hono core | ~30 | v4.12.8 | Class extends, generics, barrel exports |
| JavaScript | Axios | 61 | v1.9.0 | ES classes, extends, adapter pattern |
| Rust | bat | 40 | v0.26.1 | Traits, impl blocks, module hierarchy |
| Ruby | Sidekiq | 57 | v8.1.1 | Module mixins, require chains, namespaces |
| Go | Cobra | 19 | v1.10.2 | Struct methods, implicit interfaces |
| Java | Gson | 86 | gson-parent-2.12.1 | TypeAdapter hierarchy, generics, factories |
| Kotlin | Exposed core | 79 | 0.61.0 | Extension functions, sealed classes, DSL |

### Medium (validate cross-file resolution + scale)

| Language | Project | Files | Tag | Why |
|----------|---------|-------|-----|-----|
| Python | Rich | 100 | v14.3.3 | Protocol classes, ABC, deep import chains |
| TypeScript | NestJS core | 251 | v11.1.0 | Decorators, DI, abstract classes, interfaces |
| JavaScript | Webpack | 556 | v5.99.9 | Deep inheritance, require chains, plugins |
| Rust | Axum | 58 | axum-v0.8.8 | Trait-heavy, generics, cross-crate use |
| Ruby | Devise | 70 | v5.0.3 | ActiveSupport::Concern, module chains |
| Go | Gin | 58 | v1.12.0 | Interface implementations, struct embedding |
| Java | Javalin | 180 | javalin-parent-7.1.0 | Mixed Java+Kotlin, handler hierarchy |
| Kotlin | Detekt | ~120 | v1.23.8 | Abstract hierarchy, visitor pattern, multi-module |

## Ground Truth Format

TOML files, one per test project. ~100-150 entries each (curated, not exhaustive).

```toml
[meta]
language = "python"
project = "click"
version = "8.3.1"
commit = "abc123"

# Key symbols (20-40 per project)
[[symbols]]
file = "src/click/core.py"
name = "Command"
kind = "Class"

[[symbols]]
file = "src/click/core.py"
name = "Command.invoke"
kind = "Method"

# Critical relationships (30-50 per project)
[[relationships]]
kind = "Inherits"
source = "src/click/core.py::Group"
target = "src/click/core.py::Command"
tier = "must_find"

[[relationships]]
kind = "Calls"
source = "src/click/decorators.py::command"
target = "src/click/core.py::Command"
tier = "should_find"

# Impact queries (10-20 per project)
[[impact_queries]]
question = "What calls Command.invoke?"
target = "src/click/core.py::Command.invoke"
direction = "upstream"
expected = ["src/click/core.py::Group.invoke"]
hops = 1

# Edge cases (language-specific)
[[edge_cases]]
pattern = "decorated_function"
file = "src/click/decorators.py"
pass_condition = "symbol_exists"
expected_symbol = "src/click/decorators.py::command"
```

## Automation

```
engram-test-harness (Rust binary)
  |
  +-- Load ground truth TOML
  +-- Parse test project with Engram
  +-- Diff: expected vs actual
  +-- Score: per-dimension percentages
  +-- Report: table with grades
```

**Matching rules:**
- Symbols match on: file path + name + kind (line number is informational)
- Relationships match on: kind + source + target
- Impact queries: set intersection, report precision + recall per query

**Regression detection:** Store previous scores. Any dimension dropping >5 points = warning. Any language below C = blocks merge.

## Implementation Order

1. Clone + pin the 8 small test projects (git submodules)
2. Build the test harness binary (TOML loader + diff engine + scorer)
3. Create ground truth for Python (Click) and Rust (bat) first
4. Run, score, fix parser bugs
5. Expand to remaining 6 small projects
6. Add medium projects for cross-file stress testing
