//! Tree-sitter based Ruby parser.
//!
//! Extracts symbols and relationships from a single Ruby source file.
//!
//! Handles Rails conventions: controllers, models, mixins, scopes,
//! callbacks, associations, and nested module/class definitions.
//!
//! # Note on type annotations
//! Ruby is dynamically typed. Standard Ruby has no type annotation syntax in
//! the language itself (RBS type signatures live in separate `.rbs` files, not
//! inline in `.rb` source). The tree-sitter-ruby grammar therefore has no type
//! annotation nodes to extract, so this parser does not emit UsesType
//! relationships.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Parse a Ruby file and extract symbols and relationships.
///
/// - `file_path`  - canonical path string stored on each symbol
/// - `source`     - raw UTF-8 source text
/// - `project`    - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_ruby_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("failed to load Ruby grammar");

    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!("tree-sitter failed to parse {file_path}");
        return FileParseResult::default();
    };

    let source_bytes = source.as_bytes();
    let root = tree.root_node();

    let mut ctx = ParseContext {
        file_path,
        project,
        file_mtime,
        result: FileParseResult::default(),
        name_to_id: HashMap::new(),
        imported_names: HashSet::new(),
    };

    // Create the file-level symbol first.
    let file_symbol_id = Uuid::new_v4();
    ctx.result.symbols.push(Symbol {
        id: file_symbol_id,
        name: file_path.to_string(),
        symbol_type: SymbolType::File,
        file_path: file_path.to_string(),
        start_line: Some(1),
        end_line: Some(source.lines().count() as i32),
        language: "ruby".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
        layer: None,
    });

    // First pass: collect requires/require_relative so call scoring can use them.
    let mut cursor = root.walk();
    collect_requires(&root, source_bytes, &mut ctx, &mut cursor);

    // Second pass: collect class, module, and method definitions.
    let mut cursor2 = root.walk();
    collect_definitions(
        &root,
        file_symbol_id,
        None, // no enclosing class at top level
        None, // no namespace prefix at top level
        source_bytes,
        &mut ctx,
        &mut cursor2,
    );

    // Third pass: collect call expressions.
    let mut cursor3 = root.walk();
    collect_calls(&root, source_bytes, &mut ctx, &mut cursor3);

    ctx.result
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

struct ParseContext<'a> {
    file_path: &'a str,
    project: &'a str,
    file_mtime: DateTime<Utc>,
    result: FileParseResult,
    /// name -> symbol UUID for all symbols defined in this file.
    name_to_id: HashMap<String, Uuid>,
    /// Module/class names that were required or included - used for confidence scoring.
    imported_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Require collection
// ---------------------------------------------------------------------------

/// Walk top-level statements looking for `require` and `require_relative` calls.
///
/// These appear as `call` nodes at the program level with method names
/// "require" or "require_relative".
fn collect_requires<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        // Requires can be bare `require "foo"` or wrapped in `call` nodes.
        if child.kind() == "call" {
            if let Some(raw) = extract_require(&child, source) {
                record_require(raw, ctx);
            }
        }
        // Also descend one level for things like `Bundler.require` blocks.
        // We don't recurse deeply - requires are almost always at file scope.
    }
}

/// If `node` is a `require` or `require_relative` call, return the string argument.
fn extract_require(node: &Node<'_>, source: &[u8]) -> Option<String> {
    // tree-sitter-ruby: call node has fields: receiver (optional), method, arguments.
    // For bare `require "foo"`, method is an identifier node with text "require".
    let method_node = node.child_by_field_name("method")?;
    let method_name = node_text(&method_node, source);

    if method_name != "require" && method_name != "require_relative" {
        return None;
    }

    // Arguments node contains the string literal.
    let args_node = node.child_by_field_name("arguments")?;
    let mut cursor = args_node.walk();
    for arg in args_node.named_children(&mut cursor) {
        let text = node_text(&arg, source);
        // Strip surrounding quotes from string literals.
        let stripped = text
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();
        if !stripped.is_empty() {
            return Some(stripped);
        }
    }
    None
}

/// Record a require as a raw import and placeholder relationship.
fn record_require(path: String, ctx: &mut ParseContext<'_>) {
    let file_id = ctx.result.symbols[0].id;

    // Track the last component as an imported name for call scoring.
    let last = path.split('/').last().unwrap_or(&path);
    // Convert snake_case file names to CamelCase class names heuristically.
    let class_name = snake_to_camel(last);
    ctx.imported_names.insert(class_name);
    ctx.imported_names.insert(last.to_string());

    let is_relative = path.starts_with('.');
    let dot_count = if is_relative { 1usize } else { 0usize };

    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: path.clone(),
        is_relative,
        dot_count,
        module_path: path.clone(),
    });

    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, path.as_bytes());
    ctx.result.relationships.push(Relationship {
        source_id: file_id,
        target_id,
        rel_type: RelationType::Imports,
        confidence: if is_relative { 0.3 } else { 0.5 },
    });
}

/// Naively convert a snake_case string to CamelCase.
/// `"application_controller"` -> `"ApplicationController"`.
fn snake_to_camel(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect()
}

/// Convert a CamelCase string to snake_case.
/// `"ApplicationController"` -> `"application_controller"`.
fn camel_to_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.extend(c.to_lowercase());
    }
    result
}

/// Convert a Ruby constant path to a Zeitwerk-style file path.
/// `"Api::V1::CandidatesController"` -> `"api/v1/candidates_controller"`.
fn zeitwerk_path(constant: &str) -> String {
    constant
        .split("::")
        .map(|seg| camel_to_snake(seg))
        .collect::<Vec<_>>()
        .join("/")
}

/// Generate a synthetic `RawImport` for a Zeitwerk-autoloaded constant.
///
/// Rails uses Zeitwerk which maps constant names to file paths by convention:
/// `SomeModule::SomeClass` -> `some_module/some_class.rb`. Since Ruby/Rails
/// has no explicit import statements, we infer them so the walker can resolve
/// file-to-file Import edges.
fn emit_zeitwerk_import(constant: &str, ctx: &mut ParseContext<'_>) {
    let path = zeitwerk_path(constant);
    if path.is_empty() {
        return;
    }

    let file_id = ctx.result.symbols[0].id;

    // Track names for call confidence scoring.
    ctx.imported_names.insert(constant.to_string());
    let short = constant.split("::").last().unwrap_or(constant);
    ctx.imported_names.insert(short.to_string());

    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: path.clone(),
        is_relative: false,
        dot_count: 0,
        module_path: path.clone(),
    });

    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, path.as_bytes());
    ctx.result.relationships.push(Relationship {
        source_id: file_id,
        target_id,
        rel_type: RelationType::Imports,
        confidence: 0.6, // inferred from Zeitwerk convention, not explicit require
    });
}

// ---------------------------------------------------------------------------
// Definition collection
// ---------------------------------------------------------------------------

/// Recursively walk the AST collecting class, module, and method definitions.
///
/// - `parent_id`       - the symbol UUID of the enclosing scope (file, class, or module)
/// - `enclosing_class` - `Some(uuid)` when inside a class/module body (for Method vs Function)
/// - `namespace`       - accumulated module/class name prefix for nested types (e.g. "App::")
fn collect_definitions<'a>(
    node: &Node<'a>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    namespace: Option<&str>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "class" => {
                let class_id =
                    process_class(&child, parent_id, namespace, source, ctx);
                if let Some(body) = child.child_by_field_name("body") {
                    let class_name = class_name_from_node(&child, source);
                    let new_ns = build_namespace(namespace, &class_name);
                    let mut inner = body.walk();
                    collect_definitions(
                        &body,
                        class_id,
                        Some(class_id),
                        Some(&new_ns),
                        source,
                        ctx,
                        &mut inner,
                    );
                }
            }
            "module" => {
                let module_id =
                    process_module(&child, parent_id, namespace, source, ctx);
                if let Some(body) = child.child_by_field_name("body") {
                    let module_name = module_name_from_node(&child, source);
                    let new_ns = build_namespace(namespace, &module_name);
                    let mut inner = body.walk();
                    collect_definitions(
                        &body,
                        module_id,
                        Some(module_id),
                        Some(&new_ns),
                        source,
                        ctx,
                        &mut inner,
                    );
                }
            }
            "method" => {
                // `def foo(args) ... end` - instance method
                let sym_id =
                    process_method(&child, parent_id, enclosing_class, false, source, ctx);
                // Recurse into method body for nested defs (rare but valid Ruby).
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(
                        &body,
                        sym_id,
                        None,
                        namespace,
                        source,
                        ctx,
                        &mut inner,
                    );
                }
            }
            "singleton_method" => {
                // `def self.foo(args) ... end` - class method
                let sym_id =
                    process_method(&child, parent_id, enclosing_class, true, source, ctx);
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(
                        &body,
                        sym_id,
                        None,
                        namespace,
                        source,
                        ctx,
                        &mut inner,
                    );
                }
            }
            "call" if enclosing_class.is_some() => {
                // Handle `attr_accessor :foo, :bar` etc. inside a class/module body.
                // These emit Method symbols for the generated accessor methods.
                process_attr_call(&child, parent_id, source, ctx);
            }
            _ => {
                // Keep descending into begin/end blocks, if/unless, rescue, etc.
                let mut inner = child.walk();
                collect_definitions(
                    &child,
                    parent_id,
                    enclosing_class,
                    namespace,
                    source,
                    ctx,
                    &mut inner,
                );
            }
        }
    }
}

/// Build a namespace prefix string for nested types.
fn build_namespace(existing: Option<&str>, name: &str) -> String {
    match existing {
        None => name.to_string(),
        Some(ns) => format!("{ns}::{name}"),
    }
}

/// Extract the short class name text from a `class` node.
fn class_name_from_node(node: &Node<'_>, source: &[u8]) -> String {
    node.child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string())
}

/// Extract the short module name text from a `module` node.
fn module_name_from_node(node: &Node<'_>, source: &[u8]) -> String {
    node.child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string())
}

/// Process a `class` node; emits a Class symbol, a Defines relationship,
/// and an Inherits relationship if a superclass is specified.
fn process_class(
    node: &Node<'_>,
    parent_id: Uuid,
    namespace: Option<&str>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let short_name = class_name_from_node(node, source);
    let qualified_name = match namespace {
        Some(ns) => format!("{ns}::{short_name}"),
        None => short_name.clone(),
    };

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    // Register both short and qualified names for cross-file call resolution.
    ctx.name_to_id.insert(short_name.clone(), id);
    ctx.name_to_id.insert(qualified_name.clone(), id);

    ctx.result.symbols.push(Symbol {
        id,
        name: qualified_name.clone(),
        symbol_type: SymbolType::Class,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "ruby".to_string(),
        project: ctx.project.to_string(),
        signature: Some(build_class_signature(node, &qualified_name, source)),
        file_mtime: ctx.file_mtime,
        layer: None,
    });

    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // INHERITS: `class Foo < Bar` or `class Foo < Some::Module`
    // tree-sitter-ruby field name for superclass is "superclass".
    // NOTE: tree-sitter-ruby includes the `<` operator in the superclass node text,
    // so we must strip it: "< Interrupt" -> "Interrupt".
    if let Some(superclass_node) = node.child_by_field_name("superclass") {
        let raw = node_text(&superclass_node, source);
        let base_name = raw.trim_start_matches('<').trim().to_string();
        if !base_name.is_empty() {
            // Use the last component for local name lookup (e.g. "Base" from "ActionController::Base").
            let short_base = base_name.split("::").last().unwrap_or(&base_name);
            let target_id = ctx
                .name_to_id
                .get(short_base)
                .copied()
                .or_else(|| ctx.name_to_id.get(&base_name).copied())
                .unwrap_or_else(|| {
                    Uuid::new_v5(&Uuid::NAMESPACE_OID, base_name.as_bytes())
                });

            let confidence = if ctx.name_to_id.contains_key(short_base)
                || ctx.name_to_id.contains_key(&base_name)
            {
                1.0
            } else if ctx.imported_names.contains(short_base) {
                0.8
            } else {
                0.5
            };

            ctx.result.relationships.push(Relationship {
                source_id: id,
                target_id,
                rel_type: RelationType::Inherits,
                confidence,
            });

            // Zeitwerk autoload: generate a synthetic import for the superclass
            // so the walker can resolve file-to-file Import edges.
            emit_zeitwerk_import(&base_name, ctx);
        }
    }

    // INHERITS (mixin): `include SomeModule` or `prepend SomeModule`
    // These appear as method calls inside the class body, handled in collect_definitions
    // via the call collection pass. We handle `include` / `prepend` / `extend` here
    // eagerly while we still have the class id in context.
    collect_mixin_includes(node, id, source, ctx);

    id
}

/// Scan a class/module node's body for `include`, `prepend`, and `extend` calls
/// and emit Inherits relationships for each.
fn collect_mixin_includes(
    class_node: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let Some(body) = class_node.child_by_field_name("body") else {
        return;
    };

    let mut cursor = body.walk();
    for stmt in body.children(&mut cursor) {
        if stmt.kind() != "call" {
            continue;
        }
        let method_name = stmt
            .child_by_field_name("method")
            .map(|n| node_text(&n, source))
            .unwrap_or_default();

        if !matches!(method_name.as_str(), "include" | "prepend" | "extend") {
            continue;
        }

        let Some(args) = stmt.child_by_field_name("arguments") else {
            continue;
        };

        let mut ac = args.walk();
        for arg in args.named_children(&mut ac) {
            let module_name = node_text(&arg, source);
            if module_name.is_empty() {
                continue;
            }

            // Track the module name for call scoring.
            ctx.imported_names.insert(module_name.clone());

            let short = module_name.split("::").last().unwrap_or(&module_name);
            let target_id = ctx
                .name_to_id
                .get(short)
                .copied()
                .or_else(|| ctx.name_to_id.get(&module_name).copied())
                .unwrap_or_else(|| {
                    Uuid::new_v5(&Uuid::NAMESPACE_OID, module_name.as_bytes())
                });

            let confidence = if ctx.name_to_id.contains_key(short)
                || ctx.name_to_id.contains_key(&module_name)
            {
                1.0
            } else {
                0.7 // included module - likely defined elsewhere in the project
            };

            ctx.result.relationships.push(Relationship {
                source_id: class_id,
                target_id,
                rel_type: RelationType::Inherits,
                confidence,
            });

            // Zeitwerk autoload: generate a synthetic import for the included module.
            emit_zeitwerk_import(&module_name, ctx);
        }
    }
}

/// Build a human-readable signature for a class definition.
fn build_class_signature(node: &Node<'_>, qualified_name: &str, source: &[u8]) -> String {
    if let Some(superclass) = node.child_by_field_name("superclass") {
        // tree-sitter-ruby includes the `<` in the superclass text; strip it.
        let raw = node_text(&superclass, source);
        let parent = raw.trim_start_matches('<').trim();
        format!("class {qualified_name} < {parent}")
    } else {
        format!("class {qualified_name}")
    }
}

/// Process a `module` node - modules act as namespaces and mixins in Ruby.
/// Emits a Class symbol (modules have no separate SymbolType).
fn process_module(
    node: &Node<'_>,
    parent_id: Uuid,
    namespace: Option<&str>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let short_name = module_name_from_node(node, source);
    let qualified_name = match namespace {
        Some(ns) => format!("{ns}::{short_name}"),
        None => short_name.clone(),
    };

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(short_name.clone(), id);
    ctx.name_to_id.insert(qualified_name.clone(), id);

    ctx.result.symbols.push(Symbol {
        id,
        name: qualified_name.clone(),
        symbol_type: SymbolType::Class, // modules map to Class; no Module type exists
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "ruby".to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("module {qualified_name}")),
        file_mtime: ctx.file_mtime,
        layer: None,
    });

    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    id
}

/// Process a `method` or `singleton_method` node.
///
/// - `is_singleton` - true for `def self.foo`
fn process_method(
    node: &Node<'_>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    is_singleton: bool,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let symbol_type = if enclosing_class.is_some() {
        SymbolType::Method
    } else {
        SymbolType::Function
    };

    let signature = build_method_signature(node, &name, is_singleton, source);
    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "ruby".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
        layer: None,
    });

    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    id
}

/// Process `attr_accessor`, `attr_reader`, `attr_writer` calls inside a class body.
///
/// Emits one Method symbol per named accessor so that ground-truth checks like
/// `Sidekiq::Launcher#managers` resolve to a real symbol.
fn process_attr_call(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let method_name = match node.child_by_field_name("method") {
        Some(n) => node_text(&n, source),
        None => return,
    };

    if !matches!(
        method_name.as_str(),
        "attr_accessor" | "attr_reader" | "attr_writer"
    ) {
        return;
    }

    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };

    let line = node.start_position().row as i32 + 1;
    let mut ac = args.walk();
    for arg in args.named_children(&mut ac) {
        let arg_text = node_text(&arg, source);
        // Arguments are symbols like `:managers` or bare identifiers.
        let name = arg_text.trim_start_matches(':').to_string();
        if name.is_empty() || name.starts_with('"') || name.starts_with('\'') {
            continue;
        }

        let id = Uuid::new_v4();
        ctx.name_to_id.insert(name.clone(), id);
        ctx.result.symbols.push(Symbol {
            id,
            name: name.clone(),
            symbol_type: SymbolType::Method,
            file_path: ctx.file_path.to_string(),
            start_line: Some(line),
            end_line: Some(line),
            language: "ruby".to_string(),
            project: ctx.project.to_string(),
            signature: Some(format!("{method_name} :{name}")),
            file_mtime: ctx.file_mtime,
            layer: None,
        });

        ctx.result.relationships.push(Relationship {
            source_id: parent_id,
            target_id: id,
            rel_type: RelationType::Defines,
            confidence: 1.0,
        });
    }
}

/// Build a human-readable method signature.
fn build_method_signature(
    node: &Node<'_>,
    name: &str,
    is_singleton: bool,
    source: &[u8],
) -> String {
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source));

    let prefix = if is_singleton { "def self." } else { "def " };

    match params {
        Some(p) => format!("{prefix}{name}{p}"),
        None => format!("{prefix}{name}"),
    }
}

// ---------------------------------------------------------------------------
// Call collection
// ---------------------------------------------------------------------------

/// Walk the entire tree looking for method call expressions.
fn collect_calls<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        if child.kind() == "call" {
            process_call(&child, source, ctx);
        }
        let mut inner = child.walk();
        collect_calls(&child, source, ctx, &mut inner);
    }
}

/// Classify how a callee was referenced, for confidence scoring.
#[derive(Debug, PartialEq)]
enum CalleeKind {
    /// Plain bare call: `foo(...)`
    Bare,
    /// `self.method(...)` or chained self call
    SelfChain,
    /// Any other receiver.method pattern
    Receiver,
}

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let (callee_name, callee_kind) = extract_callee_name(node, source);
    if callee_name.is_empty() {
        return;
    }

    // Skip Rails DSL class-body calls that are not real runtime calls:
    // belongs_to, has_many, has_one, has_and_belongs_to_many, validates,
    // validate, scope, before_action, after_action, before_validation,
    // after_commit, attr_accessor, etc.
    // We keep them in the tree but don't emit Calls edges for them - they
    // would just create noise from every model/controller file.
    if is_rails_dsl_call(&callee_name) {
        return;
    }

    let caller_id = find_enclosing_function(node, ctx);
    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);

    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&callee_name) {
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(&callee_name) {
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.8,
        )
    } else if callee_kind == CalleeKind::SelfChain {
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.6,
        )
    } else {
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.5,
        )
    };

    ctx.result.relationships.push(Relationship {
        source_id,
        target_id,
        rel_type: RelationType::Calls,
        confidence,
    });
}

/// Rails DSL method names that appear as method calls in class bodies but
/// are declarative, not procedural calls. Emitting Calls edges for these
/// would produce low-value noise in the graph.
fn is_rails_dsl_call(name: &str) -> bool {
    matches!(
        name,
        "belongs_to"
            | "has_many"
            | "has_one"
            | "has_and_belongs_to_many"
            | "validates"
            | "validate"
            | "validates_presence_of"
            | "validates_uniqueness_of"
            | "validates_format_of"
            | "validates_length_of"
            | "validates_numericality_of"
            | "validates_inclusion_of"
            | "scope"
            | "before_action"
            | "after_action"
            | "around_action"
            | "skip_before_action"
            | "before_validation"
            | "after_validation"
            | "before_save"
            | "after_save"
            | "before_create"
            | "after_create"
            | "before_update"
            | "after_update"
            | "before_destroy"
            | "after_destroy"
            | "after_commit"
            | "after_rollback"
            | "attr_accessor"
            | "attr_reader"
            | "attr_writer"
            | "include"
            | "prepend"
            | "extend"
            | "allow_browser"
            | "helper_method"
            | "protect_from_forgery"
            | "rescue_from"
            | "cattr_accessor"
            | "mattr_accessor"
            | "delegate"
    )
}

/// Extract the callee name from a `call` node and classify the call pattern.
///
/// tree-sitter-ruby call node fields:
///   - `receiver` - the object being called on (optional)
///   - `method`   - the method name identifier
///
/// Examples:
///   `foo()`         -> receiver=None, method="foo"   -> ("foo", Bare)
///   `self.bar()`    -> receiver=self, method="bar"   -> ("bar", SelfChain)
///   `obj.method()`  -> receiver=obj,  method="method" -> ("method", Receiver)
fn extract_callee_name(node: &Node<'_>, source: &[u8]) -> (String, CalleeKind) {
    let method_name = match node.child_by_field_name("method") {
        Some(n) => node_text(&n, source),
        None => return (String::new(), CalleeKind::Bare),
    };

    if method_name.is_empty() {
        return (String::new(), CalleeKind::Bare);
    }

    match node.child_by_field_name("receiver") {
        None => (method_name, CalleeKind::Bare),
        Some(recv) => {
            let kind = if node_text(&recv, source) == "self" {
                CalleeKind::SelfChain
            } else {
                CalleeKind::Receiver
            };
            (method_name, kind)
        }
    }
}

/// Find the UUID of the innermost method/function that contains `node`.
/// Returns `None` if the call is at class or file scope.
fn find_enclosing_function(
    call_node: &Node<'_>,
    ctx: &ParseContext<'_>,
) -> Option<Uuid> {
    let call_start = call_node.start_position().row as i32 + 1;

    let mut best: Option<(Uuid, i32)> = None; // (id, range_size)

    for sym in &ctx.result.symbols {
        if !matches!(sym.symbol_type, SymbolType::Function | SymbolType::Method) {
            continue;
        }
        if sym.file_path != ctx.file_path {
            continue;
        }
        let (start, end) = match (sym.start_line, sym.end_line) {
            (Some(s), Some(e)) => (s, e),
            _ => continue,
        };
        if call_start >= start && call_start <= end {
            let range = end - start;
            let current_best = best.map(|(_, r)| r).unwrap_or(i32::MAX);
            if range < current_best {
                best = Some((sym.id, range));
            }
        }
    }

    best.map(|(id, _)| id)
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

fn node_text(node: &Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_inherits_interrupt() {
        let source = r#"
module Sidekiq
  class Shutdown < Interrupt; end
end
"#;
        let result =
            parse_ruby_file("lib/sidekiq.rb", source, "test", chrono::Utc::now());

        let shutdown = result
            .symbols
            .iter()
            .find(|s| s.name == "Sidekiq::Shutdown")
            .expect("Sidekiq::Shutdown not found");

        let inherits: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| {
                r.source_id == shutdown.id
                    && r.rel_type == RelationType::Inherits
            })
            .collect();

        assert!(!inherits.is_empty(), "Expected Inherits relationship from Shutdown");

        // Verify the relationship target is the deterministic UUID for "Interrupt".
        // tree-sitter-ruby includes `<` in the superclass node; the parser strips it,
        // so base_name is "Interrupt" and the UUID is new_v5(NAMESPACE_OID, "Interrupt").
        let expected_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Interrupt");
        assert_eq!(
            inherits[0].target_id, expected_id,
            "Inherits target UUID should be new_v5(NAMESPACE_OID, 'Interrupt')"
        );
    }

    #[test]
    fn test_camel_to_snake() {
        assert_eq!(camel_to_snake("ApplicationController"), "application_controller");
        assert_eq!(camel_to_snake("Api"), "api");
        assert_eq!(camel_to_snake("V1"), "v1");
        assert_eq!(camel_to_snake("User"), "user");
        assert_eq!(camel_to_snake("VoiceScreen"), "voice_screen");
    }

    #[test]
    fn test_zeitwerk_path() {
        assert_eq!(zeitwerk_path("ApplicationController"), "application_controller");
        assert_eq!(zeitwerk_path("Api::V1::CandidatesController"), "api/v1/candidates_controller");
        assert_eq!(zeitwerk_path("Authenticatable"), "authenticatable");
    }

    #[test]
    fn test_superclass_generates_zeitwerk_import() {
        let source = r#"
class CandidatesController < ApplicationController
  def index
  end
end
"#;
        let result =
            parse_ruby_file("app/controllers/candidates_controller.rb", source, "test", chrono::Utc::now());

        // Should have a RawImport for the Zeitwerk path of ApplicationController.
        let import = result
            .raw_imports
            .iter()
            .find(|i| i.module_path == "application_controller");
        assert!(import.is_some(), "Expected Zeitwerk import for ApplicationController");

        // Should have an Imports relationship.
        let imports: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::Imports)
            .collect();
        assert!(!imports.is_empty(), "Expected at least one Imports relationship from Zeitwerk inference");
    }

    #[test]
    fn test_include_generates_zeitwerk_import() {
        let source = r#"
class User < ApplicationRecord
  include Authenticatable
  include Api::Trackable
end
"#;
        let result =
            parse_ruby_file("app/models/user.rb", source, "test", chrono::Utc::now());

        let auth_import = result
            .raw_imports
            .iter()
            .find(|i| i.module_path == "authenticatable");
        assert!(auth_import.is_some(), "Expected Zeitwerk import for Authenticatable");

        let trackable_import = result
            .raw_imports
            .iter()
            .find(|i| i.module_path == "api/trackable");
        assert!(trackable_import.is_some(), "Expected Zeitwerk import for Api::Trackable");
    }

    #[test]
    fn test_attr_accessor_emits_methods() {
        let source = r#"
module Sidekiq
  class Launcher
    attr_accessor :managers, :poller
  end
end
"#;
        let result =
            parse_ruby_file("lib/sidekiq/launcher.rb", source, "test", chrono::Utc::now());

        let managers = result
            .symbols
            .iter()
            .find(|s| s.name == "managers" && s.symbol_type == SymbolType::Method);
        assert!(managers.is_some(), "Expected 'managers' method symbol from attr_accessor");

        let poller = result
            .symbols
            .iter()
            .find(|s| s.name == "poller" && s.symbol_type == SymbolType::Method);
        assert!(poller.is_some(), "Expected 'poller' method symbol from attr_accessor");
    }
}
