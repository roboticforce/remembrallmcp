//! Tree-sitter based Java parser.
//!
//! Extracts symbols and relationships from a single Java source file.
//!
//! ## What is extracted
//!
//! Symbols:
//! - `class_declaration`       -> SymbolType::Class
//! - `interface_declaration`   -> SymbolType::Class
//! - `enum_declaration`        -> SymbolType::Class
//! - `record_declaration`      -> SymbolType::Class
//! - `method_declaration` inside a class/interface/enum -> SymbolType::Method
//! - `constructor_declaration`                          -> SymbolType::Method
//! - the file itself           -> SymbolType::File
//!
//! Relationships:
//! - `import_declaration`  -> RelationType::Imports
//! - `method_invocation`   -> RelationType::Calls
//! - `extends` clause      -> RelationType::Inherits
//! - `implements` clause   -> RelationType::Inherits
//! - enclosing scope -> symbol -> RelationType::Defines

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Parse a Java file and extract symbols and relationships.
///
/// - `file_path`  - canonical path string stored on each symbol
/// - `source`     - raw UTF-8 source text
/// - `project`    - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_java_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("failed to load Java grammar");

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
        language: "java".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
        layer: None,
    });

    // First pass: collect import declarations.
    let mut cursor = root.walk();
    collect_imports(&root, source_bytes, &mut ctx, &mut cursor);

    // Second pass: collect class/interface/enum/record definitions and their members.
    let mut cursor2 = root.walk();
    collect_definitions(
        &root,
        file_symbol_id,
        None,
        source_bytes,
        &mut ctx,
        &mut cursor2,
    );

    // Third pass: collect method invocations (calls).
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
    /// Simple names imported into this file (the last component of the FQCN).
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
    for child in node.children(cursor) {
        if child.kind() == "import_declaration" {
            process_import(&child, source, ctx);
        }
    }
}

fn process_import(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // `import com.example.Foo;`
    // `import static org.junit.Assert.*;`
    //
    // tree-sitter-java represents the import path as a `scoped_identifier`
    // or `identifier` child (field name: "name" in some grammars). We read the
    // full text of the import node and strip the trailing semicolon to get the
    // raw import string.
    let raw_text = node_text(node, source);

    // Strip "import " prefix, optional "static " keyword, and trailing ";"
    let stripped = raw_text
        .trim_start_matches("import")
        .trim()
        .trim_start_matches("static")
        .trim()
        .trim_end_matches(';')
        .trim();

    if stripped.is_empty() {
        return;
    }

    // Record the simple name (last component) for call confidence scoring.
    // For wildcard imports (`java.util.*`) we cannot know the simple name.
    if !stripped.ends_with('*') {
        let simple = stripped.split('.').last().unwrap_or(stripped);
        ctx.imported_names.insert(simple.to_string());
    }

    let file_id = ctx.result.symbols[0].id;

    // Java imports are always absolute (no relative imports in Java).
    // Store as a raw import with dot_count=0 so the walker can attempt
    // suffix matching against known file paths.
    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: stripped.to_string(),
        is_relative: false,
        dot_count: 0,
        module_path: stripped.to_string(),
    });

    // Emit placeholder relationship; walker will rewrite if resolved.
    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, stripped.as_bytes());
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

/// Type-declaration node kinds that map to SymbolType::Class.
fn is_type_decl(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

/// Recursively walk the AST collecting type declarations and their members.
///
/// - `parent_id`       - UUID of the enclosing scope (file or outer class)
/// - `enclosing_class` - Some(class_id) when inside a class/interface body
fn collect_definitions<'a>(
    node: &Node<'a>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        let kind = child.kind();

        if is_type_decl(kind) {
            let class_id = process_type_decl(&child, parent_id, source, ctx);
            // Recurse into the class body to capture nested types and methods.
            if let Some(body) = child.child_by_field_name("body") {
                let mut inner = body.walk();
                collect_definitions(&body, class_id, Some(class_id), source, ctx, &mut inner);
            }
        } else if kind == "method_declaration" || kind == "constructor_declaration" {
            // Only capture methods when we are inside a type body.
            if let Some(class_id) = enclosing_class {
                process_method(&child, class_id, source, ctx);
            }
        } else if kind == "block" || kind == "class_body" || kind == "interface_body" || kind == "enum_body" {
            // Descend into bodies to find nested type declarations and methods.
            let mut inner = child.walk();
            collect_definitions(&child, parent_id, enclosing_class, source, ctx, &mut inner);
        } else {
            // For any other node, keep descending to catch inner classes in
            // static initializers, anonymous class bodies, etc. We skip
            // anonymous classes (object_creation_expression with class_body)
            // by not matching class_body here when enclosing_class is None -
            // but we still need to recurse the tree.
            let mut inner = child.walk();
            collect_definitions(&child, parent_id, enclosing_class, source, ctx, &mut inner);
        }
    }
}

fn process_type_decl(
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
        language: "java".to_string(),
        project: ctx.project.to_string(),
        signature: Some(build_type_signature(node, &name, source)),
        file_mtime: ctx.file_mtime,
        layer: None,
    });

    // DEFINES: parent scope (file or outer class) defines this type.
    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // INHERITS from `extends` clause.
    // Java grammar: `superclass` field on class_declaration (a `type_identifier`
    // wrapped in `superclass`), or direct `extends_interfaces` on interfaces.
    extract_extends(node, id, source, ctx);

    // INHERITS from `implements` clause.
    extract_implements(node, id, source, ctx);

    id
}

/// Emit Inherits relationships for the `extends SuperClass` clause.
fn extract_extends(node: &Node<'_>, class_id: Uuid, source: &[u8], ctx: &mut ParseContext<'_>) {
    // class_declaration has a field "superclass" that is a `superclass` node
    // containing the type name.
    // interface_declaration has "extends_interfaces" containing "type_list".
    for field in &["superclass", "extends_interfaces"] {
        if let Some(super_node) = node.child_by_field_name(field) {
            emit_type_list_inherits(&super_node, class_id, source, ctx);
        }
    }
}

/// Emit Inherits relationships for the `implements InterfaceList` clause.
fn extract_implements(node: &Node<'_>, class_id: Uuid, source: &[u8], ctx: &mut ParseContext<'_>) {
    if let Some(impl_node) = node.child_by_field_name("interfaces") {
        emit_type_list_inherits(&impl_node, class_id, source, ctx);
    }
}

/// Walk a node that may contain type_identifier or type_list children and emit
/// an Inherits relationship for each one.
fn emit_type_list_inherits(
    node: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let mut cursor = node.walk();
    // Recursively scan for type_identifier nodes (the actual class/interface names).
    emit_inherits_for_node(node, class_id, source, ctx, &mut cursor);
}

fn emit_inherits_for_node<'a>(
    node: &Node<'a>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "type_identifier" => {
                let base_name = node_text(&child, source);
                if base_name.is_empty() {
                    continue;
                }
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
                    source_id: class_id,
                    target_id,
                    rel_type: RelationType::Inherits,
                    confidence,
                });
            }
            _ => {
                let mut inner = child.walk();
                emit_inherits_for_node(&child, class_id, source, ctx, &mut inner);
            }
        }
    }
}

fn process_method(
    node: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<constructor>".to_string());

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Method,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "java".to_string(),
        project: ctx.project.to_string(),
        signature: Some(build_method_signature(node, &name, source)),
        file_mtime: ctx.file_mtime,
        layer: None,
    });

    // DEFINES: class defines this method.
    ctx.result.relationships.push(Relationship {
        source_id: class_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // USES_TYPE: relationships from parameter types and return type.
    collect_type_annotations(node, id, source, ctx);

    id
}

// ---------------------------------------------------------------------------
// Type annotation extraction
// ---------------------------------------------------------------------------

/// Walk a method or constructor node's parameter list and return type,
/// collecting `UsesType` relationships for every non-builtin type name found.
fn collect_type_annotations(
    method_node: &Node<'_>,
    method_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let mut type_names: Vec<String> = Vec::new();

    // 1. Parameter types: walk `formal_parameters` -> `formal_parameter` children.
    //    Each `formal_parameter` has a `type` field.
    if let Some(params) = method_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            // formal_parameter, spread_parameter, receiver_parameter
            if let Some(type_node) = param.child_by_field_name("type") {
                extract_type_identifiers(&type_node, source, &mut type_names);
            }
        }
    }

    // 2. Return type: `type` field on method_declaration (absent on constructors).
    if let Some(return_type) = method_node.child_by_field_name("type") {
        extract_type_identifiers(&return_type, source, &mut type_names);
    }

    // 3. Create UsesType relationships for non-builtin types.
    for type_name in type_names {
        if is_builtin_type(&type_name) {
            continue;
        }
        let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&type_name) {
            (id, 1.0_f32)
        } else if ctx.imported_names.contains(&type_name) {
            (Uuid::new_v5(&Uuid::NAMESPACE_OID, type_name.as_bytes()), 0.8)
        } else {
            (Uuid::new_v5(&Uuid::NAMESPACE_OID, type_name.as_bytes()), 0.5)
        };

        ctx.result.relationships.push(Relationship {
            source_id: method_id,
            target_id,
            rel_type: RelationType::UsesType,
            confidence,
        });
    }
}

/// Recursively extract all type identifier names from a type node.
///
/// Node kinds handled:
/// - `type_identifier`          -> push the name directly (e.g. `Order`)
/// - `generic_type`             -> recurse (e.g. `List<Order>` yields `Order`)
/// - `array_type`               -> recurse into the element type (e.g. `Order[]`)
/// - `scoped_type_identifier`   -> push only the rightmost identifier component
///                                 (e.g. `com.example.Order` -> `Order`)
/// - everything else            -> recurse into named children
fn extract_type_identifiers(node: &Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "type_identifier" => {
            let name = node_text(node, source);
            if !name.is_empty() {
                out.push(name);
            }
        }
        "scoped_type_identifier" => {
            // e.g. `java.util.List` or `com.example.Foo` - use only the rightmost
            // component because that is what appears in import simple-name lists.
            let last_idx = node.named_child_count().saturating_sub(1) as u32;
            if let Some(last) = node.named_child(last_idx) {
                if last.kind() == "type_identifier" || last.kind() == "identifier" {
                    let name = node_text(&last, source);
                    if !name.is_empty() {
                        out.push(name);
                    }
                }
            }
        }
        "generic_type" => {
            // The first named child of a generic_type is the base type_identifier
            // (or scoped_type_identifier); the remaining children are type_arguments.
            // Recurse into all named children so we pick up both the outer type
            // and any type arguments (e.g. `Map<String, Order>` -> `Order`).
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_type_identifiers(&child, source, out);
            }
        }
        "array_type" => {
            // `element` field holds the component type.
            if let Some(elem) = node.child_by_field_name("element") {
                extract_type_identifiers(&elem, source, out);
            } else {
                // Fallback: recurse into all named children.
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    extract_type_identifiers(&child, source, out);
                }
            }
        }
        "type_arguments" | "wildcard" => {
            // `<T extends Foo>` or `? extends Bar` - recurse to collect Foo/Bar.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_type_identifiers(&child, source, out);
            }
        }
        _ => {
            // Catch-all: recurse so we don't miss anything in unusual nodes.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_type_identifiers(&child, source, out);
            }
        }
    }
}

/// Returns true for Java primitive types, their boxed equivalents, and the most
/// common standard library types that are not project-specific.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // Primitives
        "int" | "long" | "short" | "byte" | "float" | "double" | "boolean" | "char" | "void"
        // Boxed primitives + top-level types
        | "String" | "Object" | "Integer" | "Long" | "Short" | "Byte" | "Float" | "Double"
        | "Boolean" | "Character" | "Void" | "Class" | "Number" | "Comparable" | "Iterable"
        // Common functional interfaces
        | "Runnable" | "Callable"
        // Exception hierarchy
        | "Throwable" | "Exception" | "RuntimeException" | "Error"
        // Common collections / utility types
        | "List" | "Map" | "Set" | "Collection" | "ArrayList" | "HashMap" | "HashSet"
        | "Optional" | "Stream" | "Iterator"
        // Common annotations / meta types
        | "Enum" | "Override" | "Deprecated" | "SuppressWarnings" | "FunctionalInterface"
    )
}

// ---------------------------------------------------------------------------
// Signature builders
// ---------------------------------------------------------------------------

/// Build a human-readable signature for a class/interface/enum.
///
/// Examples:
///   `class UserService extends BaseService implements Auditable`
///   `interface PaymentGateway`
///   `enum Status`
fn build_type_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    let keyword = match node.kind() {
        "interface_declaration" => "interface",
        "enum_declaration" => "enum",
        "record_declaration" => "record",
        "annotation_type_declaration" => "@interface",
        _ => "class",
    };

    let mut sig = format!("{keyword} {name}");

    if let Some(super_node) = node.child_by_field_name("superclass") {
        let super_text = node_text(&super_node, source);
        // Strip the "extends " prefix that tree-sitter includes in the node text.
        let cleaned = super_text
            .trim_start_matches("extends")
            .trim();
        if !cleaned.is_empty() {
            sig.push_str(&format!(" extends {cleaned}"));
        }
    }

    if let Some(impl_node) = node.child_by_field_name("interfaces") {
        let impl_text = node_text(&impl_node, source);
        let cleaned = impl_text
            .trim_start_matches("implements")
            .trim();
        if !cleaned.is_empty() {
            sig.push_str(&format!(" implements {cleaned}"));
        }
    }

    sig
}

/// Build a human-readable signature for a method or constructor.
///
/// Examples:
///   `public void processOrder(Order order, User user)`
///   `UserService(Repository repo)`
fn build_method_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    // Collect modifiers (public, static, final, etc.).
    let modifiers = node
        .child_by_field_name("modifiers")
        .map(|n| format!("{} ", node_text(&n, source)))
        .unwrap_or_default();

    // Return type (absent for constructors).
    let return_type = node
        .child_by_field_name("type")
        .map(|n| format!("{} ", node_text(&n, source)))
        .unwrap_or_default();

    // Parameter list.
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    format!("{modifiers}{return_type}{name}{params}")
        .trim()
        .to_string()
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
        if child.kind() == "method_invocation" {
            process_call(&child, source, ctx);
        }
        let mut inner = child.walk();
        collect_calls(&child, source, ctx, &mut inner);
    }
}

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // tree-sitter-java method_invocation fields:
    //   name     - the method name identifier
    //   object   - the receiver expression (optional; absent for unqualified calls)
    //   arguments - argument list

    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let callee_name = node_text(&name_node, source);
    if callee_name.is_empty() {
        return;
    }

    let has_object = node.child_by_field_name("object").is_some();

    // Determine if the call goes through `this` (analogous to Python's `self`).
    let is_this_call = node
        .child_by_field_name("object")
        .map(|obj| {
            let text = node_text(&obj, source);
            text == "this" || text == "super"
        })
        .unwrap_or(false);

    // Find the innermost enclosing method that contains this call node.
    let caller_id = find_enclosing_method(node, ctx);

    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&callee_name) {
        // Defined in this file - high confidence.
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(&callee_name) {
        // Imported name - medium-high confidence.
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.8,
        )
    } else if is_this_call {
        // this.method() or super.method() - same-class call, medium confidence.
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.6,
        )
    } else if has_object {
        // obj.method() - unknown receiver type.
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.5,
        )
    } else {
        // Unqualified call - could be a method in the same class.
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

/// Find the UUID of the innermost method/constructor that contains `node`.
/// Returns None if the call is outside any method (e.g., in a field initializer).
fn find_enclosing_method(call_node: &Node<'_>, ctx: &ParseContext<'_>) -> Option<Uuid> {
    let call_start = call_node.start_position().row as i32 + 1;

    let mut best: Option<(Uuid, i32, i32)> = None;

    for sym in &ctx.result.symbols {
        if sym.symbol_type != SymbolType::Method {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParseResult {
        let now = chrono::Utc::now();
        parse_java_file("Test.java", src, "test", now)
    }

    fn uses_type_rels(result: &FileParseResult) -> Vec<(uuid::Uuid, uuid::Uuid, f32)> {
        result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::UsesType)
            .map(|r| (r.source_id, r.target_id, r.confidence))
            .collect()
    }

    fn method_id(result: &FileParseResult, name: &str) -> Option<uuid::Uuid> {
        result
            .symbols
            .iter()
            .find(|s| s.symbol_type == SymbolType::Method && s.name == name)
            .map(|s| s.id)
    }

    #[test]
    fn test_uses_type_param_annotation() {
        let src = r#"
class OrderService {
    public void processOrder(Order order) {}
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        assert!(!rels.is_empty(), "expected at least one UsesType relationship");
        let process_id = method_id(&result, "processOrder").expect("processOrder not found");
        assert!(
            rels.iter().any(|(src_id, _, _)| *src_id == process_id),
            "UsesType should originate from processOrder"
        );
    }

    #[test]
    fn test_uses_type_return_annotation() {
        let src = r#"
class UserRepository {
    public User findById(long id) { return null; }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let find_id = method_id(&result, "findById").expect("findById not found");
        assert!(
            rels.iter().any(|(src_id, _, _)| *src_id == find_id),
            "UsesType should originate from findById for User return type"
        );
    }

    #[test]
    fn test_no_uses_type_for_builtins() {
        let src = r#"
class Calculator {
    public int add(int a, int b) { return a + b; }
    public String format(double value) { return ""; }
}
"#;
        let result = parse(src);
        // `int`, `double` are builtins - no UsesType for them.
        // `String` is also a builtin in our list.
        let rels = uses_type_rels(&result);
        assert!(
            rels.is_empty(),
            "primitive and String types should not produce UsesType, got: {rels:?}"
        );
    }

    #[test]
    fn test_uses_type_generic_type_param() {
        let src = r#"
class CartService {
    public List<Product> getItems(Cart cart) { return null; }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let get_id = method_id(&result, "getItems").expect("getItems not found");
        // `List` is a builtin, but `Product` and `Cart` should produce UsesType.
        let from_get: Vec<_> = rels.iter().filter(|(s, _, _)| *s == get_id).collect();
        assert!(
            from_get.len() >= 2,
            "expected UsesType for Product and Cart (List is filtered), got {from_get:?}"
        );
    }

    #[test]
    fn test_uses_type_confidence_same_file() {
        let src = r#"
class Payment {}
class PaymentService {
    public void process(Payment p) {}
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let process_id = method_id(&result, "process").expect("process not found");
        let rel = rels
            .iter()
            .find(|(s, _, _)| *s == process_id)
            .expect("no UsesType from process");
        assert_eq!(rel.2, 1.0, "same-file type should have confidence 1.0");
    }

    #[test]
    fn test_uses_type_confidence_external() {
        let src = r#"
import com.example.Order;
class ShipmentService {
    public void ship(Order o) {}
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let ship_id = method_id(&result, "ship").expect("ship not found");
        let rel = rels
            .iter()
            .find(|(s, _, _)| *s == ship_id)
            .expect("no UsesType from ship");
        assert_eq!(
            rel.2, 0.8,
            "imported type should have confidence 0.8"
        );
    }

    #[test]
    fn test_uses_type_array_type() {
        let src = r#"
class FileProcessor {
    public void process(Document[] docs) {}
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let proc_id = method_id(&result, "process").expect("process not found");
        assert!(
            rels.iter().any(|(s, _, _)| *s == proc_id),
            "array element type Document should produce UsesType"
        );
    }
}
