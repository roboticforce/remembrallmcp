//! Tree-sitter based Go parser.
//!
//! Extracts symbols and relationships from a single Go source file.
//!
//! # Extracted symbols
//! - `function_declaration`  -> SymbolType::Function  (top-level `func foo()`)
//! - `method_declaration`    -> SymbolType::Method    (`func (s *Server) Handle()`)
//! - `type_declaration` with `struct_type`    -> SymbolType::Class
//! - `type_declaration` with `interface_type` -> SymbolType::Class
//! - The file itself         -> SymbolType::File
//!
//! # Extracted relationships
//! - `import_declaration` -> RelationType::Imports  (single and block imports)
//! - `call_expression`    -> RelationType::Calls
//! - Struct/interface embedding -> RelationType::Inherits
//! - Receiver type defines method -> RelationType::Defines

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Parse a Go file and extract symbols and relationships.
///
/// - `file_path`  - canonical path string stored on each symbol
/// - `source`     - raw UTF-8 source text
/// - `project`    - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_go_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("failed to load Go grammar");

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

    // File-level symbol - always the first symbol in the result.
    let file_symbol_id = Uuid::new_v4();
    ctx.result.symbols.push(Symbol {
        id: file_symbol_id,
        name: file_path.to_string(),
        symbol_type: SymbolType::File,
        file_path: file_path.to_string(),
        start_line: Some(1),
        end_line: Some(source.lines().count() as i32),
        language: "go".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
    });

    // Pass 1: collect imports.
    let mut cursor = root.walk();
    collect_imports(&root, source_bytes, &mut ctx, &mut cursor);

    // Pass 2: collect type and function/method declarations.
    let mut cursor2 = root.walk();
    collect_definitions(&root, file_symbol_id, source_bytes, &mut ctx, &mut cursor2);

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
    /// name -> symbol UUID for symbols defined in this file.
    name_to_id: HashMap<String, Uuid>,
    /// Import aliases and package names imported into this file.
    imported_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Import collection
// ---------------------------------------------------------------------------

/// Collect all `import_declaration` nodes at the source-file level.
///
/// Go supports two forms:
///   - Single: `import "fmt"`
///   - Block:  `import ( "fmt"\n alias "github.com/gorilla/mux" )`
fn collect_imports<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        if child.kind() == "import_declaration" {
            process_import_declaration(&child, source, ctx);
        }
    }
}

fn process_import_declaration(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let file_id = ctx.result.symbols[0].id;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            // Single import: `import "fmt"` or `import alias "pkg"`
            "interpreted_string_literal" | "raw_string_literal" => {
                let path = unquote(node_text(&child, source));
                emit_import(file_id, &path, None, ctx);
            }
            // Block: `import ( spec spec ... )`
            "import_spec_list" => {
                let mut list_cursor = child.walk();
                for spec in child.named_children(&mut list_cursor) {
                    if spec.kind() == "import_spec" {
                        process_import_spec(&spec, source, file_id, ctx);
                    }
                }
            }
            // Single import_spec directly under import_declaration
            "import_spec" => {
                process_import_spec(&child, source, file_id, ctx);
            }
            _ => {}
        }
    }
}

fn process_import_spec(node: &Node<'_>, source: &[u8], file_id: Uuid, ctx: &mut ParseContext<'_>) {
    // tree-sitter-go fields on import_spec: name (optional alias), path
    let path_node = node.child_by_field_name("path");
    let alias_node = node.child_by_field_name("name");

    let path = path_node
        .map(|n| unquote(node_text(&n, source)))
        .unwrap_or_default();

    if path.is_empty() {
        return;
    }

    // Alias: explicit alias, "_" (blank import), or "." (dot import)
    let alias = alias_node.map(|n| node_text(&n, source));

    emit_import(file_id, &path, alias.as_deref(), ctx);
}

/// Emit an import relationship and record the imported package name.
fn emit_import(file_id: Uuid, import_path: &str, alias: Option<&str>, ctx: &mut ParseContext<'_>) {
    if import_path.is_empty() {
        return;
    }

    // Determine the local name used in the file to reference the package.
    // Priority: explicit alias > last path segment (Go convention).
    let local_name = match alias {
        Some("_") | Some(".") | None => {
            // Derive from last path segment: "github.com/gorilla/mux" -> "mux"
            import_path
                .rsplit('/')
                .next()
                .unwrap_or(import_path)
                .to_string()
        }
        Some(a) => a.to_string(),
    };

    if !local_name.is_empty() && local_name != "_" && local_name != "." {
        ctx.imported_names.insert(local_name);
    }

    // Record as raw import; Go imports use path notation (contains '/').
    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: import_path.to_string(),
        // Go imports with '/' in the path are absolute package paths - treat
        // them as relative only if they start with "./" or "../" (rare but valid).
        is_relative: import_path.starts_with("./") || import_path.starts_with("../"),
        dot_count: 0,
        module_path: import_path.to_string(),
    });

    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, import_path.as_bytes());
    ctx.result.relationships.push(Relationship {
        source_id: file_id,
        target_id,
        rel_type: RelationType::Imports,
        confidence: 0.3,
    });
}

// ---------------------------------------------------------------------------
// Definition collection
// ---------------------------------------------------------------------------

/// Walk top-level declarations collecting functions, methods, and type defs.
fn collect_definitions<'a>(
    node: &Node<'a>,
    file_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "function_declaration" => {
                process_function_declaration(&child, file_id, source, ctx);
            }
            "method_declaration" => {
                process_method_declaration(&child, file_id, source, ctx);
            }
            "type_declaration" => {
                process_type_declaration(&child, file_id, source, ctx);
            }
            _ => {}
        }
    }
}

/// `func foo(args) returnType { ... }`
fn process_function_declaration(
    node: &Node<'_>,
    file_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let signature = build_func_signature(node, &name, source, None);
    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Function,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "go".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
    });

    ctx.result.relationships.push(Relationship {
        source_id: file_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });
}

/// `func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) { ... }`
fn process_method_declaration(
    node: &Node<'_>,
    file_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    // Extract receiver type name for the Defines relationship.
    // `func (s *Server) Foo()` -> receiver type is "Server"
    let receiver_type = node
        .child_by_field_name("receiver")
        .and_then(|recv| extract_receiver_type(&recv, source));

    let signature = build_func_signature(node, &name, source, receiver_type.as_deref());
    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    // Use "ReceiverType.MethodName" as the unique key to avoid collisions
    // when multiple types have a method with the same name.
    let qualified_name = if let Some(ref rt) = receiver_type {
        format!("{rt}.{name}")
    } else {
        name.clone()
    };
    ctx.name_to_id.insert(qualified_name, id);

    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Method,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "go".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
    });

    // DEFINES from the receiver struct (if we know it), otherwise from the file.
    let defines_source = if let Some(ref rt) = receiver_type {
        ctx.name_to_id
            .get(rt)
            .copied()
            .unwrap_or(file_id)
    } else {
        file_id
    };

    ctx.result.relationships.push(Relationship {
        source_id: defines_source,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: if receiver_type.is_some() && defines_source != file_id {
            1.0
        } else {
            0.8
        },
    });
}

/// `type Foo struct { ... }` or `type Foo interface { ... }`
fn process_type_declaration(
    node: &Node<'_>,
    file_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let mut cursor = node.walk();
    for spec in node.named_children(&mut cursor) {
        if spec.kind() == "type_spec" {
            process_type_spec(&spec, file_id, source, ctx);
        }
    }
}

fn process_type_spec(
    node: &Node<'_>,
    file_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_default();

    if name.is_empty() {
        return;
    }

    let type_node = node.child_by_field_name("type");
    let type_kind = type_node.as_ref().map(|n| n.kind()).unwrap_or("");

    let is_struct_or_iface = matches!(type_kind, "struct_type" | "interface_type");
    if !is_struct_or_iface {
        // Type aliases and other type defs are skipped for now.
        return;
    }

    let signature = format!("type {name} {type_kind}");
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
        language: "go".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
    });

    ctx.result.relationships.push(Relationship {
        source_id: file_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // Collect embedded types (struct embedding / interface embedding).
    if let Some(type_body) = type_node {
        collect_embeddings(&type_body, id, source, ctx);
    }
}

/// Collect struct embedding and interface embedding relationships.
///
/// Struct: `type Handler struct { http.Handler; Logger }` - each embedded
/// field without an explicit field name is an embedding.
///
/// Interface: `type ReadWriter interface { Reader; Writer }` - embedded
/// interface types.
fn collect_embeddings(
    type_body: &Node<'_>,
    owner_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let mut cursor = type_body.walk();
    for child in type_body.named_children(&mut cursor) {
        match child.kind() {
            // Struct embedding: a field_declaration where the only named child is
            // a type (no field name). tree-sitter-go represents embedded fields
            // as `field_declaration` nodes with a `type` field but no `name`.
            "field_declaration" => {
                // Embedded field: no `name` child, only a type reference.
                if child.child_by_field_name("name").is_none() {
                    if let Some(type_node) = child.child_by_field_name("type") {
                        let embedded = strip_pointer(node_text(&type_node, source));
                        // Strip package qualifier: "http.Handler" -> "Handler"
                        let base_name = embedded.split('.').last().unwrap_or(&embedded).to_string();
                        if !base_name.is_empty() {
                            emit_inherits(owner_id, &base_name, ctx);
                        }
                    }
                }
            }
            // Interface embedding: just a type_name or qualified_type_identifier
            // inside the interface body.
            "type_name" | "qualified_type_identifier" => {
                let embedded = node_text(&child, source);
                let base_name = embedded.split('.').last().unwrap_or(&embedded).to_string();
                if !base_name.is_empty() {
                    emit_inherits(owner_id, &base_name, ctx);
                }
            }
            _ => {}
        }
    }
}

fn emit_inherits(owner_id: Uuid, base_name: &str, ctx: &mut ParseContext<'_>) {
    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(base_name) {
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(base_name) {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, base_name.as_bytes()), 0.8)
    } else {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, base_name.as_bytes()), 0.5)
    };

    ctx.result.relationships.push(Relationship {
        source_id: owner_id,
        target_id,
        rel_type: RelationType::Inherits,
        confidence,
    });
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
    for child in node.children(cursor) {
        if child.kind() == "call_expression" {
            process_call(&child, source, ctx);
        }
        let mut inner = child.walk();
        collect_calls(&child, source, ctx, &mut inner);
    }
}

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // tree-sitter-go: call_expression has a `function` field.
    let Some(function_node) = node.child_by_field_name("function") else {
        return;
    };

    let (callee_name, is_qualified) = extract_callee(&function_node, source);
    if callee_name.is_empty() {
        return;
    }

    let caller_id = find_enclosing_function(node, ctx);

    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&callee_name) {
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(callee_name.split('.').next().unwrap_or("")) {
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.8,
        )
    } else if is_qualified {
        // receiver.Method() - can't resolve statically without type info.
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

    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);

    ctx.result.relationships.push(Relationship {
        source_id,
        target_id,
        rel_type: RelationType::Calls,
        confidence,
    });
}

/// Extract the callee name from a call expression's function node.
///
/// - `foo()`         -> ("foo", false)
/// - `pkg.Func()`    -> ("Func", true)
/// - `s.Method()`    -> ("Method", true)
/// - `a.b.c()`       -> ("c", true)
fn extract_callee(node: &Node<'_>, source: &[u8]) -> (String, bool) {
    match node.kind() {
        "identifier" => (node_text(node, source), false),
        "selector_expression" => {
            // `X.Y` - take the field (method/function name).
            let field = node
                .child_by_field_name("field")
                .map(|n| node_text(&n, source))
                .unwrap_or_default();
            (field, true)
        }
        _ => (String::new(), false),
    }
}

/// Find the innermost enclosing function or method that contains `call_node`.
fn find_enclosing_function(call_node: &Node<'_>, ctx: &ParseContext<'_>) -> Option<Uuid> {
    let call_start = call_node.start_position().row as i32 + 1;
    let mut best: Option<(Uuid, i32)> = None; // (id, range_size)

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

/// Build a human-readable signature for a function or method.
///
/// - Function: `func foo(a int, b string) error`
/// - Method:   `func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request)`
fn build_func_signature(
    node: &Node<'_>,
    name: &str,
    source: &[u8],
    receiver_type: Option<&str>,
) -> String {
    let receiver_text = node
        .child_by_field_name("receiver")
        .map(|n| format!("{} ", node_text(&n, source)));

    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    let result = node
        .child_by_field_name("result")
        .map(|n| format!(" {}", node_text(&n, source)))
        .unwrap_or_default();

    let _ = receiver_type; // used by caller for relationship lookup; not in sig text

    format!(
        "func {}{}{}{}",
        receiver_text.as_deref().unwrap_or(""),
        name,
        params,
        result
    )
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Extract the concrete receiver type from a parameter list node.
///
/// `(s *Server)` -> Some("Server")
/// `(s Server)`  -> Some("Server")
fn extract_receiver_type(recv_node: &Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = recv_node.walk();
    for child in recv_node.named_children(&mut cursor) {
        // Each receiver parameter is a `parameter_declaration`.
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                let raw = node_text(&type_node, source);
                // Strip pointer: "*Server" -> "Server"
                let clean = strip_pointer(raw);
                // Strip package qualifier (unusual for receivers but handle it).
                let base = clean.split('.').last().unwrap_or(&clean).to_string();
                if !base.is_empty() {
                    return Some(base);
                }
            }
        }
    }
    None
}

/// Remove a leading `*` from a type expression (pointer dereference).
fn strip_pointer(s: String) -> String {
    s.trim_start_matches('*').trim().to_string()
}

/// Remove surrounding quotes from an import path string.
fn unquote(s: String) -> String {
    s.trim_matches('"').trim_matches('`').to_string()
}

fn node_text(node: &Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .trim()
        .to_string()
}
