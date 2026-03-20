//! Tree-sitter based TypeScript/JavaScript parser.
//!
//! Handles .ts, .tsx, .js, .jsx files.
//! Extracts: functions, arrow functions, classes, methods, interfaces, imports.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Language, Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Language variant - determines grammar and file extension handling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TsLang {
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
}

impl TsLang {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            _ => None,
        }
    }

    fn tree_sitter_language(self) -> Language {
        match self {
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript | Self::Jsx => tree_sitter_javascript::LANGUAGE.into(),
        }
    }

    pub fn language_tag(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Jsx => "jsx",
        }
    }
}

/// Parse a TypeScript or JavaScript file and extract symbols and relationships.
pub fn parse_ts_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
    lang: TsLang,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.tree_sitter_language())
        .expect("failed to load TS/JS grammar");

    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!("tree-sitter failed to parse {file_path}");
        return FileParseResult::default();
    };

    let source_bytes = source.as_bytes();
    let root = tree.root_node();

    let mut ctx = TsParseContext {
        file_path,
        project,
        file_mtime,
        lang_tag: lang.language_tag(),
        result: FileParseResult::default(),
        name_to_id: HashMap::new(),
        imported_names: HashSet::new(),
    };

    // File-level symbol.
    let file_symbol_id = Uuid::new_v4();
    ctx.result.symbols.push(Symbol {
        id: file_symbol_id,
        name: file_path.to_string(),
        symbol_type: SymbolType::File,
        file_path: file_path.to_string(),
        start_line: Some(1),
        end_line: Some(source.lines().count() as i32),
        language: ctx.lang_tag.to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
    });

    // First pass: collect imports.
    let mut cursor = root.walk();
    collect_imports(&root, source_bytes, &mut ctx, &mut cursor);

    // Second pass: collect definitions.
    let mut cursor2 = root.walk();
    collect_definitions(&root, file_symbol_id, None, source_bytes, &mut ctx, &mut cursor2);

    // Third pass: collect calls.
    let mut cursor3 = root.walk();
    collect_calls(&root, source_bytes, &mut ctx, &mut cursor3);

    ctx.result
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

struct TsParseContext<'a> {
    file_path: &'a str,
    project: &'a str,
    file_mtime: DateTime<Utc>,
    lang_tag: &'a str,
    result: FileParseResult,
    name_to_id: HashMap<String, Uuid>,
    imported_names: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Import collection
// ---------------------------------------------------------------------------

fn collect_imports<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            // `import { foo } from 'bar'` / `import foo from 'bar'`
            "import_statement" => process_import_statement(&child, source, ctx),
            // `export { foo } from 'bar'` / `export * from 'bar'`
            "export_statement" => process_export_statement(&child, source, ctx),
            _ => {}
        }
    }
}

fn process_import_statement(node: &Node<'_>, source: &[u8], ctx: &mut TsParseContext<'_>) {
    // source field holds the string literal module specifier.
    let module_name = node
        .child_by_field_name("source")
        .map(|n| strip_quotes(&node_text(&n, source)))
        .unwrap_or_default();

    if module_name.is_empty() {
        return;
    }

    let file_id = ctx.result.symbols[0].id;

    // Relative imports start with './' or '../'. Record for walker resolution.
    let is_relative = module_name.starts_with('.');
    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: module_name.clone(),
        is_relative,
        dot_count: if is_relative { 1 } else { 0 }, // TS uses path strings not dot-counts
        module_path: module_name.clone(),
    });

    // Placeholder relationship - walker rewrites target_id for resolved imports.
    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, module_name.as_bytes());
    ctx.result.relationships.push(Relationship {
        source_id: file_id,
        target_id,
        rel_type: RelationType::Imports,
        confidence: if is_relative { 0.3 } else { 0.8 },
    });

    // Collect imported names for call-site scoring.
    // import_clause -> named_imports -> import_specifier -> name
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_import_clause_names(&child, source, ctx);
    }
}

fn collect_import_clause_names(node: &Node<'_>, source: &[u8], ctx: &mut TsParseContext<'_>) {
    match node.kind() {
        "identifier" => {
            // default import: `import Foo from 'bar'`
            ctx.imported_names.insert(node_text(node, source));
        }
        "named_imports" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "import_specifier" {
                    // `{ foo as bar }` - use alias if present, otherwise original name
                    let name = child
                        .child_by_field_name("alias")
                        .or_else(|| child.child_by_field_name("name"))
                        .map(|n| node_text(&n, source))
                        .unwrap_or_default();
                    if !name.is_empty() {
                        ctx.imported_names.insert(name);
                    }
                }
            }
        }
        "namespace_import" => {
            // `* as ns` - record the namespace alias
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    ctx.imported_names.insert(node_text(&child, source));
                }
            }
        }
        "import_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_import_clause_names(&child, source, ctx);
            }
        }
        _ => {}
    }
}

fn process_export_statement(node: &Node<'_>, source: &[u8], ctx: &mut TsParseContext<'_>) {
    // Only care about re-exports: `export { foo } from 'bar'`
    if let Some(source_node) = node.child_by_field_name("source") {
        let module_name = strip_quotes(&node_text(&source_node, source));
        if !module_name.is_empty() {
            let file_id = ctx.result.symbols[0].id;
            let is_relative = module_name.starts_with('.');
            ctx.result.raw_imports.push(RawImport {
                source_id: file_id,
                module_raw: module_name.clone(),
                is_relative,
                dot_count: if is_relative { 1 } else { 0 },
                module_path: module_name.clone(),
            });
            let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, module_name.as_bytes());
            ctx.result.relationships.push(Relationship {
                source_id: file_id,
                target_id,
                rel_type: RelationType::Imports,
                confidence: if is_relative { 0.3 } else { 0.8 },
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Definition collection
// ---------------------------------------------------------------------------

fn collect_definitions<'a>(
    node: &Node<'a>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "function_declaration" | "generator_function_declaration" => {
                let sym_id =
                    process_function_declaration(&child, parent_id, enclosing_class, source, ctx);
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(&body, sym_id, None, source, ctx, &mut inner);
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                let class_id = process_class_declaration(&child, parent_id, source, ctx);
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(&body, class_id, Some(class_id), source, ctx, &mut inner);
                }
            }
            "interface_declaration" => {
                // Treat interfaces as SymbolType::Class.
                process_interface_declaration(&child, parent_id, source, ctx);
            }
            "type_alias_declaration" => {
                // `type Foo = { ... }` - treat as Class (interface-like).
                process_type_alias_declaration(&child, parent_id, source, ctx);
            }
            "method_definition" => {
                // Inside a class body.
                let sym_id =
                    process_method_definition(&child, parent_id, enclosing_class, source, ctx);
                if let Some(body) = child.child_by_field_name("body") {
                    let mut inner = body.walk();
                    collect_definitions(&body, sym_id, None, source, ctx, &mut inner);
                }
            }
            "public_field_definition" => {
                // Class field with arrow function value:
                //   fetch: (req: Request) => Response = (req) => { ... }
                // The `value` field (from _initializer) holds the arrow_function.
                if let Some(value) = child.child_by_field_name("value") {
                    if matches!(
                        value.kind(),
                        "arrow_function" | "function_expression" | "generator_function_expression"
                    ) {
                        let name = child
                            .child_by_field_name("name")
                            .map(|n| node_text(&n, source))
                            .unwrap_or_else(|| "<anonymous>".to_string());

                        let sym_type = if enclosing_class.is_some() {
                            SymbolType::Method
                        } else {
                            SymbolType::Function
                        };
                        let signature = build_fn_signature(&value, &name, source, "const");
                        let start_line = child.start_position().row as i32 + 1;
                        let end_line = child.end_position().row as i32 + 1;
                        let sym_id = Uuid::new_v4();

                        ctx.name_to_id.insert(name.clone(), sym_id);
                        ctx.result.symbols.push(Symbol {
                            id: sym_id,
                            name,
                            symbol_type: sym_type,
                            file_path: ctx.file_path.to_string(),
                            start_line: Some(start_line),
                            end_line: Some(end_line),
                            language: ctx.lang_tag.to_string(),
                            project: ctx.project.to_string(),
                            signature: Some(signature),
                            file_mtime: ctx.file_mtime,
                        });
                        ctx.result.relationships.push(Relationship {
                            source_id: parent_id,
                            target_id: sym_id,
                            rel_type: RelationType::Defines,
                            confidence: 1.0,
                        });

                        if let Some(body) = value.child_by_field_name("body") {
                            let mut inner = body.walk();
                            collect_definitions(&body, sym_id, None, source, ctx, &mut inner);
                        }
                    }
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                // `const foo = () => ...` or `const foo = function() ...`
                let mut dc = child.walk();
                for decl in child.named_children(&mut dc) {
                    if decl.kind() == "variable_declarator" {
                        process_arrow_function_or_fn_expr(
                            &decl,
                            parent_id,
                            enclosing_class,
                            source,
                            ctx,
                        );
                    }
                }
            }
            "export_statement" => {
                // `export function foo() {}` / `export class Foo {}` / `export const foo = () => {}`
                // `export interface Foo {}` / `export type Foo = { ... }`
                let mut dc = child.walk();
                for inner_child in child.named_children(&mut dc) {
                    match inner_child.kind() {
                        "function_declaration" | "generator_function_declaration" => {
                            let sym_id = process_function_declaration(
                                &inner_child,
                                parent_id,
                                enclosing_class,
                                source,
                                ctx,
                            );
                            if let Some(body) = inner_child.child_by_field_name("body") {
                                let mut bc = body.walk();
                                collect_definitions(&body, sym_id, None, source, ctx, &mut bc);
                            }
                        }
                        "class_declaration" | "abstract_class_declaration" => {
                            let class_id = process_class_declaration(
                                &inner_child,
                                parent_id,
                                source,
                                ctx,
                            );
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
                        "interface_declaration" => {
                            process_interface_declaration(&inner_child, parent_id, source, ctx);
                        }
                        "type_alias_declaration" => {
                            process_type_alias_declaration(&inner_child, parent_id, source, ctx);
                        }
                        "lexical_declaration" | "variable_declaration" => {
                            let mut dc2 = inner_child.walk();
                            for decl in inner_child.named_children(&mut dc2) {
                                if decl.kind() == "variable_declarator" {
                                    process_arrow_function_or_fn_expr(
                                        &decl,
                                        parent_id,
                                        enclosing_class,
                                        source,
                                        ctx,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                let mut inner = child.walk();
                collect_definitions(&child, parent_id, enclosing_class, source, ctx, &mut inner);
            }
        }
    }
}

fn process_function_declaration(
    node: &Node<'_>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
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

    let signature = build_fn_signature(node, &name, source, "function");
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
        language: ctx.lang_tag.to_string(),
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

fn process_method_definition(
    node: &Node<'_>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
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

    let signature = build_fn_signature(node, &name, source, "method");
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
        language: ctx.lang_tag.to_string(),
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

fn process_class_declaration(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
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
        language: ctx.lang_tag.to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("class {name}")),
        file_mtime: ctx.file_mtime,
    });

    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // TypeScript: class heritage is in a `class_heritage` child node.
    // It can contain `extends_clause` and/or `implements_clause`.
    // JavaScript: class heritage is also a `class_heritage` child, but its
    // direct text is the base class expression (no nested extends_clause).
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "class_heritage" => {
                process_class_heritage(&child, id, source, ctx);
            }
            // JavaScript class declarations may expose `class_heritage` directly
            // or have no separate named node - handled above.
            _ => {}
        }
    }

    id
}

/// Process TypeScript `class_heritage` (contains `extends_clause` and/or `implements_clause`).
/// Also handles plain JavaScript `class_heritage` which is just `extends <expr>`.
fn process_class_heritage(
    heritage: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
) {
    let mut cursor = heritage.walk();
    for child in heritage.named_children(&mut cursor) {
        match child.kind() {
            "extends_clause" => {
                // TypeScript: extends_clause contains one or more _extends_clause_single nodes,
                // each with a `value` field.
                let mut ec = child.walk();
                for item in child.named_children(&mut ec) {
                    if let Some(val) = item.child_by_field_name("value") {
                        let base_name = node_text(&val, source);
                        if !base_name.is_empty() {
                            let (target_id, confidence) = resolve_name(&base_name, ctx);
                            ctx.result.relationships.push(Relationship {
                                source_id: class_id,
                                target_id,
                                rel_type: RelationType::Inherits,
                                confidence,
                            });
                        }
                    } else {
                        // Some tree-sitter versions expose the expression directly.
                        let base_name = node_text(&item, source);
                        if !base_name.is_empty() && item.kind() != "type_arguments" {
                            let (target_id, confidence) = resolve_name(&base_name, ctx);
                            ctx.result.relationships.push(Relationship {
                                source_id: class_id,
                                target_id,
                                rel_type: RelationType::Inherits,
                                confidence,
                            });
                        }
                    }
                }
            }
            "implements_clause" => {
                // TypeScript: implements_clause contains one or more type references.
                // Each type reference's first identifier is the interface name.
                let mut ic = child.walk();
                for type_node in child.named_children(&mut ic) {
                    // type_node is typically `type_identifier`, `generic_type`, or similar.
                    // For `implements Router<T>`, the outer node is `generic_type` with
                    // a `name` field pointing to `Router`.
                    let iface_name = extract_type_name(&type_node, source);
                    if !iface_name.is_empty() {
                        let (target_id, confidence) = resolve_name(&iface_name, ctx);
                        ctx.result.relationships.push(Relationship {
                            source_id: class_id,
                            target_id,
                            rel_type: RelationType::Inherits,
                            confidence,
                        });
                    }
                }
            }
            _ => {
                // JavaScript plain class_heritage: the node itself is the base expression.
                // e.g., kind = "identifier" or "member_expression".
                let base_name = node_text(heritage, source);
                // Strip leading "extends " keyword if present.
                let base_name = base_name
                    .strip_prefix("extends ")
                    .unwrap_or(&base_name)
                    .trim();
                if !base_name.is_empty() {
                    let (target_id, confidence) = resolve_name(base_name, ctx);
                    ctx.result.relationships.push(Relationship {
                        source_id: class_id,
                        target_id,
                        rel_type: RelationType::Inherits,
                        confidence,
                    });
                }
                // Only process once for JS heritage.
                break;
            }
        }
    }
}

/// Extract the primary type name from a type node.
///
/// Handles: `type_identifier` ("Router"), `generic_type` ("Router<T>" -> "Router"),
/// `nested_type_identifier` ("a.B" -> "B").
fn extract_type_name(node: &Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "type_identifier" | "identifier" => node_text(node, source),
        "generic_type" => {
            // `name` field holds the base type identifier.
            node.child_by_field_name("name")
                .map(|n| node_text(&n, source))
                .unwrap_or_default()
        }
        "nested_type_identifier" => {
            // `a.B` - take the right-hand identifier.
            node.child_by_field_name("member")
                .map(|n| node_text(&n, source))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn process_interface_declaration(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    // Use SymbolType::Class for interfaces per requirements.
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Class,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: ctx.lang_tag.to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("interface {name}")),
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

fn process_type_alias_declaration(
    node: &Node<'_>,
    parent_id: Uuid,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
) -> Uuid {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    // Treat type aliases as Class (same as interfaces) per test harness convention.
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type: SymbolType::Class,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: ctx.lang_tag.to_string(),
        project: ctx.project.to_string(),
        signature: Some(format!("type {name}")),
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

/// Handle `const foo = () => {}` and `const foo = function() {}`.
fn process_arrow_function_or_fn_expr(
    declarator: &Node<'_>,
    parent_id: Uuid,
    enclosing_class: Option<Uuid>,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
) {
    let Some(value) = declarator.child_by_field_name("value") else {
        return;
    };

    let is_function = matches!(
        value.kind(),
        "arrow_function" | "function_expression" | "generator_function_expression"
    );
    if !is_function {
        return;
    }

    let name = declarator
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "<anonymous>".to_string());

    let symbol_type = if enclosing_class.is_some() {
        SymbolType::Method
    } else {
        SymbolType::Function
    };

    let signature = build_fn_signature(&value, &name, source, "const");
    let start_line = declarator.start_position().row as i32 + 1;
    let end_line = declarator.end_position().row as i32 + 1;
    let id = Uuid::new_v4();

    ctx.name_to_id.insert(name.clone(), id);
    ctx.result.symbols.push(Symbol {
        id,
        name: name.clone(),
        symbol_type,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: ctx.lang_tag.to_string(),
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

    if let Some(body) = value.child_by_field_name("body") {
        let mut inner = body.walk();
        collect_definitions(&body, id, None, source, ctx, &mut inner);
    }
}

// ---------------------------------------------------------------------------
// Call collection
// ---------------------------------------------------------------------------

fn collect_calls<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut TsParseContext<'_>,
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

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut TsParseContext<'_>) {
    let Some(function_node) = node.child_by_field_name("function") else {
        return;
    };

    let (callee_name, callee_kind) = extract_callee_name(&function_node, source);
    if callee_name.is_empty() {
        return;
    }

    let caller_id = find_enclosing_function(node, ctx);
    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);

    // Score confidence based on what we know about the callee.
    //
    // Confidence rules:
    //   1.0 - method name matches a known symbol defined in this file
    //   0.8 - method name matches an imported name
    //   0.6 - this.xxx.method() pattern: instance method call, type not resolvable statically
    //   0.5 - unresolved call (bare or non-this member expression)
    let (target_id, confidence) = if ctx.name_to_id.contains_key(&callee_name) {
        resolve_name(&callee_name, ctx)
    } else if ctx.imported_names.contains(&callee_name) {
        resolve_name(&callee_name, ctx)
    } else if callee_kind == CalleeKind::ThisChain {
        (
            Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()),
            0.6_f32,
        )
    } else {
        resolve_name(&callee_name, ctx)
    };

    ctx.result.relationships.push(Relationship {
        source_id,
        target_id,
        rel_type: RelationType::Calls,
        confidence,
    });
}

/// Describes how a callee was referenced, used for confidence scoring.
#[derive(Debug, PartialEq)]
enum CalleeKind {
    /// Plain bare call: `foo()`
    Bare,
    /// `this.method()` or `this.service.method()` - instance method call via this
    ThisChain,
    /// Any other dotted call: `obj.method()`, `module.func()`, `a.b.c()`
    Member,
}

fn extract_callee_name(node: &Node<'_>, source: &[u8]) -> (String, CalleeKind) {
    match node.kind() {
        "identifier" => (node_text(node, source), CalleeKind::Bare),
        "member_expression" => {
            // `obj.method` - take the property (method name).
            let method = node
                .child_by_field_name("property")
                .map(|n| node_text(&n, source))
                .unwrap_or_default();
            let kind = if member_expression_starts_with_this(node, source) {
                CalleeKind::ThisChain
            } else {
                CalleeKind::Member
            };
            (method, kind)
        }
        _ => (String::new(), CalleeKind::Bare),
    }
}

/// Walk up a member_expression chain to determine if it starts with `this`.
///
/// For `this.service.method`, the tree looks like:
///   member_expression(object=member_expression(object=this, property="service"), property="method")
fn member_expression_starts_with_this(node: &Node<'_>, _source: &[u8]) -> bool {
    let mut current = node.clone();
    loop {
        match current.kind() {
            "member_expression" => {
                if let Some(obj) = current.child_by_field_name("object") {
                    current = obj;
                } else {
                    return false;
                }
            }
            "this" => return true,
            "identifier" => {
                // Not `this`, just a plain identifier at the root.
                return false;
            }
            _ => return false,
        }
    }
}

fn find_enclosing_function(call_node: &Node<'_>, ctx: &TsParseContext<'_>) -> Option<Uuid> {
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

fn resolve_name(name: &str, ctx: &TsParseContext<'_>) -> (Uuid, f32) {
    if let Some(&id) = ctx.name_to_id.get(name) {
        (id, 1.0)
    } else if ctx.imported_names.contains(name) {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()), 0.8)
    } else {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()), 0.5)
    }
}

// ---------------------------------------------------------------------------
// Signature building
// ---------------------------------------------------------------------------

fn build_fn_signature(node: &Node<'_>, name: &str, source: &[u8], keyword: &str) -> String {
    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    let return_type = node
        .child_by_field_name("return_type")
        .map(|n| format!(": {}", node_text(&n, source)));

    match keyword {
        "const" => format!(
            "const {name} = ({params_inner}){ret} => ...",
            name = name,
            params_inner = params.trim_matches(|c| c == '(' || c == ')'),
            ret = return_type.as_deref().unwrap_or(""),
        ),
        _ => format!(
            "{keyword} {name}{params}{ret}",
            keyword = keyword,
            name = name,
            params = params,
            ret = return_type.as_deref().unwrap_or(""),
        ),
    }
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

fn node_text(node: &Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").trim().to_string()
}

fn strip_quotes(s: &str) -> String {
    s.trim_matches(|c| c == '\'' || c == '"' || c == '`')
        .to_string()
}
