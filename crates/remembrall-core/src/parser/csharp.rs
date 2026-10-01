//! Tree-sitter based C# parser.
//!
//! Extracts symbols and relationships from a single C# source file.
//!
//! ## What is extracted
//!
//! Symbols:
//! - `class_declaration`          -> SymbolType::Class
//! - `struct_declaration`         -> SymbolType::Class
//! - `interface_declaration`      -> SymbolType::Class
//! - `enum_declaration`           -> SymbolType::Class
//! - `record_declaration`         -> SymbolType::Class (C# 9+ records)
//! - `record_struct_declaration`  -> SymbolType::Class
//! - `method_declaration` inside a type body -> SymbolType::Method
//! - `constructor_declaration`                     -> SymbolType::Method
//! - `property_declaration` inside a type body     -> SymbolType::Field
//! - `field_declaration` inside a type body       -> SymbolType::Field
//! - the file itself                               -> SymbolType::File
//!
//! Relationships:
//! - `using_directive`           -> RelationType::Imports
//! - `invocation_expression`     -> RelationType::Calls
//! - `base_list` (base class + interfaces) -> RelationType::Inherits
//! - enclosing scope -> symbol    -> RelationType::Defines
//! - parameter/return/property/field types -> RelationType::UsesType
//! - `this.<member>` reads       -> RelationType::References
//!
//! Namespaces are not symbols (they are containers); we descend into their
//! bodies to find nested type declarations.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use tree_sitter::{Node, Parser, TreeCursor};
use uuid::Uuid;

use crate::graph::types::{RelationType, Relationship, Symbol, SymbolType};
use crate::parser::python::{FileParseResult, RawImport};

/// Parse a C# file and extract symbols and relationships.
///
/// - `file_path`  - canonical path string stored on each symbol
/// - `source`     - raw UTF-8 source text
/// - `project`    - project name tag
/// - `file_mtime` - filesystem mtime; stored on symbols for incremental indexing
pub fn parse_csharp_file(
    file_path: &str,
    source: &str,
    project: &str,
    file_mtime: DateTime<Utc>,
) -> FileParseResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .expect("failed to load C# grammar");

    let Some(tree) = parser.parse(source, None) else {
        tracing::warn!("tree-sitter failed to parse {file_path}");
        return FileParseResult { had_errors: true, ..FileParseResult::default() };
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
        class_fields: HashMap::new(),
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
        language: "csharp".to_string(),
        project: project.to_string(),
        signature: None,
        file_mtime,
        layer: None,
        parent_symbol_id: None,
        moniker: None,
    });

    // First pass: collect using directives.
    let mut cursor = root.walk();
    collect_usings(&root, source_bytes, &mut ctx, &mut cursor);

    // Second pass: collect type declarations and their members.
    let mut cursor2 = root.walk();
    collect_definitions(
        &root,
        file_symbol_id,
        None,
        source_bytes,
        &mut ctx,
        &mut cursor2,
    );

    // Third pass: collect invocation expressions (calls) and object creations.
    let mut cursor3 = root.walk();
    collect_calls(&root, source_bytes, &mut ctx, &mut cursor3);

    ctx.result.had_errors = root.has_error();
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
    /// Simple names brought into scope by `using` (only static- and alias-imports
    /// introduce a usable type name; plain namespace usings do not).
    imported_names: HashSet<String>,
    /// (class_id, member_name) -> field/property symbol UUID for members defined
    /// in this file. Used to resolve `this.<member>` reads to the enclosing
    /// class's field or property.
    class_fields: HashMap<(Uuid, String), Uuid>,
}

// ---------------------------------------------------------------------------
// Using collection
// ---------------------------------------------------------------------------

fn collect_usings<'a>(
    node: &Node<'a>,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
    cursor: &mut TreeCursor<'a>,
) {
    for child in node.children(cursor) {
        if child.kind() == "using_directive" {
            process_using(&child, source, ctx);
        }
    }
}

fn process_using(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // Three forms:
    //   using System.IO;                  // namespace using - no usable type name
    //   using static System.Math;         // static using  -> simple name "Math"
    //   using Foo = System.IO.Bar;        // alias using  -> simple name "Foo"
    //
    // We read the raw text and strip the leading `using` keyword and trailing `;`.
    let raw_text = node_text(node, source);

    let stripped = raw_text
        .trim_start_matches("using")
        .trim()
        .trim_end_matches(';')
        .trim();

    if stripped.is_empty() {
        return;
    }

    let is_static = stripped.starts_with("static ");
    let body = if is_static {
        stripped.trim_start_matches("static").trim()
    } else {
        stripped
    };

    let (module_path, simple_name) = if let Some(eq) = body.find('=') {
        // Alias: `Alias = Target.Path`
        let alias = body[..eq].trim();
        let target = body[eq + 1..].trim();
        (target.to_string(), Some(alias.to_string()))
    } else {
        // Namespace or static using: last component is only a usable name for
        // static usings (which name a type), not for namespace usings.
        let last = body.split('.').last().unwrap_or(body).trim();
        let name = if is_static {
            Some(last.to_string())
        } else {
            None
        };
        (body.to_string(), name)
    };

    if let Some(name) = simple_name {
        if !name.is_empty() {
            ctx.imported_names.insert(name);
        }
    }

    let file_id = ctx.result.symbols[0].id;

    // C# usings are always absolute namespace/type paths (no relative imports).
    // Store as a raw import so the walker can attempt suffix matching against
    // known file paths (rarely resolves for C#, but kept for parity).
    ctx.result.raw_imports.push(RawImport {
        source_id: file_id,
        module_raw: module_path.clone(),
        is_relative: false,
        dot_count: 0,
        module_path: module_path.clone(),
    });

    // Emit placeholder relationship; walker will rewrite if resolved.
    let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, module_path.as_bytes());
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
            | "struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "record_struct_declaration"
    )
}

/// Recursively walk the AST collecting type declarations and their members.
///
/// - `parent_id`        - UUID of the enclosing scope (file or outer class)
/// - `enclosing_class`  - Some(class_id) when inside a type body
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
            // Recurse into the body to capture nested types and members.
            if let Some(body) = child.child_by_field_name("body") {
                let mut inner = body.walk();
                collect_definitions(&body, class_id, Some(class_id), source, ctx, &mut inner);
            }
        } else if kind == "namespace_declaration" {
            // Namespaces are containers, not symbols. Descend into their body
            // to find nested types without changing the enclosing class.
            if let Some(body) = child.child_by_field_name("body") {
                let mut inner = body.walk();
                collect_definitions(&body, parent_id, enclosing_class, source, ctx, &mut inner);
            } else {
                let mut inner = child.walk();
                collect_definitions(&child, parent_id, enclosing_class, source, ctx, &mut inner);
            }
        } else if kind == "method_declaration" || kind == "constructor_declaration" {
            if let Some(class_id) = enclosing_class {
                process_method(&child, class_id, source, ctx);
            }
        } else if kind == "property_declaration" {
            if let Some(class_id) = enclosing_class {
                process_property(&child, class_id, source, ctx);
            }
        } else if kind == "field_declaration" {
            if let Some(class_id) = enclosing_class {
                process_field(&child, class_id, source, ctx);
            }
        } else {
            // Descend to catch nested types in declaration_list / block /
            // namespace bodies. We do not recurse into method bodies here -
            // local functions and nested types inside methods are rare and
            // would risk miscounting; calls inside method bodies are handled
            // in the third pass.
            if kind == "declaration_list" || kind == "block" {
                let mut inner = child.walk();
                collect_definitions(&child, parent_id, enclosing_class, source, ctx, &mut inner);
            }
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
        language: "csharp".to_string(),
        project: ctx.project.to_string(),
        signature: Some(build_type_signature(node, &name, source)),
        file_mtime: ctx.file_mtime,
        layer: None,
        parent_symbol_id: None,
        moniker: None,
    });

    // DEFINES: parent scope (file or outer class) defines this type.
    ctx.result.relationships.push(Relationship {
        source_id: parent_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // INHERITS from the base_list (C# puts base class and interfaces in one
    // `: Base, I1, I2` clause).
    extract_base_list(node, id, source, ctx);

    id
}

/// Emit Inherits relationships for every type in the `base_list` child.
///
/// The C# grammar places the base class and implemented interfaces in one
/// `: Base, I1, I2` clause. The `base_list` node's named children are the
/// type expressions directly (e.g. `identifier`, `qualified_name`,
/// `generic_name`, or `primary_constructor_base_type`), so we extract type
/// identifiers from each named child rather than expecting a `type` wrapper.
fn extract_base_list(node: &Node<'_>, class_id: Uuid, source: &[u8], ctx: &mut ParseContext<'_>) {
    // `base_list` is a named child of type declarations but is NOT exposed via
    // a field name, so locate it by kind.
    let mut cursor = node.walk();
    let base_list = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "base_list");
    let Some(base_list) = base_list else {
        return;
    };

    let mut type_names: Vec<String> = Vec::new();
    let mut cursor2 = base_list.walk();
    for child in base_list.named_children(&mut cursor2) {
        extract_type_identifiers(&child, source, &mut type_names);
    }

    for base_name in type_names {
        if base_name.is_empty() {
            continue;
        }
        let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&base_name) {
            (id, 1.0_f32)
        } else if ctx.imported_names.contains(&base_name) {
            (Uuid::new_v5(&Uuid::NAMESPACE_OID, base_name.as_bytes()), 0.8)
        } else {
            (Uuid::new_v5(&Uuid::NAMESPACE_OID, base_name.as_bytes()), 0.5)
        };

        ctx.result.relationships.push(Relationship {
            source_id: class_id,
            target_id,
            rel_type: RelationType::Inherits,
            confidence,
        });
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
        language: "csharp".to_string(),
        project: ctx.project.to_string(),
        signature: Some(build_method_signature(node, &name, source)),
        file_mtime: ctx.file_mtime,
        layer: None,
        parent_symbol_id: None,
        moniker: None,
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

/// Capture a C# property as a `Field` symbol with a `Defines` edge from the
/// enclosing class. Properties are idiomatic C# members and are treated as
/// typed fields for impact analysis. Also emits a UsesType edge for the
/// property's declared type.
fn process_property(
    node: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = node_text(&name_node, source);
    if name.is_empty() {
        return;
    }
    let start_line = node.start_position().row as i32 + 1;
    let end_line = node.end_position().row as i32 + 1;
    let id = Uuid::new_v4();
    ctx.class_fields.insert((class_id, name.clone()), id);

    ctx.result.symbols.push(Symbol {
        id,
        name,
        symbol_type: SymbolType::Field,
        file_path: ctx.file_path.to_string(),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: "csharp".to_string(),
        project: ctx.project.to_string(),
        signature: Some(build_property_signature(node, source)),
        file_mtime: ctx.file_mtime,
        layer: None,
        parent_symbol_id: Some(class_id),
        moniker: None,
    });

    ctx.result.relationships.push(Relationship {
        source_id: class_id,
        target_id: id,
        rel_type: RelationType::Defines,
        confidence: 1.0,
    });

    // UsesType for the property type.
    if let Some(type_node) = node.child_by_field_name("type") {
        emit_uses_type(&type_node, id, source, ctx);
    }
}

/// Capture each variable declarator of a class-body `field_declaration` as a
/// `Field` symbol with a `Defines` edge from the enclosing class. Handles
/// multi-declarator fields (`int amount, total;`) and emits a UsesType edge
/// for the declared type.
fn process_field(
    node: &Node<'_>,
    class_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    // field_declaration -> variable_declaration -> {type, variable_declarator*}
    let mut cursor = node.walk();
    for var_decl in node.named_children(&mut cursor) {
        if var_decl.kind() != "variable_declaration" {
            continue;
        }
        let type_node = var_decl.child_by_field_name("type");

        let mut inner = var_decl.walk();
        for child in var_decl.named_children(&mut inner) {
            if child.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let name = node_text(&name_node, source);
            if name.is_empty() {
                continue;
            }
            let start_line = child.start_position().row as i32 + 1;
            let end_line = child.end_position().row as i32 + 1;
            let id = Uuid::new_v4();
            ctx.class_fields.insert((class_id, name.clone()), id);

            ctx.result.symbols.push(Symbol {
                id,
                name,
                symbol_type: SymbolType::Field,
                file_path: ctx.file_path.to_string(),
                start_line: Some(start_line),
                end_line: Some(end_line),
                language: "csharp".to_string(),
                project: ctx.project.to_string(),
                signature: None,
                file_mtime: ctx.file_mtime,
                layer: None,
                parent_symbol_id: Some(class_id),
                moniker: None,
            });

            ctx.result.relationships.push(Relationship {
                source_id: class_id,
                target_id: id,
                rel_type: RelationType::Defines,
                confidence: 1.0,
            });

            // UsesType for the field's declared type.
            if let Some(tn) = type_node {
                emit_uses_type(&tn, id, source, ctx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type annotation extraction
// ---------------------------------------------------------------------------

/// Walk a method/constructor node's parameter list and return type,
/// collecting `UsesType` relationships for every non-builtin type name found.
fn collect_type_annotations(
    method_node: &Node<'_>,
    method_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    // 1. Parameter types.
    if let Some(params) = method_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for param in params.named_children(&mut cursor) {
            // parameter, fixed_parameter, etc. Each has a `type` field.
            if let Some(type_node) = param.child_by_field_name("type") {
                emit_uses_type(&type_node, method_id, source, ctx);
            }
        }
    }

    // 2. Return type. The C# grammar names this field `returns` on
    //    method_declaration; some grammar versions use `type`. Try both.
    let return_type = method_node
        .child_by_field_name("type")
        .or_else(|| method_node.child_by_field_name("returns"));
    if let Some(return_type) = return_type {
        emit_uses_type(&return_type, method_id, source, ctx);
    }
}

/// Extract type identifiers from `type_node` and emit a UsesType relationship
/// for each non-builtin name, scored by same-file/imported/unknown confidence.
fn emit_uses_type(
    type_node: &Node<'_>,
    source_id: Uuid,
    source: &[u8],
    ctx: &mut ParseContext<'_>,
) {
    let mut type_names: Vec<String> = Vec::new();
    extract_type_identifiers(type_node, source, &mut type_names);

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
            source_id,
            target_id,
            rel_type: RelationType::UsesType,
            confidence,
        });
    }
}

/// Recursively extract all type identifier names from a type node.
///
/// Node kinds handled:
/// - `identifier`            -> push the name directly (e.g. `Order`)
/// - `qualified_name`         -> use only the rightmost component
///                               (e.g. `System.IO.Order` -> `Order`)
/// - `generic_name`           -> push the base name, then recurse into the
///                               type_argument_list (e.g. `List<Order>` -> `Order`)
/// - `array_type`             -> recurse into the element type
/// - `nullable_type`           -> recurse into the underlying type
/// - `pointer_type`            -> recurse into the pointee type
/// - `predefined_type`         -> push the keyword (filtered as builtin)
/// - `tuple_type`              -> recurse into element types
/// - everything else            -> recurse into named children
fn extract_type_identifiers(node: &Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "identifier" => {
            let name = node_text(node, source);
            if !name.is_empty() {
                out.push(name);
            }
        }
        "qualified_name" => {
            // `name` field is the rightmost component.
            if let Some(n) = node.child_by_field_name("name") {
                extract_type_identifiers(&n, source, out);
            } else {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    extract_type_identifiers(&child, source, out);
                }
            }
        }
        "generic_name" => {
            // The base name is the `name` field (an identifier).
            if let Some(n) = node.child_by_field_name("name") {
                let name = node_text(&n, source);
                if !name.is_empty() {
                    out.push(name);
                }
            }
            // Recurse into type arguments so we capture `Order` in `List<Order>`.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "type_argument_list" {
                    extract_type_identifiers(&child, source, out);
                }
            }
        }
        "array_type" => {
            if let Some(elem) = node.child_by_field_name("type") {
                extract_type_identifiers(&elem, source, out);
            } else {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    extract_type_identifiers(&child, source, out);
                }
            }
        }
        "nullable_type" | "pointer_type" => {
            if let Some(inner) = node.child_by_field_name("type") {
                extract_type_identifiers(&inner, source, out);
            } else {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    extract_type_identifiers(&child, source, out);
                }
            }
        }
        "predefined_type" => {
            let name = node_text(node, source);
            if !name.is_empty() {
                out.push(name);
            }
        }
        "type_argument_list" | "tuple_type" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_type_identifiers(&child, source, out);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                extract_type_identifiers(&child, source, out);
            }
        }
    }
}

/// Returns true for C# predefined types, their common boxed/BCL equivalents,
/// and the most common standard library types that are not project-specific.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        // Predefined / primitive keywords
        "bool" | "byte" | "sbyte" | "char" | "decimal" | "double" | "float"
        | "int" | "uint" | "long" | "ulong" | "short" | "ushort"
        | "object" | "string" | "void" | "var" | "dynamic" | "nint" | "nuint"
        // Common BCL types
        | "Task" | "ValueTask" | "List" | "Dictionary" | "HashSet" | "SortedSet"
        | "SortedDictionary" | "SortedList" | "Queue" | "Stack" | "LinkedList"
        | "Array" | "Enumerable" | "IEnumerable" | "ICollection" | "IList"
        | "IDictionary" | "IReadOnlyList" | "IReadOnlyCollection"
        | "IReadOnlyDictionary" | "ISet" | "IEnumerator"
        | "Action" | "Func" | "Predicate" | "Tuple" | "ValueTuple"
        | "ConcurrentDictionary" | "ConcurrentQueue" | "ConcurrentBag"
        | "Span" | "ReadOnlySpan" | "Memory" | "ReadOnlyMemory"
        | "Nullable" | "AsyncLazy" | "StringBuilder" | "DateTime" | "DateTimeOffset"
        | "TimeSpan" | "Guid" | "Uri" | "Version" | "Math" | "Convert"
        | "Stream" | "CancellationToken" | "CancellationTokenSource"
        // Exception / interface hierarchy
        | "Exception" | "SystemException" | "InvalidOperationException"
        | "ArgumentException" | "ArgumentNullException" | "NotImplementedException"
        | "IDisposable" | "IAsyncDisposable" | "IComparable" | "IEquatable"
        | "ICloneable" | "IFormattable"
    )
}

// ---------------------------------------------------------------------------
// Signature builders
// ---------------------------------------------------------------------------

/// Build a human-readable signature for a class/struct/interface/enum/record.
///
/// Examples:
///   `class UserService : BaseService, IAuditable`
///   `interface PaymentGateway`
///   `struct Point`
///   `enum Status`
fn build_type_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    let keyword = match node.kind() {
        "struct_declaration" => "struct",
        "interface_declaration" => "interface",
        "enum_declaration" => "enum",
        "record_struct_declaration" => "record struct",
        "record_declaration" => "record",
        _ => "class",
    };

    let mut sig = format!("{keyword} {name}");

    if let Some(base_node) = node.child_by_field_name("base_list") {
        let base_text = node_text(&base_node, source);
        // Strip the leading ":" that tree-sitter includes in the base_list text.
        let cleaned = base_text.trim_start_matches(':').trim();
        if !cleaned.is_empty() {
            sig.push_str(&format!(" : {cleaned}"));
        }
    }

    sig
}

/// Build a human-readable signature for a method or constructor.
///
/// Examples:
///   `public void ProcessOrder(Order order, User user)`
///   `UserService(Repository repo)`
fn build_method_signature(node: &Node<'_>, name: &str, source: &[u8]) -> String {
    // Modifiers (public, static, async, etc.) appear as named children of kind
    // `modifier`; collect their text.
    let modifiers = collect_modifiers(node, source);

    // Return type (absent for constructors). Try both field names.
    let return_type = node
        .child_by_field_name("type")
        .or_else(|| node.child_by_field_name("returns"))
        .map(|n| format!("{} ", node_text(&n, source)))
        .unwrap_or_default();

    let params = node
        .child_by_field_name("parameters")
        .map(|n| node_text(&n, source))
        .unwrap_or_else(|| "()".to_string());

    format!("{modifiers}{return_type}{name}{params}")
        .trim()
        .to_string()
}

/// Build a human-readable signature for a property, e.g. `public int Amount { get; set; }`.
fn build_property_signature(node: &Node<'_>, source: &[u8]) -> String {
    let modifiers = collect_modifiers(node, source);
    let type_text = node
        .child_by_field_name("type")
        .map(|n| node_text(&n, source))
        .unwrap_or_default();
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(&n, source))
        .unwrap_or_default();
    format!("{modifiers}{type_text} {name} {{ get; set; }}")
        .trim()
        .to_string()
}

/// Collect leading `modifier` children (public, private, static, async, ...)
/// into a single trailing-space string.
fn collect_modifiers(node: &Node<'_>, source: &[u8]) -> String {
    let mut cursor = node.walk();
    let mut out = String::new();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifier" {
            let m = node_text(&child, source);
            if !m.is_empty() {
                out.push_str(&m);
                out.push(' ');
            }
        }
    }
    out
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
        if child.kind() == "invocation_expression" {
            process_call(&child, source, ctx);
        } else if child.kind() == "object_creation_expression" {
            process_object_creation(&child, source, ctx);
        } else if child.kind() == "member_access_expression" {
            process_member_read(&child, source, ctx);
        }
        let mut inner = child.walk();
        collect_calls(&child, source, ctx, &mut inner);
    }
}

fn process_call(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    // tree-sitter-c-sharp invocation_expression fields:
    //   function   - the callee expression (identifier, member_access_expression, ...)
    //   arguments  - argument list
    let Some(func_node) = node.child_by_field_name("function") else {
        return;
    };

    let (callee_name, is_this_call, _has_receiver) = match func_node.kind() {
        "identifier" => (node_text(&func_node, source), false, false),
        "member_access_expression" => {
            let name = func_node
                .child_by_field_name("name")
                .map(|n| node_text(&n, source))
                .unwrap_or_default();
            let receiver = func_node
                .child_by_field_name("expression")
                .map(|r| node_text(&r, source))
                .unwrap_or_default();
            let is_this = receiver == "this" || receiver == "base";
            (name, is_this, true)
        }
        "generic_name" => (
            func_node
                .child_by_field_name("name")
                .map(|n| node_text(&n, source))
                .unwrap_or_default(),
            false,
            false,
        ),
        _ => (String::new(), false, false),
    };

    if callee_name.is_empty() {
        return;
    }

    let caller_id = find_enclosing_method(node, ctx);

    let (target_id, confidence) = if let Some(&id) = ctx.name_to_id.get(&callee_name) {
        (id, 1.0_f32)
    } else if ctx.imported_names.contains(&callee_name) {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()), 0.8)
    } else if is_this_call {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()), 0.6)
    } else {
        (Uuid::new_v5(&Uuid::NAMESPACE_OID, callee_name.as_bytes()), 0.5)
    };

    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);

    ctx.result.relationships.push(Relationship {
        source_id,
        target_id,
        rel_type: RelationType::Calls,
        confidence,
    });
}

/// Emit a UsesType edge for a `new Foo(...)` expression, sourced from the
/// enclosing method. This captures constructor-style references to domain
/// types that are not visible in method signatures.
fn process_object_creation(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let caller_id = find_enclosing_method(node, ctx);
    let source_id = caller_id.unwrap_or(ctx.result.symbols[0].id);
    emit_uses_type(&type_node, source_id, source, ctx);
}

/// Emit a `References` edge for a `this.<member>` read inside a method.
///
/// Only `member_access_expression` nodes whose object is `this`/`base` and
/// which are NOT the `function` of an enclosing invocation (those are handled
/// as Calls). Writes (`this.x = ...`) are skipped. Resolves only when the
/// member names a known field/property of the enclosing class.
fn process_member_read(node: &Node<'_>, source: &[u8], ctx: &mut ParseContext<'_>) {
    let Some(obj) = node.child_by_field_name("expression") else {
        return;
    };
    let obj_text = node_text(&obj, source);
    if obj_text != "this" && obj_text != "base" {
        return;
    }

    // Skip if this member_access is the function of an invocation_expression
    // (i.e. `this.Foo()` is a call, not a field read).
    if let Some(parent) = node.parent() {
        if parent.kind() == "invocation_expression"
            && parent.child_by_field_name("function").map(|f| f.id()) == Some(node.id())
        {
            return;
        }
    }

    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let member_name = node_text(&name_node, source);
    if member_name.is_empty() {
        return;
    }

    // Skip writes: `this.x = ...`.
    if let Some(parent) = node.parent() {
        if parent.kind() == "assignment_expression"
            && parent.child_by_field_name("left").map(|l| l.id()) == Some(node.id())
        {
            return;
        }
    }

    let Some(class_id) = find_enclosing_class(node, ctx) else {
        return;
    };
    let Some(&field_id) = ctx.class_fields.get(&(class_id, member_name.clone())) else {
        return;
    };

    let source_id = find_enclosing_method(node, ctx).unwrap_or_else(|| ctx.result.symbols[0].id);

    ctx.result.relationships.push(Relationship {
        source_id,
        target_id: field_id,
        rel_type: RelationType::References,
        confidence: 1.0,
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

/// Find the UUID of the innermost class symbol that contains `node`.
fn find_enclosing_class(node: &Node<'_>, ctx: &ParseContext<'_>) -> Option<Uuid> {
    let target = node.start_position().row as i32 + 1;
    let mut best: Option<(Uuid, i32)> = None;

    for sym in &ctx.result.symbols {
        if sym.symbol_type != SymbolType::Class || sym.file_path != ctx.file_path {
            continue;
        }
        let (Some(s), Some(e)) = (sym.start_line, sym.end_line) else {
            continue;
        };
        if target >= s && target <= e {
            let range = e - s;
            if range < best.map(|(_, r)| r).unwrap_or(i32::MAX) {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> FileParseResult {
        let now = chrono::Utc::now();
        parse_csharp_file("Test.cs", src, "test", now)
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

    fn field_id(result: &FileParseResult, name: &str) -> Option<uuid::Uuid> {
        result
            .symbols
            .iter()
            .find(|s| s.symbol_type == SymbolType::Field && s.name == name)
            .map(|s| s.id)
    }

    fn class_id(result: &FileParseResult, name: &str) -> Option<uuid::Uuid> {
        result
            .symbols
            .iter()
            .find(|s| s.symbol_type == SymbolType::Class && s.name == name)
            .map(|s| s.id)
    }

    #[test]
    fn test_extracts_class_and_method() {
        let src = r#"
class OrderService {
    public void ProcessOrder(Order order) { }
}
"#;
        let result = parse(src);
        assert!(class_id(&result, "OrderService").is_some());
        assert!(method_id(&result, "ProcessOrder").is_some());
    }

    #[test]
    fn test_extracts_struct_interface_enum_record() {
        let src = r#"
struct Point { public int X; }
interface IRepository { User Find(long id); }
enum Status { Active, Closed }
record Customer(string Name);
"#;
        let result = parse(src);
        assert!(class_id(&result, "Point").is_some(), "struct -> class symbol");
        assert!(class_id(&result, "IRepository").is_some(), "interface -> class");
        assert!(class_id(&result, "Status").is_some(), "enum -> class");
        assert!(class_id(&result, "Customer").is_some(), "record -> class");
    }

    #[test]
    fn test_uses_type_param_annotation() {
        let src = r#"
class OrderService {
    public void ProcessOrder(Order order) { }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        assert!(!rels.is_empty(), "expected at least one UsesType relationship");
        let proc_id = method_id(&result, "ProcessOrder").expect("ProcessOrder not found");
        assert!(
            rels.iter().any(|(src_id, _, _)| *src_id == proc_id),
            "UsesType should originate from ProcessOrder"
        );
    }

    #[test]
    fn test_uses_type_return_annotation() {
        let src = r#"
class UserRepository {
    public User FindById(long id) { return null; }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let find_id = method_id(&result, "FindById").expect("FindById not found");
        assert!(
            rels.iter().any(|(s, _, _)| *s == find_id),
            "UsesType should originate from FindById for User return type"
        );
    }

    #[test]
    fn test_no_uses_type_for_builtins() {
        let src = r#"
class Calculator {
    public int Add(int a, int b) { return a + b; }
    public string Format(double value) { return ""; }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        assert!(
            rels.is_empty(),
            "primitive and string types should not produce UsesType, got: {rels:?}"
        );
    }

    #[test]
    fn test_uses_type_generic_type_param() {
        let src = r#"
class CartService {
    public List<Product> GetItems(Cart cart) { return null; }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let get_id = method_id(&result, "GetItems").expect("GetItems not found");
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
class Payment { }
class PaymentService {
    public void Process(Payment p) { }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let process_id = method_id(&result, "Process").expect("Process not found");
        let rel = rels
            .iter()
            .find(|(s, _, _)| *s == process_id)
            .expect("no UsesType from Process");
        assert_eq!(rel.2, 1.0, "same-file type should have confidence 1.0");
    }

    #[test]
    fn test_uses_type_confidence_imported() {
        let src = r#"
using static Models.Order;
class ShipmentService {
    public void Ship(Order o) { }
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        let ship_id = method_id(&result, "Ship").expect("Ship not found");
        let rel = rels
            .iter()
            .find(|(s, _, _)| *s == ship_id)
            .expect("no UsesType from Ship");
        assert_eq!(
            rel.2, 0.8,
            "imported type should have confidence 0.8"
        );
    }

    #[test]
    fn test_property_and_field_symbols() {
        let src = r#"
class Account {
    public int Balance { get; set; }
    private string owner;
}
"#;
        let result = parse(src);
        assert!(field_id(&result, "Balance").is_some(), "property -> field symbol");
        assert!(field_id(&result, "owner").is_some(), "field -> field symbol");
    }

    #[test]
    fn test_uses_type_for_property_and_field() {
        let src = r#"
class Invoice {
    public Customer Buyer { get; set; }
    private Address address;
}
"#;
        let result = parse(src);
        let rels = uses_type_rels(&result);
        assert!(
            rels.len() >= 2,
            "expected UsesType for both property Buyer and field address, got {rels:?}"
        );
    }

    #[test]
    fn test_inherits_base_and_interface() {
        let src = r#"
class PaymentGateway : BaseGateway, IPayment {
    public void Charge() { }
}
"#;
        let result = parse(src);
        let pg_id = class_id(&result, "PaymentGateway").expect("PaymentGateway not found");
        let inherits: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::Inherits && r.source_id == pg_id)
            .collect();
        assert!(
            inherits.len() >= 2,
            "expected Inherits for BaseGateway and IPayment, got {inherits:?}"
        );
    }

    #[test]
    fn test_inherits_same_file_confidence() {
        let src = r#"
class Base { }
class Derived : Base { }
"#;
        let result = parse(src);
        let derived_id = class_id(&result, "Derived").expect("Derived not found");
        let rel = result
            .relationships
            .iter()
            .find(|r| r.rel_type == RelationType::Inherits && r.source_id == derived_id)
            .expect("no Inherits from Derived");
        assert_eq!(rel.confidence, 1.0, "same-file base should be confidence 1.0");
    }

    #[test]
    fn test_calls_recorded() {
        let src = r#"
class Service {
    public void Run() {
        Logger.Log("hi");
    }
}
"#;
        let result = parse(src);
        let run_id = method_id(&result, "Run").expect("Run not found");
        let calls: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::Calls && r.source_id == run_id)
            .collect();
        assert!(!calls.is_empty(), "expected a Calls edge from Run to Log");
    }

    #[test]
    fn test_this_call_higher_confidence() {
        let src = r#"
class Calculator {
    public int Compute(int x) { return this.Helper(x); }
    private int Helper(int x) { return x; }
}
"#;
        let result = parse(src);
        let compute_id = method_id(&result, "Compute").expect("Compute not found");
        // `this.Helper(x)` - Helper is defined in this file, so confidence is 1.0.
        let rel = result
            .relationships
            .iter()
            .find(|r| r.rel_type == RelationType::Calls && r.source_id == compute_id)
            .expect("no call from Compute");
        assert_eq!(rel.confidence, 1.0);
    }

    #[test]
    fn test_object_creation_emits_uses_type() {
        let src = r#"
class Factory {
    public Order Build() {
        return new Order();
    }
}
"#;
        let result = parse(src);
        let build_id = method_id(&result, "Build").expect("Build not found");
        let rels = uses_type_rels(&result);
        assert!(
            rels.iter().any(|(s, _, _)| *s == build_id),
            "new Order() should emit UsesType from Build, got {rels:?}"
        );
    }

    #[test]
    fn test_this_property_read_references() {
        let src = r#"
class Counter {
    private int count;
    public int Peek() {
        return this.count;
    }
}
"#;
        let result = parse(src);
        let count_id = field_id(&result, "count").expect("count field not found");
        let refs: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::References && r.target_id == count_id)
            .collect();
        assert!(
            !refs.is_empty(),
            "this.count read should emit a References edge to the count field"
        );
    }

    #[test]
    fn test_this_property_write_not_counted() {
        let src = r#"
class Counter {
    private int count;
    public void Bump() {
        this.count = this.count + 1;
    }
}
"#;
        let result = parse(src);
        let count_id = field_id(&result, "count").expect("count field not found");
        let refs: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::References && r.target_id == count_id)
            .collect();
        // The read `this.count + 1` should produce exactly one References edge;
        // the write `this.count = ...` must NOT be counted.
        assert_eq!(
            refs.len(),
            1,
            "expected exactly one References for the read, got {refs:?}"
        );
    }

    #[test]
    fn test_using_directive_records_import() {
        let src = r#"
using System.Collections.Generic;
using static System.Math;
using Foo = System.IO.File;
class App { }
"#;
        let result = parse(src);
        let imports: Vec<_> = result
            .relationships
            .iter()
            .filter(|r| r.rel_type == RelationType::Imports)
            .collect();
        assert_eq!(imports.len(), 3, "expected one import edge per using directive");
    }

    #[test]
    fn test_namespaces_are_descended_not_symbols() {
        let src = r#"
namespace MyApp.Services {
    class UserService { }
}
"#;
        let result = parse(src);
        // No "MyApp" or "Services" class symbol; UserService is found inside.
        assert!(class_id(&result, "MyApp").is_none());
        assert!(class_id(&result, "Services").is_none());
        assert!(class_id(&result, "UserService").is_some());
    }
}