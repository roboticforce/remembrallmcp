//! Tree-sitter based Kotlin parser.
//!
//! Extracts symbols and relationships from a single Kotlin source file.
//!
//! # Symbols extracted
//! - `class_declaration`, `object_declaration` -> SymbolType::Class
//! - `companion_object` -> SymbolType::Class
//! - `function_declaration` inside a class/object body -> SymbolType::Method
//! - Top-level `function_declaration` -> SymbolType::Function
//! - The file itself -> SymbolType::File
//!
//! # Relationships extracted
//! - `import` -> RelationType::Imports
//! - `call_expression` -> RelationType::Calls
//! - `delegation_specifiers` in class declarations -> RelationType::Inherits
//! - Classes/functions defined inside a scope -> RelationType::Defines

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Parse a Kotlin file and extract symbols and relationships.
///
/// - `file_path`  - canonical path string stored on each symbol
/// - `source`     - raw UTF-8 source text
/// - `project`    - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_kotlin_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .expect("failed to load Kotlin grammar");

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

    // File-level symbol - always the first entry in symbols.
    let file_symbol_id = Uuid::new_v4();
    ctx.result.symbols.push(Symbol {
        id: file_symbol_id,
        name: file_path.to_string(),
        symbol_type: SymbolType::File,
        file_path: file_path.to_string(),
        start_line: Some(1),
        end_line: Some(source.lines().count() as i32),
        language: "kotlin".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
    });

    // Pass 1: collect imports from top-level `import` nodes.
    collect_imports(&root, source_bytes, &mut ctx);

    // Pass 2: collect class, object, and function declarations recursively.
    collect_definitions(&root, file_symbol_id, false, source_bytes, &mut ctx);

    // Pass 3: collect call expressions.
    collect_calls(&root, source_bytes, &mut ctx);

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
    /// Short names imported into this file (for confidence scoring).
    imported_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Import collection
// ---------------------------------------------------------------------------

/// Walk top-level children of `source_file` looking for `import` nodes.
///
/// Kotlin AST: the root `source_file` contains `import` nodes as direct
/// children. Each `import` node contains a `qualified_identifier` (and
/// optionally a wildcard `*`).
fn collect_imports(root: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "import" {
            process_import(&child, source, ctx);
        }
    }
}

fn process_import(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // The full text of this node is e.g. "import org.jetbrains.exposed.sql.Table"
    // or "import org.jetbrains.exposed.sql.transactions.*"
    // Strip the leading "import " keyword and trim.
    let full_text = node_text(node, source);
    let after_import = full_text
        .trim_start_matches("import")
        .trim_start();

    // Handle aliased imports: `import com.example.Foo as Bar`
    let (module_raw, alias_opt) = if let Some(idx) = after_import.find(" as ") {
        let module = after_import[..idx].trim();
        let alias = after_import[idx + 4..].trim();
        (module.to_string(), Some(alias.to_string()))
    } else {
        (after_import.trim().to_string(), None)
    };

    if module_raw.is_empty() {
        return;
    }

    // Record the short name (last segment or alias) for call confidence scoring.
    if let Some(alias) = &alias_opt {
        ctx.imported_names.insert(alias.clone());
    } else if !module_raw.ends_with('*') {
        if let Some(last) = module_raw.split('.').last() {
            ctx.imported_names.insert(last.to_string());
        }
    }

    let file_id = ctx.result.symbols[0].id;

    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: module_raw.clone(),
        is_relative: false,
        dot_count: 0,
        module_path: module_raw.clone(),
    });

    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, module_raw.as_bytes());
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

/// Recursively collect class, object, companion_object, and function declarations.
///
/// - `parent_id`     - UUID of the enclosing scope (file or class/object)
/// - `in_class_body` - true when we're inside a class/object body
fn collect_definitions(
    node: &Node<'_>,
    parent_id: Uuid,
    in_class_body: bool,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" | "object_declaration" => {
                let class_id = process_type_declaration(&child, parent_id, source, ctx);
                // Find and recurse into the class_body child.
                if let Some(body) = find_child_by_kind(&child, "class_body") {
                    collect_definitions(&body, class_id, true, source, ctx);
                }
                // Also handle enum_class_body (for enum classes).
                if let Some(body) = find_child_by_kind(&child, "enum_class_body") {
                    collect_definitions(&body, class_id, true, source, ctx);
                }
            }
            "companion_object" => {
                let comp_id = process_companion_object(&child, parent_id, source, ctx);
                if let Some(body) = find_child_by_kind(&child, "class_body") {
                    collect_definitions(&body, comp_id, true, source, ctx);
                }
            }
            "function_declaration" => {
                let sym_id = process_function(&child, parent_id, in_class_body, source, ctx);
                // Recurse into function body for nested lambdas/local classes.
                if let Some(body) = find_child_by_kind(&child, "function_body") {
                    collect_definitions(&body, sym_id, false, source, ctx);
                }
            }
            // Descend into other block constructs transparently.
            _ => {
                collect_definitions(&child, parent_id, in_class_body, source, ctx);
            }
        }
    }
}

/// Process a `class_declaration` or `object_declaration` node.
///
/// In this grammar, interfaces are also `class_declaration` nodes (the
/// keyword is `interface` instead of `class`).
fn process_type_declaration(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    // The name is in an `identifier` field child.
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    let signature = build_type_signature(node, &name, source);

    ctx.name_to_id.insert(name.clone(), id);
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Class,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "kotlin".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
        file_mtime: ctx.file_mtime,
    });

    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // Emit INHERITS relationships from delegation_specifiers.
    collect_supertypes(node, id, source, ctx);

    id
}

/// Process a `companion_object` node.
fn process_companion_object(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "Companion".to_string());

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
        language: "kotlin".to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("companion object {name}")),
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

/// Process a `function_declaration` node.
fn process_function(
    node: &Node<'_>,
    parent_id: Uuid,
    in_class_body: bool,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let symbol_type = if in_class_body {
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
        language: "kotlin".to_string(),
        project: ctx.project.to_string(),
        signature: Some(signature),
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

/// Walk `delegation_specifiers` to emit INHERITS relationships.
///
/// Kotlin AST: `delegation_specifiers` contains `delegation_specifier` nodes,
/// each of which contains either a `constructor_invocation` or a `type` (user_type).
fn collect_supertypes(
    node: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let Some(specs_node) = find_child_by_kind(node, "delegation_specifiers") else {
        return;
    };

    let mut specs_cursor = specs_node.walk();
    for spec in specs_node.children(&mut specs_cursor) {
        if spec.kind() != "delegation_specifier" {
            continue;
        }

        // A delegation_specifier contains one of:
        //   constructor_invocation -> Bar(...)
        //   type -> IFoo (a user_type directly)
        let mut sc = spec.walk();
        for inner in spec.children(&mut sc) {
            let base_name = match inner.kind() {
                "constructor_invocation" => {
                    // First child of constructor_invocation is the type (user_type).
                    find_child_by_kind(&inner, "user_type")
                        .map(|n| extract_simple_type_name(&n, source))
                        .unwrap_or_default()
                }
                "user_type" => extract_simple_type_name(&inner, source),
                _ => continue,
            };

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
    }
}

/// Extract the simple (unqualified) type name from a `user_type` node.
///
/// A `user_type` may look like `List<String>` or just `IFoo`.
/// We want the first identifier segment: `List` or `IFoo`.
fn extract_simple_type_name(node: &Node<'_>, source: &[u8]) -> String {
    // user_type contains identifier children (possibly multiple for qualified types).
    // Take the first one.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return node_text(&child, source);
        }
    }
    // Fallback: strip generics from the full text.
    let full = node_text(node, source);
    full.split('<').next().unwrap_or(&full).trim().to_string()
}

// ---------------------------------------------------------------------------
// Signature builders
// ---------------------------------------------------------------------------

/// Build a human-readable signature for a class/object/interface.
fn build_type_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    let mut modifiers: Vec<String> = Vec::new();

    // Collect modifiers from the `modifiers` child node.
    if let Some(mods_node) = find_child_by_kind(node, "modifiers") {
        let mut mc = mods_node.walk();
        for modifier in mods_node.children(&mut mc) {
            // modifier children are things like class_modifier, visibility_modifier, etc.
            // Recurse one level to get the actual keyword text.
            let mut mc2 = modifier.walk();
            for kw in modifier.children(&mut mc2) {
                let text = node_text(&kw, source);
                if matches!(
                    text.as_str(),
                    "data"
                        | "sealed"
                        | "abstract"
                        | "open"
                        | "inner"
                        | "value"
                        | "enum"
                        | "annotation"
                ) {
                    modifiers.push(text);
                }
            }
        }
    }

    // Determine keyword: interface classes use "interface", objects use "object".
    let keyword = {
        let mut c = node.walk();
        let kw_text = node
            .children(&mut c)
            .find(|ch| matches!(ch.kind(), "interface" | "class" | "object"))
            .map(|ch| node_text(&ch, source))
            .unwrap_or_else(|| match node.kind() {
                "object_declaration" => "object".to_string(),
                _ => "class".to_string(),
            });
        kw_text
    };

    if modifiers.is_empty() {
        format!("{keyword} {name}")
    } else {
        format!("{} {keyword} {name}", modifiers.join(" "))
    }
}

/// Build a human-readable signature for a function.
fn build_function_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    let mut prefix_parts: Vec<String> = Vec::new();

    if let Some(mods_node) = find_child_by_kind(node, "modifiers") {
        let mut mc = mods_node.walk();
        for modifier in mods_node.children(&mut mc) {
            let mut mc2 = modifier.walk();
            for kw in modifier.children(&mut mc2) {
                let text = node_text(&kw, source);
                if matches!(
                    text.as_str(),
                    "suspend"
                        | "inline"
                        | "operator"
                        | "override"
                        | "private"
                        | "protected"
                        | "internal"
                        | "public"
                        | "abstract"
                        | "open"
                ) {
                    prefix_parts.push(text);
                }
            }
        }
    }

    // Parameters: look for `function_value_parameters` child.
    let params = find_child_by_kind(node, "function_value_parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    // Return type: the `type` field child (after `:` and before `{`).
    // In the grammar this is a direct child with kind "user_type", "nullable_type", etc.
    // It appears after the function_value_parameters and before the function_body.
    let return_type = find_return_type(node, source);

    let fun_part = format!(
        "fun {name}{params}{}",
        return_type
            .as_deref()
            .map(|t| format!(": {t}"))
            .unwrap_or_default()
    );

    if prefix_parts.is_empty() {
        fun_part
    } else {
        format!("{} {fun_part}", prefix_parts.join(" "))
    }
}

/// Find the return type annotation of a function_declaration.
///
/// In the Kotlin grammar the return type appears as a direct child after
/// the `function_value_parameters`. It is a `type` field in the grammar spec,
/// but tree-sitter exposes it as a named child with kinds like `user_type`,
/// `nullable_type`, `function_type`, etc. We scan children positionally to
/// find it between the parameters and the function body.
fn find_return_type(node: &Node<'_>, source: &[u8]) -> Option<String> {
    let mut after_params = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_value_parameters" {
            after_params = true;
            continue;
        }
        if !after_params {
            continue;
        }
        match child.kind() {
            "function_body" | "block" => break,
            "user_type" | "nullable_type" | "function_type" | "parenthesized_type"
            | "dynamic_type" => {
                return Some(node_text(&child, source));
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Call collection
// ---------------------------------------------------------------------------

/// Walk the entire tree collecting `call_expression` nodes.
fn collect_calls(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "call_expression" {
            process_call(&child, source, ctx);
        }
        collect_calls(&child, source, ctx);
    }
}

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let Some(callee_node) = node.child(0) else {
        return;
    };

    let (callee_name, is_chained) = extract_callee_name(&callee_node, source);
    if callee_name.is_empty() {
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
    } else if is_chained {
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

/// Extract the leaf callee name and whether it was accessed via navigation.
fn extract_callee_name(node: &Node<'_>, source: &[u8]) -> (String, bool) {
    match node.kind() {
        "identifier" | "simple_identifier" => (node_text(node, source), false),
        "navigation_expression" => {
            // Last child is the right-hand identifier after the final `.`
            // AST: navigation_expression -> [expr, ".", identifier]
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            // Walk backwards to find the last identifier.
            let name = children
                .iter()
                .rev()
                .find(|c| c.kind() == "identifier" || c.kind() == "simple_identifier")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            (name, true)
        }
        _ => {
            let text = node_text(node, source);
            let name = text.split('.').last().unwrap_or(&text).to_string();
            let is_chained = name != text;
            (name, is_chained)
        }
    }
}

/// Find the UUID of the innermost function/method that contains `call_node`.
fn find_enclosing_function(call_node: &Node<'_>, ctx: &ParseContext<'_>) -> Option<Uuid> {
    let call_start = call_node.start_position().row as i32 + 1;

    let mut best: Option<(Uuid, i32, i32)> = None;

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

/// Find the first direct child of `node` with the given `kind`.
fn find_child_by_kind<'a>(node: &Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| c.kind() == kind)
}

fn node_text(node: &Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .trim()
        .to_string()
}
