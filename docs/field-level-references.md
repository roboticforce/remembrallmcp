# Field-Level Reference Tracking - Design

> Status: Proposed. Research-backed design for adding language-agnostic field/attribute/property
> reference tracking and impact analysis to RemembrallMCP.

## Problem

RemembrallMCP is symbol-based. It indexes things with their own callable identity: functions,
classes, methods. It does **not** index class/struct members that are plain data fields, and it
has no concept of a "reference to a field" distinct from a call/import relationship.

This means it cannot answer "who references the `amount` column?" - and a large fraction of real
blast-radius questions in ORM-heavy codebases are exactly that. Django model fields like `amount`
and `revenue_recognition_date` are class attributes, not callable symbols, so they fall through
the tree-sitter `Function`/`Class`/`Method` captures. Worse, the actual reference vector in those
codebases is a **string literal inside a DSL call** (`Sum("amount")`, `.filter(amount__gt=1)`),
which no language server resolves either.

This doc designs a universal abstraction that covers all 13 supported languages and, uniquely,
resolves the string-literal DSL references that LSP servers refuse to build.

## Research summary

Four research streams were combined (SCIP/LSIF/LSP standards, per-language-server behavior,
string-literal/DSL tooling, cross-language field-reference taxonomy, and our own graph model).

Key findings:

- **No existing standard models string-literal field references.** SCIP has a `StringLiteral`
  SyntaxKind for highlighting only. LSP `textDocument/references` defines only the wire shape;
  what counts as a reference is server discretion, and LSP has no way to advertise a
  non-identifier reference kind (microsoft/language-server-protocol#1911).
- **No production language server resolves DSL/reflection string references.** rust-analyzer,
  pyright, gopls, jdtls, and Kotlin LSP resolve AST identifier occurrences only.
  `getattr(obj,"field")`, `Sum("field")`, `getDeclaredField("field")`, `FieldByName("Field")`
  are unresolved everywhere. TypeScript is the lone exception, and only for `obj["prop"]`
  bracket access where the string is syntactically a property key.
- **Stack graphs** (GitHub's language-agnostic name resolver, archived Sep 2025) explicitly do
  not handle `getattr(obj,"field")` - there is no syntactic `.` node to generate from.
- **The dominant industry pattern** (JPA static metamodel, SQLAlchemy 2.0 `Mapped`, Prisma
  codegen, django-stubs mypy plugin) is to *eliminate* the string reference via codegen-typed
  mirrors, not resolve it. Each is bespoke per language+framework.

**Conclusion:** the borrowable abstractions are the *symbol* model and the *role/occurrence*
model. The string-literal binding layer is novel work and is our differentiator. No LSP server
will build it because LSP cannot represent it.

### What we borrow

- **SCIP descriptor grammar** - fields are first-class symbols scoped under their enclosing
  type via the `Term` suffix (`pkg mod Class#field.`). We mirror this as `parent_symbol_id` +
  a stable string `moniker`.
- **SCIP Occurrence roles** - `Definition`, `ReadAccess`, `WriteAccess`. Read/write matters
  for fields (rust-analyzer classifies this).
- **SCIP Relationship booleans** - `is_implementation`/`is_reference` for override chains, so
  "find references on an interface method" includes overrides without faking textual
  occurrences.
- **LSIF `moniker`** - a stable, position-independent, scheme-qualified identifier for a
  symbol. Used as the cross-pass join key for the DSL resolver and for dedup across reindex.
- **LSP `SymbolKind`** - `Property (7)`, `Field (8)`, `EnumMember (22)` as the canonical
  cross-language field taxonomy.
- **tree-sitter for extraction** (the "sqlshield pattern") - tree-sitter is the universal
  substrate for pulling string literals out of source across languages.
- **Tartu abstract-string algebra** (Annamaa, 2010) - bounded interprocedural constant
  propagation of string values, the theoretical foundation for resolving non-literal-but-
  constant field names like `name = "amount"; Sum(name)`.

### What we invent

- An `occurrences.origin` enum recording *how* a reference was found
  (`ast_identifier`, `bracket_string`, `dsl_string`, `reflection_string`, `attribute_string`,
  `macro`). No standard has this. It lets impact reports distinguish "definitely used" from
  "string match in a DSL call".
- A data-driven **DSL-call registry** mapping `(language, callee_pattern) -> field_arg_positions`.
- A **field-definition provider** trait (one impl per supported DSL) emitting
  `(scope_id, field_name, location, aliases)` tuples.
- A **field "surfaces" model**: a field has N resolvable names (source name + json tag + gorm
  column + serde rename + DB column). The resolver binds against all surfaces; aliases can be
  computed (case conventions like `rename_all = "camelCase"`), not just literal.
- **Impact-analysis traversal that follows field-reference edges** alongside `Calls`/`Imports`.
  This is novel for string-literal refs; SQLPrism/rawsql-ts do column-level impact for SQL only.

## The 7-kind taxonomy

Every field reference across the 13 supported languages (and beyond) maps onto one of these.
Resolution class in parens: **A** = tree-sitter alone (same-file, direct); **B** = light
type/scope inference (cross-file direct); **C** = framework/DSL-aware resolver;
**D** = receiver-type inference.

| # | Kind | Shape | Languages | Class |
|---|---|---|---|---|
| 1 | DirectMemberAccess | `obj.field` / `obj->field` / `obj?.field` / `s.0` | all | A / B |
| 2 | Destructure | `{field} = obj` / `let S{field} = s` / `case C(field) =>` / `auto [field] = s` | TS, JS, Rust, Kotlin, Swift, Scala, C++, Python, Ruby(hash) | A / B |
| 3 | ComputedKeyAccess | `obj["field"]` / `obj[field]` / `obj->{"field"}` | TS, JS, PHP, Python | A (literal) / D (var) |
| 4 | StringLiteralDSL | `Sum("field")`, `.filter(field=)`, `df["field"]`, `Decoder.forProduct1("field")`, `NLOHMANN_DEFINE_TYPE_NONINTRUSIVE(T, field)` | all (framework-dependent) | C |
| 5 | Reflection | `getattr(o,name)`, `cls.getField("x")`, `reflect.FieldByName("X")`, `o.send(:x)`, `Mirror(o).children` | Python, Go, Java, Kotlin, Ruby, C#, Swift, Scala, PHP | D (+C if literal arg) |
| 6 | SymbolKey | `hash[:field]` (Ruby), `h.fetch(:field)` | Ruby primarily | A (literal) / D (var) |
| 7 | SchemaTag | `#[serde(rename="x")]`, `` `json:"x"` ``, `[JsonProperty("x")]`, `@SerialName("x")`, `CodingKeys case x = "x_key"` | Rust, Go, Java, C#, Kotlin, Swift, PHP, C++ | C |

Two conceptual distinctions the taxonomy forces:

- **StringLiteralDSL vs SchemaTag.** StringLiteralDSL is a *usage-site* string that names a
  field in a query/call (`Sum("amount")`). SchemaTag is a *definition-site* tag that binds a
  field to an external name (`#[serde(rename = "amount")]`). The resolver needs both:
  SchemaTag creates the alias mapping; StringLiteralDSL consumes it. This is the link that
  makes `Sum("amount")` resolve to the ORM field `amount`.
- **ComputedKeyAccess with a literal normalizes to DirectMemberAccess** at the resolver layer
  (`obj["x"]` == `obj.x`). tree-sitter cannot equate them; the resolver should.
- **SymbolKey (Ruby `hash[:field]`) is NOT an object-field reference.** It binds to a hash key.
  Track it as its own kind so Ruby code is modeled without polluting field-reference queries.

tree-sitter alone reliably yields: `kind`, `name_token` (for kinds 1, 2, literal 3, literal 4,
literal 6, 7). It cannot yield: cross-file binding (B), dynamic-key resolution (D), framework
alias mapping (C), or any Reflection binding (D). So the parser layer emits *unresolved*
reference edges tagged with `kind + name_token + resolution_class`; a separate resolver pass
promotes them to resolved edges.

## Architecture

Two-layer separation that fits the existing two-phase walker (`parser/walker.rs`).

### Layer 1 - Parser (universal): emit unresolved field-reference edges

For every field reference found, emit an occurrence row with:
- `kind` - one of the 7 kinds above.
- `name_token` - the literal/symbol/identifier text naming the field (when present).
- `resolution_class` - A / B / C / D.
- `name_is_dynamic` - boolean (literal vs variable-derived).
- `framework` - for kinds 4 and 7 (e.g. `"django"`, `"serde"`, `"gorm"`).
- `resolved = false`, `confidence` per class (A/B: 1.0; C: 0.6-0.8; D: 0.5 or unresolved).

### Layer 2 - Resolver (framework-aware): promote unresolved edges to resolved

Three sub-resolvers, each handling its resolution class:

- **A/B resolver** (reuses the existing two-phase walker): import graph + symbol table. Handles
  DirectMemberAccess, Destructure, literal ComputedKeyAccess, literal SymbolKey. Extends
  `synthetic_to_real` (walker.rs:174-177) to include `Field` symbols so cross-file field-name
  resolution works (today it is built only from `Function | Method | Class`).
- **C resolver** (the novel part): DSL-call registry + field-definition providers. Recognizes
  `getattr(o,"x")`, `Sum("x")`, `.filter(x=)`, `FieldByName("F")`, `send(:x)`,
  `@JsonProperty("x")`, `json:"x"`, `#[serde(rename="x")]`, etc.; extracts the string; binds it
  to a Field symbol via the provider. This is where `Sum("amount")` finally resolves to the
  Django model field.
- **D resolver**: receiver-type inference for reflection/dynamic keys. Where the receiver type
  is known, bind; otherwise emit `resolved = false, origin = reflection_string` rather than
  silently miss.

## Data model changes

Current model (`graph/types.rs`):
- `SymbolType {File, Function, Class, Method}` - no Field/Property.
- `RelationType {Calls, Imports, Defines, Inherits, UsesType}` - no reference/access edge.
- Flat symbols, no parent scope, no per-occurrence record.
- `relationships` PK `(source_id, target_id, rel_type)` - collapses read/write on the same
  pair, no per-edge metadata.

### Symbol extensions (`types.rs:7-27`)

- Add `SymbolType::Field`, `Property`, `EnumMember` (map LSP `SymbolKind` per language).
- Add `parent_symbol_id: Option<Uuid>` to `Symbol` so a field is scoped under its enclosing
  type. This is the SCIP `#Type field.` concept and makes "member" first-class.
- Add `moniker: Option<String>` to `Symbol` (SCIP-style descriptor string). The cross-pass join
  key for the DSL resolver and the dedup key across reindex. More portable and debuggable than
  the UUID for that join.

### New `occurrences` table (distinct from `relationships`)

The `relationships` PK collapses read/write on the same pair and has no per-site metadata.
Field impact needs per-site records. Borrow SCIP's Occurrence:

```sql
CREATE TABLE IF NOT EXISTS {schema}.occurrences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    symbol_id UUID NOT NULL REFERENCES {schema}.symbols(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    start_line INTEGER,
    start_col INTEGER,
    end_line INTEGER,
    end_col INTEGER,
    role TEXT NOT NULL,          -- definition | read | write | reference
    origin TEXT NOT NULL,        -- ast_identifier | bracket_string | dsl_string | reflection_string | attribute_string | macro
    kind TEXT NOT NULL,          -- the 7-kind taxonomy
    framework TEXT,              -- nullable; for kinds 4 and 7
    confidence REAL NOT NULL DEFAULT 1.0,
    resolved BOOLEAN NOT NULL DEFAULT false,
    project TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX idx_occurrences_symbol ON {schema}.occurrences (symbol_id);
CREATE INDEX idx_occurrences_file ON {schema}.occurrences (file_path);
```

The `origin` enum is the abstraction no existing standard has. It lets impact reports
distinguish "definitely used" (AST identifier) from "string match in a DSL call"
(`dsl_string`, lower confidence).

### Relationship extension (`types.rs:62-69`)

- Add `RelationType::References` as the symbol-to-symbol *aggregate* edge (e.g.
  `function -> field`) for blast-radius traversal.
- Add `Implements` / `Overrides` (SCIP's `is_implementation` pair) so "find references on an
  interface method/abstract field" includes overrides without faking textual occurrences.

### Impact CTE implications (`store.rs:307-373`)

- The recursive CTE traverses all relationship types indiscriminately, so a `References` edge
  automatically flows into blast-radius. That is the desired behavior: it is how
  "breakage at `Sum("amount")` is on the critical path of both signal detectors" gets *proven*
  rather than asserted.
- Confidence is multiplicative along the chain. Field-reference edges at 0.6-0.8 (DSL) will
  decay downstream scores. Make this a knob: add an optional `rel_type` filter to the CTE so
  users can ask "blast radius via calls only" vs "via calls + field references".
- `add_relationships_batch` dedup (`store.rs:239`) keys on `(source_id, target_id, rel_type)`.
  Fine for the aggregate `References` edge; the per-site read/write distinction lives in
  `occurrences`, not `relationships`.

## Critical gotchas (must handle before/in Phase 1)

- **`FromStr` impls.** `SymbolRow::into_symbol` and `ImpactRow` use
  `unwrap_or(SymbolType::Function)` / `unwrap_or(RelationType::Calls)` (`store.rs:717`, `755`).
  Extending the enums without extending both `FromStr` impls means stored `"field"` /
  `"references"` rows silently misparse. Fix `types.rs:40-51` and `types.rs:83-94` first.
- **Two parallel indexing paths.** The DB-backed `Indexer` (`indexer.rs`, via `CodeParser`) and
  the `walker` two-phase path are separate; only the walker does cross-file resolution.
  Confirm which path `remembrall_index` actually uses before assuming changes take effect, and
  route both through the same field-capture + resolution logic.
- **`find_enclosing_function`** (`python.rs:933`, `typescript.rs:976`) only considers
  `Function | Method`; field references at class-body or module scope attribute to the file.
  Widen to include `Class` / `Field` if class-body refs should source from the enclosing class.
- **`generate_tour`** (`store.rs:547-558`) filters `symbol_type != 'file'`; new Field symbols
  will appear in tour symbol lists. Inert for the topological sort (built on `imports` only),
  but verify the output is still readable.
- **Parser capture gaps.** Python `collect_definitions` does not match `"assignment"` at class
  body scope (`python.rs:401-474`); TS `public_field_definition` only emits a symbol when the
  value is a function (`typescript.rs:302-356`). Both need new arms to capture plain data
  fields. Rust/Go/Java/Kotlin/Ruby struct/class fields need parser work too.

## The DSL resolver (the novel layer)

No standard helps here. Design it data-driven, not hardcoded:

1. **DSL-call registry** - a config mapping `(language, callee_pattern) -> field_arg_positions`.
   Examples: `(python, "django.db.models.Sum") -> [arg0]`, `(python, "getattr") -> [arg1]`,
   `(go, "reflect.*FieldByName") -> [arg0]`, `(ruby, "send") -> [arg0]`. Ship defaults for the
   common ones; make it user-extensible.
2. **Field-definition providers** - a trait, one impl per supported DSL, emitting
   `(scope_id, field_name, location, aliases)` tuples. Django provider introspects model
   class attributes + `_meta.fields`; Prisma provider reads the schema; JPA provider reads
   entity metadata; Go-tags provider parses struct tags; serde provider parses
   `#[serde(rename)]`. `GraphStore` is the natural home: Field symbols + their alias surfaces
   are just more symbol/occurrence rows.
3. **Bounded interprocedural constant propagation** for strings (Tartu algebra) - so
   `field_name = "amount"; Sum(field_name)` resolves. Bound it to effectively-immutable locals
   to keep it cheap.
4. **Graceful degradation** - dynamic keys, `Object.keys`, `send(:field)` with a variable:
   emit `resolved = false, origin = reflection_string` rather than silently miss. The impact
   report then shows "1 unresolved dynamic reference" instead of a false clean bill.

## Phased rollout

### Phase 1 - AST parity (matches rust-analyzer/pyright/gopls on AST refs)

- `SymbolType::Field` / `Property` + `parent_symbol_id` + `moniker` on `Symbol`.
- Extend both `FromStr` impls (`types.rs:40-51`, `types.rs:83-94`).
- Parsers capture class-body fields as child symbols: Python `assignment` at class scope; TS
  `public_field_definition` when value is not a function; Rust/Go/Java/Kotlin/Ruby struct and
  class fields.
- `occurrences` table with `origin = ast_identifier` for DirectMemberAccess and Destructure.
- Extend `synthetic_to_real` (`walker.rs:174-177`) to include `Field`.
- `RelationType::References` aggregate edge wired into the impact CTE.
- `remembrall_impact` and a new `remembrall_references` (or extended `lookup_symbol`) tool to
  surface field references.

Outcome: field-level "find references" and blast-radius for the common `obj.field` case across
all 13 languages.

### Phase 2 - Bracket + computed-key parity with TypeScript

- Normalize literal `obj["field"]` to DirectMemberAccess.
- Capture destructuring across the languages that have it.

### Phase 3 - The DSL-string layer (the differentiator)

- DSL-call registry + field-definition providers + alias surfaces + bounded string constant
  propagation.
- Ship providers for Django, Pydantic, SQLAlchemy, Prisma, serde, Go tags, JPA/Jackson,
  ActiveRecord.
- `origin = dsl_string` / `attribute_string` occurrences with reduced confidence.

This is where `Sum("amount")` resolves to the field, the thing no LSP server does.

### Phase 4 - Reflection and computed keys (best-effort)

- Receiver-type inference where feasible.
- Explicit "unresolved - dynamic" reporting tier otherwise.

## Performance

The design is fast by construction, but only because of three specific choices. Done naively,
two parts would be slow.

**Fast by construction:**
- Parse: tree-sitter is ~MB/s; field capture adds ~10-30% more AST node visits over what we
  skip today. Negligible.
- Field symbols: one row each, indexed. ~50k fields in a 1M-LOC monorepo is trivial for
  Postgres.
- Occurrence queries: `occurrences` is indexed on `symbol_id` and `file_path`; "who references
  this field" is an indexed lookup, not a scan.
- No embeddings for fields - they are graph symbols, not memories. No fastembed cost added.
- Incremental reindex: fields and occurrences participate in the existing mtime-based path
  (`indexer.rs`) via the per-file `remove_file` + re-insert cascade.

**Choice 1 - the DSL resolver runs inside the parse pass, not as a separate scan.**
The slow version scans every file once per registry pattern (O(patterns x files)). The fast
version: the parser already visits every `call_expression` once, so it does an O(1) HashMap
lookup of the callee against the registry during that visit and extracts the string arg in
place. Binding is another HashMap lookup. Zero extra passes. This is the single biggest perf
lever and is mandatory.

**Choice 2 - constant propagation is bounded.**
Full interprocedural string propagation is slow. Bound it to effectively-immutable locals
within a single function/file. This catches `name = "amount"; Sum(name)` without a
whole-program dataflow analysis.

**Choice 3 - field references do NOT fan out like Calls.**
Today the walker fans out `Calls` one edge per candidate when a name is ambiguous
(`walker.rs:234-243`). That is fine for calls. For fields it is both slow and wrong: a field
named `name`/`id`/`amount` exists on many classes, so 1000 sites x 50 candidate classes would
produce 50,000 edges and the impact CTE would traverse all of them.

Policy:
- Receiver type resolvable (class B/D) -> one edge to the correct field, confidence 1.0.
- Receiver type not resolvable and name ambiguous -> occurrence `resolved = false,
  ambiguous = true`; emit at most one low-confidence aggregate edge per (source, field-name),
  not per candidate (or no symbol-to-symbol edge, just the unresolved occurrence).
- Fallback: cap candidates (e.g. top-3 by usage frequency).

This keeps the graph sparse and the CTE fast, and "ambiguous, needs type info" is a more
honest answer than N noisy edges.

**Impact CTE (`store.rs:307-373`):** already depth-bounded (the `$2` depth param) with a cycle
guard. More `References` edges means more traversal, but still bounded by depth x branching
factor. The multiplicative confidence is one float multiply per step. The only way the CTE
gets slow is a "hub" field referenced from thousands of sites at depth 1 - which Choice 3
prevents, because high-fan-out common names do not get thousands of resolved edges.

## Explicitly deferred

- Purely dynamic refs (`Object.keys`, `send` with a variable, computed keys with arbitrary
  variables) - report as unresolved, do not fake.
- Languages/DSLs without a schema model.
- Becoming a full LSP server or replicating rust-analyzer's HIR. tree-sitter + the two-phase
  resolver is enough for the AST layer; the differentiator is the DSL layer LSP refuses to
  build.

## Headline

The universal abstraction exists at the symbol/occurrence/role level (borrow from SCIP). At
the field-reference-edge level we invent one new thing: an `occurrences.origin` enum plus a
data-driven DSL-call registry and field-definition providers. The 7-kind taxonomy + A/B/C/D
resolution classes give the language-agnostic skeleton; the providers give the per-framework
muscle. Every language maps onto the same 7 kinds; only the providers differ.

## Sources

- SCIP spec: https://raw.githubusercontent.com/sourcegraph/scip/main/docs/scip.md
- SCIP repo: https://github.com/sourcegraph/scip
- LSIF spec: https://github.com/microsoft/language-server-protocol/blob/main/indexFormat/specification.md
- LSP 3.17 spec: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/
- rust-analyzer references: https://rust-lang.github.io/rust-analyzer/ide/references/index.html
- pyright referencesProvider: https://github.com/microsoft/pyright/blob/main/packages/pyright-internal/src/languageService/referencesProvider.ts
- pyright `getattr` discussion: https://github.com/microsoft/pyright/discussions/2737
- gopls navigation + implied-refs: https://github.com/golang/go/issues/66356
- tsc findAllReferences (string-literal path): https://github.com/microsoft/TypeScript/blob/main/src/services/findAllReferences.ts
- jdtls `includeAccessors`: https://github.com/eclipse/eclipse.jdt.ls/issues/1548
- Stack graphs docs: https://github.github.io/stack-graph-docs/
- Stack graphs repo (archived): https://github.com/github/stack-graphs/tree/main/tree-sitter-stack-graphs
- Scope graphs (TU Delft): https://pl.ewi.tudelft.nl/research/projects/scope-graphs/
- Tartu embedded-DSL static analysis (Annamaa, 2010): https://kodu.ut.ee/~varmo/tday-andu/annamaa-slides.pdf
- Semgrep pattern syntax: https://semgrep.dev/docs/writing-rules/pattern-syntax
- Semgrep constant propagation: https://semgrep.dev/docs/writing-rules/data-flow/constant-propagation
- ast-grep pattern syntax: https://ast-grep.github.io/guide/pattern-syntax.html
- Comby: https://comby.dev/
- tree-sitter predicates: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/3-predicates-and-directives.html
- django-stubs: https://github.com/typeddjango/django-stubs
- SQLAlchemy 2.0 typed Mapped: https://docs.sqlalchemy.org/en/20/orm/extensions/mypy.html
- Prisma type safety: https://www.prisma.io/docs/orm/prisma-client/type-safety
- serde field attrs: https://serde.rs/field-attrs.html
- Go structtag analyzer: https://github.com/golang/tools/blob/master/go/analysis/passes/structtag/structtag.go
- Go reflect: https://pkg.go.dev/reflect
- Hibernate JPA metamodel: https://docs.hibernate.org/stable/jpamodelgen/reference/en-US/html_single/
- Jackson `@JsonProperty`: https://github.com/FasterXML/jackson-annotations/blob/master/src/main/java/com/fasterxml/jackson/annotation/JsonProperty.java
- Kotlin reflection: https://kotlinlang.org/docs/reflection.html
- sqlshield: https://github.com/davidsmfreire/sqlshield
- rawsql-ts query-uses: https://github.com/mk3008/rawsql-ts/blob/main/docs/guide/query-uses-overview.md
- SQLPrism: https://github.com/darkcofy/sqlprism