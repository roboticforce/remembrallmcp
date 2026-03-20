//! Tree-sitter based Rust parser.
//!
//! Extracts symbols and relationships from a single Rust source file.
//!
//! ## What is extracted
//!
//! Symbols:
//! - `fn` items (top-level) -> SymbolType::Function
//! - `struct` items         -> SymbolType::Class
//! - `enum` items           -> SymbolType::Class
//! - `trait` items          -> SymbolType::Class
//! - methods inside `impl`  -> SymbolType::Method
//! - the file itself        -> SymbolType::File
//!
//! Relationships:
//! - `use` declarations          -> RelationType::Imports
//! - `call_expression`           -> RelationType::Calls
//! - `impl Trait for Type`       -> RelationType::Inherits (type implements trait)
//! - enclosing scope -> symbol   -> RelationType::Defines

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Parse a Rust file and extract symbols and relationships.
///
/// - `file_path`  - canonical path string stored on each symbol
/// - `source`     - raw UTF-8 source text
/// - `project`    - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_rust_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("failed to load Rust grammar");

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

    // File-level symbol (always index 0 - other code depends on this).
    let file_symbol_id = Uuid::new_v4();
    ctx.result.symbols.push(Symbol {
        id: file_symbol_id,
        name: file_path.to_string(),
        symbol_type: SymbolType::File,
        file_path: file_path.to_string(),
        start_line: Some(1),
        end_line: Some(source.lines().count() as i32),
        language: "rust".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
    });

    // Pass 1: collect `use` declarations (imports).
    let mut cursor = root.walk();
    collect_imports(&root, source_bytes, &mut ctx, &mut cursor);

    // Pass 2: collect type/function definitions and impl blocks.
    let mut cursor2 = root.walk();
    collect_definitions(&root, file_symbol_id, None, source_bytes, &mut ctx, &mut cursor2);

    // Pass 3: collect call expressions.
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
    /// Names brought into scope via `use` statements.
    imported_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Import collection
// ---------------------------------------------------------------------------

/// Recursively walk the AST looking for `use_declaration` nodes at any depth.
///
/// Rust allows `use` declarations inside function bodies (item-scoped imports),
/// so we must scan beyond the top level to capture all imports.  The imported
/// names are recorded on the file symbol regardless of nesting depth.
fn collect_imports<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        if child.kind() == "use_declaration" {
            process_use_declaration(&child, source, ctx);
        }
        // Recurse into blocks (function bodies, impl blocks, mod items) so that
        // inner `use` declarations are also captured.
        let mut inner = child.walk();
        collect_imports(&child, source, ctx, &mut inner);
    }
}

/// Emit an Imports relationship and a RawImport for a single `use` declaration.
///
/// Rust `use` paths use `::` as separators and may start with:
/// - `crate::` - absolute from the current crate root
/// - `super::` - parent module
/// - `self::` - current module
/// - an external crate name
///
/// We convert `::` to `/` for the module_path so the walker can do suffix
/// matching the same way it does for Python absolute imports.
fn process_use_declaration(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // The `use_declaration` node structure:
    //   use_declaration -> "use" argument=use_tree ";"
    //
    // use_tree can be:
    //   - scoped_identifier (e.g., `crate::foo::Bar`)
    //   - identifier (e.g., `std`)
    //   - use_wildcard (e.g., `crate::foo::*`)
    //   - use_list (e.g., `{ Foo, Bar }` - nested under a parent path)
    //
    // We extract a flat string from the argument and normalise it.

    let Some(arg) = node.child_by_field_name("argument") else {
        return;
    };

    // Collect all leaf paths from (potentially nested) use trees.
    let mut paths: Vec<String> = Vec::new();
    collect_use_tree_paths(&arg, source, &mut paths);

    let file_id = ctx.result.symbols[0].id;

    for raw_path in paths {
        if raw_path.is_empty() {
            continue;
        }

        // Record the last segment as an imported name for call scoring.
        if let Some(last) = raw_path.split("::").last() {
            let name = last.trim_end_matches('*');
            if !name.is_empty() && name != "{" {
                ctx.imported_names.insert(name.to_string());
            }
        }

        // Classify relative vs absolute.
        let is_relative = raw_path.starts_with("crate::")
            || raw_path.starts_with("super::")
            || raw_path.starts_with("self::");

        // Convert `crate::foo::bar` -> `foo/bar` for suffix matching.
        // `super::foo` -> `foo`
        // `external_crate::foo` -> `external_crate/foo`
        let module_path = use_path_to_module_path(&raw_path);

        ctx.result.raw_imports.push(RawImport {
            source_id: file_id,
            module_raw: raw_path.clone(),
            is_relative,
            dot_count: 0, // Rust uses :: not dots
            module_path: module_path.clone(),
        });

        let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, raw_path.as_bytes());
        ctx.result.relationships.push(Relationship {
            source_id: file_id,
            target_id,
            rel_type: RelationType::Imports,
            confidence: if is_relative { 0.3 } else { 0.5 },
        });
    }
}

/// Recursively collect all terminal paths from a use_tree node.
///
/// Handles:
/// - Simple identifier: `use foo` -> ["foo"]
/// - Scoped: `use foo::bar` -> ["foo::bar"]
/// - Glob: `use foo::*` -> ["foo::*"]
/// - List: `use foo::{Bar, Baz}` -> ["foo::Bar", "foo::Baz"]
fn collect_use_tree_paths(node: &Node<'_>, source: &[u8], paths: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "self" | "crate" | "super" => {
            paths.push(node_text(node, source));
        }
        "scoped_identifier" => {
            // path::name
            paths.push(node_text(node, source));
        }
        "scoped_use_list" => {
            // `foo::{Bar, Baz}` - tree-sitter-rust: path + list
            let prefix = node
                .child_by_field_name("path")
                .map(|n| node_text(&n, source))
                .unwrap_or_default();
            if let Some(list) = node.child_by_field_name("list") {
                let mut sub_paths: Vec<String> = Vec::new();
                let mut c = list.walk();
                for child in list.named_children(&mut c) {
                    collect_use_tree_paths(&child, source, &mut sub_paths);
                }
                for sub in sub_paths {
                    if prefix.is_empty() {
                        paths.push(sub);
                    } else {
                        paths.push(format!("{prefix}::{sub}"));
                    }
                }
            }
        }
        "use_list" => {
            // `{ Foo, Bar }` as a standalone use_list (top of the tree).
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                collect_use_tree_paths(&child, source, paths);
            }
        }
        "use_wildcard" => {
            // `foo::*` - the wildcard node contains path + "*"
            paths.push(node_text(node, source));
        }
        "use_as_clause" => {
            // `foo::Bar as Baz` - record original path
            let path = node
                .child_by_field_name("path")
                .map(|n| node_text(&n, source))
                .unwrap_or_else(|| node_text(node, source));
            paths.push(path);
        }
        _ => {
            // Fallback: just grab the text.
            let text = node_text(node, source);
            if !text.is_empty() {
                paths.push(text);
            }
        }
    }
}

/// Convert a Rust use path to a filesystem-style module path for suffix matching.
///
/// - `crate::foo::bar`  -> `foo/bar`   (crate root relative)
/// - `super::foo`       -> `foo`        (parent module)
/// - `self::foo`        -> `foo`
/// - `std::collections` -> `std/collections` (external)
/// - `some::path::*`    -> `some/path`  (strip wildcard)
fn use_path_to_module_path(use_path: &str) -> String {
    let stripped = use_path
        .trim_end_matches("::*")
        .trim_end_matches("::{}")
        .trim_end_matches("::self");

    let without_prefix = if let Some(rest) = stripped.strip_prefix("crate::") {
        rest
    } else if let Some(rest) = stripped.strip_prefix("super::") {
        rest
    } else if let Some(rest) = stripped.strip_prefix("self::") {
        rest
    } else {
        stripped
    };

    without_prefix.replace("::", "/")
}

// ---------------------------------------------------------------------------
// Definition collection
// ---------------------------------------------------------------------------

/// Recursively walk the AST collecting definitions.
///
/// Top-level items: fn_item, struct_item, enum_item, trait_item, impl_item.
/// Inside impl blocks: fn_item -> Method.
fn collect_definitions<'a>(
    node: &Node<'a>,
    parent_id: Uuid,
    enclosing_impl: Option<ImplContext>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "function_item" => {
                let sym_id =
                    process_function(&child, parent_id, enclosing_impl.as_ref(), source, ctx);
                // Recurse into function body.
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(&body, sym_id, None, source, ctx, &mut inner);
                }
            }
            "struct_item" => {
                process_adt(&child, parent_id, source, ctx, "struct");
            }
            "enum_item" => {
                process_adt(&child, parent_id, source, ctx, "enum");
            }
            "trait_item" => {
                let trait_id = process_adt(&child, parent_id, source, ctx, "trait");
                // Also extract method signatures defined in the trait body.
                process_trait_body(&child, trait_id, source, ctx);
            }
            "impl_item" => {
                process_impl(&child, parent_id, source, ctx);
            }
            _ => {
                // Descend into mod items, if/match blocks, etc.
                let mut inner = child.walk();
                collect_definitions(
                    &child,
                    parent_id,
                    enclosing_impl.clone(),
                    source,
                    ctx,
                    &mut inner,
                );
            }
        }
    }
}

/// Context carried when we are inside an `impl` block.
///
/// Signals to `process_function` that the function being defined is a method
/// and provides the type name so the method can be stored as `TypeName::method`.
#[derive(Debug, Clone)]
struct ImplContext {
    /// The base type name (e.g. "Controller" for `impl Controller<'_>`).
    type_name: String,
}

/// Process `struct Foo`, `enum Bar`, `trait Baz` items -> SymbolType::Class.
fn process_adt(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    keyword: &str,
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
        language: "rust".to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("{keyword} {name}")),
        file_mtime: ctx.file_mtime,
    });

    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    id
}

/// Extract method signatures from a `trait_item` body.
///
/// Trait method definitions (with or without a default body) are collected as
/// Method symbols so that cross-file call resolution can find them via
/// `synthetic_to_real` in the walker.
fn process_trait_body(node: &Node<'_>, trait_id: Uuid, source: &[u8], ctx: &mut ParseContext<'_>) {
    // Retrieve the trait name (already registered in name_to_id by process_adt).
    let trait_name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_default();

    if trait_name.is_empty() {
        return;
    }

    let impl_ctx = ImplContext {
        type_name: trait_name,
    };

    if let Some(body) = node.child_by_field_name("body") {
        let mut c = body.walk();
        for child in body.named_children(&mut c) {
            // `function_signature_item` = signature without body (required methods)
            // `function_item` = function with default body
            if child.kind() == "function_item" || child.kind() == "function_signature_item" {
                let method_id =
                    process_function(&child, trait_id, Some(&impl_ctx), source, ctx);

                // DEFINES: the trait defines this method.
                ctx.result.relationships.push(Relationship {
                    source_id: trait_id,
                    target_id: method_id,
                    rel_type: RelationType::Defines,
                    confidence: 1.0,
                });
            }
        }
    }
}

/// Process an `impl` block.
///
/// Two forms:
/// - `impl Foo { ... }`         -> methods are defined on Foo
/// - `impl Trait for Foo { ... }` -> Foo Inherits Trait + methods on Foo
fn process_impl(node: &Node<'_>, parent_id: Uuid, source: &[u8], ctx: &mut ParseContext<'_>) {
    // In tree-sitter-rust, impl_item has:
    //   trait (optional) = the trait name
    //   type = the type being implemented
    let type_node = node.child_by_field_name("type");
    let trait_node = node.child_by_field_name("trait");

    let type_name = type_node
        .as_ref()
        .map(|n| extract_type_name(n, source))
        .unwrap_or_else(|| "<unknown>".to_string());

    // Resolve the type UUID (may already be known if defined in this file).
    let type_id = ctx
        .name_to_id
        .get(&type_name)
        .copied()
        .unwrap_or_else(|| Uuid::new_v5(&Uuid::NAMESPACE_OID, type_name.as_bytes()));

    // `impl Trait for Type` -> emit Inherits(Type, Trait).
    if let Some(trait_node) = trait_node {
        let trait_name = extract_type_name(&trait_node, source);
        if !trait_name.is_empty() {
            // If the trait is defined in this file, use its real UUID.
            // If it's a stdlib or external trait (e.g. Default, Drop, FromStr),
            // create a synthetic symbol in this file so the comparator can find
            // it via a "file::TraitName" reference.
            let trait_id = if let Some(&id) = ctx.name_to_id.get(&trait_name) {
                id
            } else {
                // Create a synthetic Class symbol representing the external trait.
                // Use a deterministic UUID so multiple impl blocks for the same
                // trait in the same file share the same node.
                let synthetic_key = format!("{}::{}", ctx.file_path, trait_name);
                let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, synthetic_key.as_bytes());
                // Only push if we haven't already emitted this synthetic trait.
                if !ctx.result.symbols.iter().any(|s| s.id == id) {
                    ctx.result.symbols.push(Symbol {
                        id,
                        name: trait_name.clone(),
                        symbol_type: SymbolType::Class,
                        file_path: ctx.file_path.to_string(),
                        start_line: None,
                        end_line: None,
                        language: "rust".to_string(),
                        project: ctx.project.to_string(),
                        signature: Some(format!("trait {trait_name}")),
                        file_mtime: ctx.file_mtime,
                    });
                    ctx.name_to_id.insert(trait_name.clone(), id);
                }
                id
            };

            let confidence = if ctx.name_to_id.contains_key(&type_name)
                && ctx.name_to_id.contains_key(&trait_name)
            {
                1.0
            } else if ctx.name_to_id.contains_key(&type_name)
                || ctx.imported_names.contains(&trait_name)
            {
                0.8
            } else {
                0.5
            };

            ctx.result.relationships.push(Relationship {
                source_id: type_id,
                target_id: trait_id,
                rel_type: RelationType::Inherits,
                confidence,
            });
        }
    }

    let impl_ctx = ImplContext {
        type_name: type_name.clone(),
    };

    // Walk the impl body collecting methods.
    if let Some(body) = node.child_by_field_name("body") {
        let mut c = body.walk();
        for child in body.named_children(&mut c) {
            if child.kind() == "function_item" {
                let method_id =
                    process_function(&child, parent_id, Some(&impl_ctx), source, ctx);

                // DEFINES: the type defines this method.
                ctx.result.relationships.push(Relationship {
                    source_id: type_id,
                    target_id: method_id,
                    rel_type: RelationType::Defines,
                    confidence: 1.0,
                });

                // Recurse into method body.
                if let Some(fn_body) = child.child_by_field_name("body") {
                    let mut inner = fn_body.walk();
                    collect_definitions(&fn_body, method_id, None, source, ctx, &mut inner);
                }
            }
        }
    }

    // Suppress the "unused parent_id" lint.
    let _ = parent_id;
}

/// Process a `fn` item inside any scope.
///
/// Returns the UUID of the newly created symbol.
fn process_function(
    node: &Node<'_>,
    parent_id: Uuid,
    impl_ctx: Option<&ImplContext>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let bare_name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    // Methods are stored as "TypeName::method_name" so that the test harness
    // and call-resolution logic can find them by their qualified name.
    let name = if let Some(ic) = impl_ctx {
        format!("{}::{}", ic.type_name, bare_name)
    } else {
        bare_name.clone()
    };

    let symbol_type = if impl_ctx.is_some() {
        SymbolType::Method
    } else {
        SymbolType::Function
    };

    let signature = build_fn_signature(node, &bare_name, source);
    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    // Register both the qualified name and the bare name for call resolution.
    // The qualified name is the canonical key; the bare name is a fallback so
    // that calls written as `new(...)` inside the same file can still resolve.
    ctx.name_to_id.insert(name.clone(), id);
    if impl_ctx.is_some() {
        ctx.name_to_id.entry(bare_name.clone()).or_insert(id);
    }
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "rust".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
    });

    // DEFINES: parent scope (file or outer function) defines this function.
    // For methods, the parent_id passed here is the file; process_impl also
    // emits a second DEFINES from the type. We emit the file-level DEFINES only
    // for top-level functions (when impl_ctx is None) to avoid duplicating.
    if impl_ctx.is_none() {
        ctx.result.relationships.push(Relationship {
            source_id: parent_id,
            target_id: id,
            rel_type: RelationType::Defines,
            confidence: 1.0,
        });
    }

    id
}

// ---------------------------------------------------------------------------
// Call collection
// ---------------------------------------------------------------------------

fn collect_calls<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    // Build a local variable-to-type map to enable field_expression resolution.
    // Example: `let controller = Controller::new(...)` -> controller: Controller
    // Example: `let mut printer: Box<dyn Printer> = ...` -> printer: Printer
    let mut local_var_types: HashMap<String, String> = HashMap::new();
    collect_local_var_types(node, source, &mut local_var_types);

    collect_calls_inner(node, source, ctx, cursor, &local_var_types);
}

fn collect_calls_inner<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
    local_var_types: &HashMap<String, String>,
) {
    for child in node.children(cursor) {
        if child.kind() == "call_expression" {
            process_call(&child, source, ctx, local_var_types);
        }
        let mut inner = child.walk();
        collect_calls_inner(&child, source, ctx, &mut inner, local_var_types);
    }
}

/// Scan `let_declaration` and `parameter` nodes to build a variable-to-type map.
///
/// Handles:
/// 1. `let x = TypeName::new(...)` -> x: TypeName (infer from constructor call)
/// 2. `let x: Box<dyn TypeName> = ...` or `let x: TypeName = ...` -> x: TypeName (from annotation)
/// 3. Function parameters `fn f(x: &mut TypeName, ...)` -> x: TypeName
fn collect_local_var_types<'a>(
    node: &Node<'a>,
    source: &[u8],
    var_types: &mut HashMap<String, String>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "let_declaration" => {
                if let Some(type_name) = extract_let_type(&child, source) {
                    if let Some(pat) = child.child_by_field_name("pattern") {
                        let var_name = extract_pattern_name(&pat, source);
                        if !var_name.is_empty() && !type_name.is_empty() {
                            var_types.insert(var_name, type_name);
                        }
                    }
                }
            }
            "parameter" => {
                // Function parameters: `name: TypeExpr`
                // The pattern is the first named child, type is via "type" field.
                if let Some(pat) = child.child_by_field_name("pattern") {
                    if let Some(type_node) = child.child_by_field_name("type") {
                        let var_name = extract_pattern_name(&pat, source);
                        let type_name = extract_leaf_type_name(&type_node, source);
                        if !var_name.is_empty() && !type_name.is_empty()
                            && var_name != "self"
                        {
                            var_types.insert(var_name, type_name);
                        }
                    }
                }
            }
            _ => {}
        }
        collect_local_var_types(&child, source, var_types);
    }
}

/// Extract the type name from a `let_declaration` node.
///
/// Tries:
/// 1. Explicit type annotation: `let x: SomeType = ...` -> "SomeType"
/// 2. Constructor inference: `let x = SomeType::new(...)` -> "SomeType"
fn extract_let_type(node: &Node<'_>, source: &[u8]) -> Option<String> {
    // Try explicit type annotation first.
    if let Some(type_node) = node.child_by_field_name("type") {
        let type_name = extract_leaf_type_name(&type_node, source);
        if !type_name.is_empty() {
            return Some(type_name);
        }
    }

    // Infer from initializer: `SomeType::new(...)` or `SomeType { ... }`
    if let Some(value_node) = node.child_by_field_name("value") {
        return infer_type_from_expr(&value_node, source);
    }

    None
}

/// Extract the leaf type name from a type annotation node.
///
/// Handles:
/// - `type_identifier` -> "Foo"
/// - `generic_type` -> strips generics
/// - `dynamic_type` (`dyn Trait`) -> extracts trait name
/// - `reference_type` -> recurse
/// - `scoped_type_identifier` -> last segment
fn extract_leaf_type_name(node: &Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "type_identifier" => node_text(node, source),
        "generic_type" => {
            // `Box<dyn Printer>` - get the outer type or recurse into type args.
            // For `Box<dyn Trait>` we want "Trait" not "Box".
            // Check if the type argument is a dyn type.
            if let Some(args) = node.child_by_field_name("type_arguments") {
                let mut c = args.walk();
                for arg in args.named_children(&mut c) {
                    let inner = extract_leaf_type_name(&arg, source);
                    if !inner.is_empty() && inner != "Box" && inner != "Vec" && inner != "Option" {
                        return inner;
                    }
                }
            }
            // Fall back to the base type name.
            node.child_by_field_name("type")
                .map(|n| extract_leaf_type_name(&n, source))
                .unwrap_or_default()
        }
        "dynamic_type" => {
            // `dyn Trait` - use the `trait` field.
            node.child_by_field_name("trait")
                .map(|n| extract_leaf_type_name(&n, source))
                .unwrap_or_default()
        }
        "reference_type" => {
            // `&mut Type` or `&Type` - use the `type` field.
            node.child_by_field_name("type")
                .map(|n| extract_leaf_type_name(&n, source))
                .unwrap_or_default()
        }
        "scoped_type_identifier" => {
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Infer the type name from an expression node (for constructor-style inference).
fn infer_type_from_expr(node: &Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "call_expression" => {
            // `SomeType::new(...)` or `SomeType::method(...)`
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "scoped_identifier" => {
                    // Take the path (left side of `::`).
                    if let Some(path) = func.child_by_field_name("path") {
                        let path_text = node_text(&path, source);
                        // Use only the last segment of a multi-segment path.
                        let last = path_text.split("::").last().unwrap_or("").to_string();
                        if !last.is_empty() {
                            return Some(last);
                        }
                    }
                    None
                }
                _ => None,
            }
        }
        // `Box::new(inner_expr)` - try to unwrap by looking at the argument.
        // If argument is also a call like `InteractivePrinter::new(...)`, use that.
        _ => None,
    }
}

/// Extract the variable name from a pattern node.
fn extract_pattern_name(node: &Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "identifier" => node_text(node, source),
        "mut_pattern" | "ref_pattern" => {
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                let name = extract_pattern_name(&child, source);
                if !name.is_empty() {
                    return name;
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

fn process_call(
    node: &Node<'_>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    local_var_types: &HashMap<String, String>,
) {
    // call_expression: function field is the callee.
    let Some(function_node) = node.child_by_field_name("function") else {
        return;
    };

    let (mut callee_name, callee_kind) = extract_callee_name(&function_node, source, local_var_types);
    if callee_name.is_empty() {
        return;
    }

    // For field_expression calls resolved to a type prefix via local var types,
    // the callee_name is already `Type::method`.  For other field expressions
    // without type info, the bare method name is used.
    // Strip the `::` qualifier from any crate:: or module:: prefixes so we
    // match the name as stored by the parser (e.g. "crate::foo::bar" -> "bar").
    if callee_kind == CalleeKind::PathOrField && callee_name.contains("::") {
        // If it's a scoped identifier with a crate/module prefix, strip it.
        // But preserve `Type::method` form for cross-file resolution.
        let segments: Vec<&str> = callee_name.split("::").collect();
        // Only strip crate/super/self prefixes; keep Type::method as-is.
        if segments[0] == "crate" || segments[0] == "super" || segments[0] == "self" {
            callee_name = segments[1..].join("::");
        }
    }

    let caller_id = find_enclosing_function(node, ctx);
    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);

    // Confidence scoring mirrors the Python/TS parsers:
    //   1.0 - callee defined in this file
    //   0.8 - callee type (first segment) was imported, or it's a self-chain
    //   0.6 - self/Self method call (type not statically resolved)
    //   0.5 - unknown
    //
    // For scoped identifiers like `Controller::new`, callee_name is the full
    // path.  We check both the full name and the type prefix against
    // imported_names so that cross-file calls resolve correctly.
    let callee_type_prefix = callee_name
        .split("::")
        .next()
        .unwrap_or("")
        .to_string();
    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&callee_name) {
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(&callee_name)
        || (!callee_type_prefix.is_empty() && ctx.imported_names.contains(&callee_type_prefix))
    {
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

#[derive(Debug, PartialEq)]
enum CalleeKind {
    /// Plain function call: `foo()`
    Bare,
    /// `self.method()` call
    SelfChain,
    /// Any other path-based call: `Foo::bar()`, `obj.method()`
    PathOrField,
}

/// Extract the leaf function name from a Rust callee expression.
///
/// Rust callee forms:
/// - `identifier` -> bare call `foo()`
/// - `field_expression` -> method call on a receiver `obj.method()`
/// - `scoped_identifier` -> `Foo::new()`, `crate::util::helper()`
///
/// For `field_expression`, if `local_var_types` contains the receiver variable's
/// type, the returned name is qualified as `Type::method`, enabling cross-file
/// call resolution.
fn extract_callee_name(
    node: &Node<'_>,
    source: &[u8],
    local_var_types: &HashMap<String, String>,
) -> (String, CalleeKind) {
    match node.kind() {
        "identifier" => (node_text(node, source), CalleeKind::Bare),
        "field_expression" => {
            // obj.field - take the field name as callee.
            let field = node
                .child_by_field_name("field")
                .map(|n| node_text(&n, source))
                .unwrap_or_default();

            if field_expr_starts_with_self(node, source) {
                return (field, CalleeKind::SelfChain);
            }

            // Try to qualify the method call using local variable type info.
            // Only look at simple receiver identifiers (not chained expressions).
            if let Some(value_node) = node.child_by_field_name("value") {
                if value_node.kind() == "identifier" {
                    let var_name = node_text(&value_node, source);
                    if let Some(type_name) = local_var_types.get(&var_name) {
                        let qualified = format!("{type_name}::{field}");
                        return (qualified, CalleeKind::PathOrField);
                    }
                }
            }

            (field, CalleeKind::PathOrField)
        }
        "scoped_identifier" => {
            // `Foo::new` - return the full qualified path so that cross-file
            // resolution can match the symbol by its qualified name (e.g.
            // `Controller::new`).  The full text is used as the callee name;
            // `process_call` handles splitting for imported-name lookup.
            let full = node_text(node, source);
            (full, CalleeKind::PathOrField)
        }
        _ => (String::new(), CalleeKind::Bare),
    }
}

/// Walk up a field_expression chain to check if it starts with `self`.
fn field_expr_starts_with_self(node: &Node<'_>, source: &[u8]) -> bool {
    let mut current = node.clone();
    loop {
        match current.kind() {
            "field_expression" => {
                if let Some(obj) = current.child_by_field_name("value") {
                    current = obj;
                } else {
                    return false;
                }
            }
            "self" => return true,
            "identifier" => {
                return node_text(&current, source) == "self";
            }
            _ => return false,
        }
    }
}

/// Find the innermost function/method symbol containing `call_node` by line number.
fn find_enclosing_function(call_node: &Node<'_>, ctx: &ParseContext<'_>) -> Option<Uuid> {
    let call_start = call_node.start_position().row as i32 + 1;
    let mut best: Option<(Uuid, i32)> = None; // (id, range)

    for sym in &ctx.result.symbols {
        if !matches!(sym.symbol_type, SymbolType::Function | SymbolType::Method) {
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
// Signature building
// ---------------------------------------------------------------------------

/// Build a human-readable signature string: `fn foo(a: A, b: B) -> C`.
///
/// Includes `async`, `pub`, and generic parameters.
fn build_fn_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    // Collect leading visibility and qualifiers.
    let mut qualifiers: Vec<&str> = Vec::new();
    let mut c = node.walk();
    for child in node.children(&mut c) {
        match child.kind() {
            "visibility_modifier" => qualifiers.push("pub"),
            "async" => qualifiers.push("async"),
            "unsafe" => qualifiers.push("unsafe"),
            "extern" => qualifiers.push("extern"),
            _ => {}
        }
    }

    let type_params = node
        .child_by_field_name("type_parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_default();

    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    let return_type = node
        .child_by_field_name("return_type")
        .map(|n| format!(" -> {}", node_text(&n, source)));

    let prefix = if qualifiers.is_empty() {
        String::new()
    } else {
        format!("{} ", qualifiers.join(" "))
    };

    format!(
        "{prefix}fn {name}{type_params}{params}{}",
        return_type.as_deref().unwrap_or("")
    )
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Extract a clean type name from a type-position node.
///
/// Handles:
/// - `type_identifier` -> "Foo"
/// - `generic_type`    -> "Foo" (strips generics like `Foo<T>`)
/// - `scoped_type_identifier` -> last segment of `crate::foo::Bar`
fn extract_type_name(node: &Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "type_identifier" => node_text(node, source),
        "generic_type" => {
            // `Foo<T>` - take just the name, not the type args.
            node.child_by_field_name("type")
                .map(|n| node_text(&n, source))
                .unwrap_or_else(|| node_text(node, source))
        }
        "scoped_type_identifier" => {
            // `crate::foo::Bar` - take the last segment.
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source))
                .unwrap_or_else(|| {
                    let full = node_text(node, source);
                    full.split("::").last().unwrap_or("").to_string()
                })
        }
        _ => {
            // Fallback: use the last `::` segment of the raw text.
            let text = node_text(node, source);
            text.split("::").last().unwrap_or("").to_string()
        }
    }
}

fn node_text(node: &Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").trim().to_string()
}
