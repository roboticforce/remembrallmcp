//! Tree-sitter based Python parser.
//!
//! Extracts symbols and relationships from a single Python source file.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};

/// An unresolved import captured during parse.
///
/// The walker resolves these after all files are indexed, using the full
/// set of known file paths to match dot-paths to actual files.
#[derive(Debug, Clone)]
pub struct RawImport {
    /// The source file symbol UUID (the file that contains this import).
    pub source_id: Uuid,
    /// The raw module string exactly as it appeared in the source.
    ///
    /// Examples:
    ///   `from ..storage.work_queue import WorkQueue`  -> `..storage.work_queue`
    ///   `from .types import TaskType`                 -> `.types`
    ///   `from sugar.memory.store import MemoryStore`  -> `sugar.memory.store`
    ///   `import os`                                   -> `os`
    pub module_raw: String,
    /// True when the module path starts with one or more dots (relative import).
    pub is_relative: bool,
    /// Number of leading dots (0 for absolute, 1 for same-package, 2+ for parent packages).
    pub dot_count: usize,
    /// The path component after the leading dots.
    ///
    /// For `..storage.work_queue` this is `storage.work_queue`.
    /// For `.types` this is `types`.
    /// For `sugar.memory.store` this is `sugar.memory.store` (dot_count = 0).
    pub module_path: String,
}

/// All symbols and relationships extracted from a single file.
#[derive(Debug, Default)]
pub struct FileParseResult {
    pub symbols: Vec<Symbol>,
    pub relationships: Vec<Relationship>,
    /// Unresolved imports - the walker resolves these after indexing all files.
    pub raw_imports: Vec<RawImport>,
}

/// Parse a Python file and extract symbols and relationships.
///
/// - `file_path` - canonical path string stored on each symbol
/// - `source`    - raw UTF-8 source text
/// - `project`   - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_python_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("failed to load Python grammar");

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
        // Map from symbol name -> UUID for same-file call resolution.
        name_to_id: HashMap::new(),
        // Modules imported into this file: module_name -> alias or original.
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
        language: "python".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
    });

    // First pass: collect imports so we can score call confidence later.
    let mut cursor = root.walk();
    collect_imports(&root, source_bytes, &mut ctx, &mut cursor);

    // Second pass: collect top-level and nested class/function definitions.
    let mut cursor2 = root.walk();
    collect_definitions(
        &root,
        file_symbol_id,
        None, // no enclosing class at top level
        source_bytes,
        &mut ctx,
        &mut cursor2,
    );

    // Third pass: collect call expressions inside function/method bodies.
    let mut cursor3 = root.walk();
    collect_calls(&root, source_bytes, &mut ctx, &mut cursor3);

    ctx.result
}

/// Resolve a Python import to an absolute filesystem path, given the importing
/// file's absolute path and the number of leading dots plus the dotted module path.
///
/// Returns the resolved absolute path WITHOUT extension - callers try both
/// `<path>.py` and `<path>/__init__.py`.
///
/// Returns `None` if the import cannot be resolved (e.g., stdlib or external package).
pub fn resolve_python_import(
    importing_file: &str,
    dot_count: usize,
    module_path: &str,
) -> Option<String> {
    let file = Path::new(importing_file);
    let file_dir = file.parent()?;

    // For relative imports: go up (dot_count - 1) package levels from the file's directory.
    // 1 dot = same package (file_dir itself)
    // 2 dots = parent package (go up one from file_dir)
    // 3 dots = grandparent, etc.
    let base_dir = if dot_count == 0 {
        // Absolute import - we cannot resolve without knowing sys.path.
        // Return None and let the walker try path suffix matching instead.
        return None;
    } else {
        let levels_up = dot_count - 1;
        let mut dir = file_dir.to_path_buf();
        for _ in 0..levels_up {
            dir = dir.parent()?.to_path_buf();
        }
        dir
    };

    // Convert the dotted module path to a filesystem path segment.
    // "storage.work_queue" -> "storage/work_queue"
    // "" (bare relative import `from . import foo`) -> ""
    let path_suffix = module_path.replace('.', "/");

    let resolved = if path_suffix.is_empty() {
        base_dir
    } else {
        base_dir.join(&path_suffix)
    };

    Some(resolved.to_string_lossy().to_string())
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
    /// Names that were imported (modules, names from modules).
    imported_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Import collection
// ---------------------------------------------------------------------------

fn collect_imports<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    // Walk all children at this level; imports only appear at module scope.
    for child in node.children(cursor) {
        match child.kind() {
            // `import os`, `import os.path`, `import os as operating_system`
            "import_statement" => {
                process_import_statement(&child, source, ctx);
            }
            // `from os import path`, `from os.path import join, exists`
            "import_from_statement" => {
                process_import_from_statement(&child, source, ctx);
            }
            _ => {}
        }
    }
}

fn process_import_statement(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // Children: "import", then one or more aliased_import or dotted_name nodes.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let module_name = match child.kind() {
            "dotted_name" | "relative_import" => node_text(&child, source),
            "aliased_import" => {
                // `import X as Y` - record both the original and alias
                let alias = child
                    .child_by_field_name("alias")
                    .and_then(|n| Some(node_text(&n, source)))
                    .unwrap_or_default();
                let original = child
                    .child_by_field_name("name")
                    .and_then(|n| Some(node_text(&n, source)))
                    .unwrap_or_default();
                if !alias.is_empty() {
                    ctx.imported_names.insert(alias.clone());
                }
                original
            }
            _ => continue,
        };
        if !module_name.is_empty() {
            // Record top-level module name (before the first dot).
            let top = module_name.split('.').next().unwrap_or(&module_name);
            ctx.imported_names.insert(top.to_string());

            // Parse leading dots for relative imports.
            let (dot_count, path_part) = parse_dot_prefix(&module_name);

            let file_id = ctx.result.symbols[0].id;

            // Record as a raw import for later resolution by the walker.
            ctx.result.raw_imports.push(RawImport {
                source_id: file_id,
                module_raw: module_name.clone(),
                is_relative: dot_count > 0,
                dot_count,
                module_path: path_part.to_string(),
            });

            // Emit a placeholder relationship. The walker will rewrite target_id
            // for imports it can resolve to a real file symbol. Unresolvable
            // imports (stdlib, third-party) keep this synthetic UUID.
            let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, module_name.as_bytes());
            ctx.result.relationships.push(Relationship {
                source_id: file_id,
                target_id,
                rel_type: RelationType::Imports,
                confidence: 0.3, // low until resolved
            });
        }
    }
}

fn process_import_from_statement(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // `from <module> import <name>, ...`
    // tree-sitter-python field names: module_name, name

    // The module_name field may be a dotted_name or a relative_import node.
    let module_node = node.child_by_field_name("module_name");

    // Build the raw module string. For relative imports tree-sitter gives us the
    // full text including leading dots as part of the relative_import node, or
    // the dots appear as unnamed children before the dotted_name.
    let raw_module = if let Some(n) = &module_node {
        node_text(n, source)
    } else {
        // `from . import foo` - no module_name child, just dots
        // Count leading dots from the node text of the full statement.
        let stmt_text = node_text(node, source);
        // Extract what's between "from" and "import"
        extract_from_module(&stmt_text)
    };

    // Count leading dots and strip them to get the path portion.
    let (dot_count, module_path) = parse_dot_prefix(&raw_module);

    let file_id = ctx.result.symbols[0].id;

    if !raw_module.is_empty() || dot_count > 0 {
        // Record raw import for the walker to resolve.
        ctx.result.raw_imports.push(RawImport {
            source_id: file_id,
            module_raw: raw_module.clone(),
            is_relative: dot_count > 0,
            dot_count,
            module_path: module_path.to_string(),
        });

        // Emit placeholder relationship (walker rewrites resolved ones).
        let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, raw_module.as_bytes());
        ctx.result.relationships.push(Relationship {
            source_id: file_id,
            target_id,
            rel_type: RelationType::Imports,
            confidence: 0.3,
        });
    }

    // Collect all imported names so we can score calls as "imported" (0.8).
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "dotted_name"
                if child.id()
                    != node
                        .child_by_field_name("module_name")
                        .map(|n| n.id())
                        .unwrap_or(0) =>
            {
                ctx.imported_names.insert(node_text(&child, source));
            }
            "aliased_import" => {
                if let Some(alias) = child.child_by_field_name("alias") {
                    ctx.imported_names.insert(node_text(&alias, source));
                }
                if let Some(name) = child.child_by_field_name("name") {
                    ctx.imported_names.insert(node_text(&name, source));
                }
            }
            "wildcard_import" => {}
            _ => {}
        }
    }
}

/// Parse the leading dots from a module string.
///
/// Returns `(dot_count, remainder)` where:
/// - `dot_count` is 0 for absolute imports, 1+ for relative
/// - `remainder` is the module path without the leading dots
///
/// Examples:
///   `..storage.work_queue` -> (2, "storage.work_queue")
///   `.types`               -> (1, "types")
///   `sugar.memory.store`   -> (0, "sugar.memory.store")
///   `..`                   -> (2, "")
fn parse_dot_prefix(s: &str) -> (usize, &str) {
    let dots = s.chars().take_while(|&c| c == '.').count();
    (dots, &s[dots..])
}

/// Extract the module portion from a `from X import Y` statement string.
/// Used as a fallback when tree-sitter doesn't give us a module_name field.
fn extract_from_module(stmt: &str) -> String {
    // stmt looks like "from . import foo" or "from .. import bar"
    let after_from = stmt.trim_start_matches("from").trim_start();
    let before_import = after_from.split("import").next().unwrap_or("").trim();
    before_import.to_string()
}

// ---------------------------------------------------------------------------
// Definition collection
// ---------------------------------------------------------------------------

/// Recursively walk the AST collecting function_definition and class_definition nodes.
///
/// - `parent_id`       - the symbol ID of the enclosing scope (file or class)
/// - `enclosing_class` - Some(class_symbol_id) when inside a class body
fn collect_definitions<'a>(
    node: &Node<'a>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "function_definition" => {
                let sym_id = process_function(&child, parent_id, enclosing_class, source, ctx);
                // Recurse into function body to catch nested classes/functions.
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(&body, sym_id, None, source, ctx, &mut inner);
                }
            }
            "class_definition" => {
                let class_id = process_class(&child, parent_id, source, ctx);
                // Recurse into class body; methods are defined here.
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(&body, class_id, Some(class_id), source, ctx, &mut inner);
                }
            }
            "decorated_definition" => {
                // @decorator\ndef foo(): ... or @decorator\nclass Foo: ...
                // The actual definition is the last named child.
                let mut dc = child.walk();
                for inner_child in child.named_children(&mut dc) {
                    match inner_child.kind() {
                        "function_definition" => {
                            let sym_id = process_function(
                                &inner_child,
                                parent_id,
                                enclosing_class,
                                source,
                                ctx,
                            );
                            if let Some(body) = inner_child.child_by_field_name("body") {
                                let mut bc = body.walk();
                                collect_definitions(
                                    &body, sym_id, None, source, ctx, &mut bc,
                                );
                            }
                        }
                        "class_definition" => {
                            let class_id =
                                process_class(&inner_child, parent_id, source, ctx);
                            if let Some(body) = inner_child.child_by_field_name("body") {
                                let mut bc = body.walk();
                                collect_definitions(
                                    &body,
                                    class_id,
                                    Some(class_id),
                                    source,
                                    ctx,
                                    &mut bc,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                // Keep descending into blocks, if/for/with/try etc.
                let mut inner = child.walk();
                collect_definitions(&child, parent_id, enclosing_class, source, ctx, &mut inner);
            }
        }
    }
}

fn process_function(
    node: &Node<'_>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
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

    let signature = build_function_signature(node, &name, source);
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
        language: "python".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
    });

    // DEFINES: parent (file or class) defines this function/method.
    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    id
}

fn process_class(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Class,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "python".to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("class {name}")),
        file_mtime: ctx.file_mtime,
    });

    // DEFINES: file (or outer class) defines this class.
    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // INHERITS: class Foo(Base1, Base2)
    if let Some(superclasses) = node.child_by_field_name("superclasses") {
        let mut cursor = superclasses.walk();
        for arg in superclasses.named_children(&mut cursor) {
            let base_name = node_text(&arg, source);
            if base_name.is_empty() || base_name == "object" {
                continue;
            }
            // If the base class is defined in this file we can resolve the UUID.
            let target_id = ctx
                .name_to_id
                .get(&base_name)
                .copied()
                .unwrap_or_else(|| Uuid::new_v5(&Uuid::NAMESPACE_OID, base_name.as_bytes()));

            let confidence = if ctx.name_to_id.contains_key(&base_name) {
                1.0
            } else if ctx.imported_names.contains(&base_name) {
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
        }
    }

    id
}

/// Build a human-readable signature string: `def foo(a, b, *, c=1) -> int`.
fn build_function_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    let return_type = node
        .child_by_field_name("return_type")
        .map(|n| format!(" -> {}", node_text(&n, source)));

    format!(
        "def {name}{params}{}",
        return_type.as_deref().unwrap_or("")
    )
}

// ---------------------------------------------------------------------------
// Call collection
// ---------------------------------------------------------------------------

/// Walk the entire tree looking for call expressions.
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

/// Describes how a callee was referenced, used for confidence scoring.
#[derive(Debug, PartialEq)]
enum CalleeKind {
    /// Plain bare call: `foo()`
    Bare,
    /// `self.method()` or `self.obj.method()` - instance method call via self
    SelfChain,
    /// Any other dotted call: `obj.method()`, `module.func()`, `a.b.c()`
    Attribute,
}

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // The function field holds the callee expression.
    let Some(function_node) = node.child_by_field_name("function") else {
        return;
    };

    // Resolve the bare function name being called.
    // Handles: `foo()`, `self.obj.method()`, `obj.method()`, `module.func()`.
    let (callee_name, callee_kind) = extract_callee_name(&function_node, source);
    if callee_name.is_empty() {
        return;
    }

    // Determine which symbol (function/method) we are inside.
    // We do this by finding the innermost enclosing function that contains this node.
    let caller_id = find_enclosing_function(node, source, ctx);

    // Score confidence based on what we know about the callee.
    //
    // Confidence rules:
    //   1.0 - method name matches a known symbol defined in this file
    //   0.8 - method name matches an imported name
    //   0.6 - self.xxx.method() pattern: we know it's a method call but can't resolve the type
    //   0.5 - unresolved attribute call (obj.method() where obj is not self)
    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&callee_name) {
        // Defined in this file - high confidence.
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(&callee_name) {
        // Imported name - medium confidence.
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.8,
        )
    } else if callee_kind == CalleeKind::SelfChain {
        // self.xxx.method() - instance method call, type not resolvable statically.
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.6,
        )
    } else {
        // Unknown - low confidence.
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.5,
        )
    };

    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);

    ctx.result.relationships.push(Relationship {
        source_id,
        target_id,
        rel_type: RelationType::Calls,
        confidence,
    });
}

/// Extract the leaf function name from a callee expression and classify the call kind.
///
/// - `foo` -> ("foo", Bare)
/// - `self.bar` -> ("bar", SelfChain)
/// - `self.queue.get()` -> ("get", SelfChain)
/// - `module.func` -> ("func", Attribute)
/// - `obj.method` -> ("method", Attribute)
fn extract_callee_name(node: &Node<'_>, source: &[u8]) -> (String, CalleeKind) {
    match node.kind() {
        "identifier" => (node_text(node, source), CalleeKind::Bare),
        "attribute" => {
            // `obj.attr` - take only the attribute part (the method name).
            let method = node
                .child_by_field_name("attribute")
                .map(|n| node_text(&n, source))
                .unwrap_or_default();
            let kind = if attribute_chain_starts_with_self(node, source) {
                CalleeKind::SelfChain
            } else {
                CalleeKind::Attribute
            };
            (method, kind)
        }
        _ => (String::new(), CalleeKind::Bare),
    }
}

/// Walk up an attribute chain to determine if it starts with `self`.
///
/// For `self.work_queue.get_next_work`, the tree looks like:
///   attribute(object=attribute(object=identifier("self"), attr="work_queue"), attr="get_next_work")
fn attribute_chain_starts_with_self(node: &Node<'_>, source: &[u8]) -> bool {
    let mut current = node.clone();
    loop {
        match current.kind() {
            "attribute" => {
                if let Some(obj) = current.child_by_field_name("object") {
                    current = obj;
                } else {
                    return false;
                }
            }
            "identifier" => {
                return node_text(&current, source) == "self";
            }
            _ => return false,
        }
    }
}

/// Find the UUID of the innermost function/method symbol that contains `node`.
/// Returns None if the call is at module scope.
fn find_enclosing_function(
    call_node: &Node<'_>,
    _source: &[u8],
    ctx: &ParseContext<'_>,
) -> Option<Uuid> {
    let call_start = call_node.start_position().row as i32 + 1;

    // Walk our collected symbols to find the innermost (smallest range) function
    // or method that contains the call's line number.
    let mut best: Option<(Uuid, i32, i32)> = None; // (id, start, end)

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
            // Prefer the tightest (innermost) enclosing scope.
            let range = end - start;
            let current_best_range = best.map(|(_, s, e)| e - s).unwrap_or(i32::MAX);
            if range < current_best_range {
                best = Some((sym.id, start, end));
            }
        }
    }

    best.map(|(id, _, _)| id)
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
