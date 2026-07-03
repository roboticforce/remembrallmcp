use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A code symbol (function, class, method, field, file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: Uuid,
    pub name: String,
    pub symbol_type: SymbolType,
    pub file_path: String,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub language: String,
    pub project: String,
    pub signature: Option<String>,
    pub file_mtime: DateTime<Utc>,
    pub layer: Option<String>,
    /// For field/property symbols, the enclosing class/struct symbol. None for
    /// file/function/class/method symbols. Parent linkage is also expressible via a
    /// `Defines` relationship (parent -> child); this column makes "member of" queryable
    /// without an edge join and disambiguates same-named fields across classes.
    pub parent_symbol_id: Option<Uuid>,
    /// Stable, position-independent, scheme-qualified identifier (SCIP-style descriptor,
    /// e.g. `pkg mod Class#field.`). Populated in Phase 3 as the cross-pass join key for
    /// the DSL string-literal resolver and the dedup key across reindex. None in Phase 1.
    pub moniker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SymbolType {
    File,
    Function,
    Class,
    Method,
    /// A data field/attribute/property of a class or struct (e.g. Python class attribute,
    /// Rust struct field, Go struct field, TS class field). Scoped under a parent class via
    /// `Symbol::parent_symbol_id`. Distinct from `Method` (callable members).
    Field,
}

impl std::fmt::Display for SymbolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::Function => write!(f, "function"),
            Self::Class => write!(f, "class"),
            Self::Method => write!(f, "method"),
            Self::Field => write!(f, "field"),
        }
    }
}

impl std::str::FromStr for SymbolType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file" => Ok(Self::File),
            "function" => Ok(Self::Function),
            "class" => Ok(Self::Class),
            "method" => Ok(Self::Method),
            "field" => Ok(Self::Field),
            _ => Err(format!("Unknown symbol type: {s}")),
        }
    }
}

/// A relationship between two symbols.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub source_id: Uuid,
    pub target_id: Uuid,
    pub rel_type: RelationType,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RelationType {
    Calls,
    Imports,
    Defines,
    Inherits,
    UsesType,
    /// A source symbol references (reads) a target field/property. The source is the
    /// enclosing function/method (or file/class when at class-body scope); the target is a
    /// `SymbolType::Field`. Distinct from `Calls` (invoking a callable) and `UsesType`
    /// (referencing a type). Flows into impact analysis like any relationship.
    References,
}

impl std::fmt::Display for RelationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Calls => write!(f, "calls"),
            Self::Imports => write!(f, "imports"),
            Self::Defines => write!(f, "defines"),
            Self::Inherits => write!(f, "inherits"),
            Self::UsesType => write!(f, "uses_type"),
            Self::References => write!(f, "references"),
        }
    }
}

impl std::str::FromStr for RelationType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "calls" => Ok(Self::Calls),
            "imports" => Ok(Self::Imports),
            "defines" => Ok(Self::Defines),
            "inherits" => Ok(Self::Inherits),
            "uses_type" => Ok(Self::UsesType),
            "references" => Ok(Self::References),
            _ => Err(format!("Unknown relation type: {s}")),
        }
    }
}

/// Result of an impact analysis query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactResult {
    pub symbol: Symbol,
    pub depth: i32,
    pub path: Vec<Uuid>,
    pub relationship: RelationType,
    pub confidence: f32,
}

/// Direction for graph traversal.
#[derive(Debug, Clone, Copy)]
pub enum Direction {
    /// Find things that depend on the target (who calls me?)
    Upstream,
    /// Find things the target depends on (what do I call?)
    Downstream,
    /// Both directions
    Both,
}

/// One stop in a guided codebase tour.
///
/// Files are ordered so that dependencies always appear before the files that
/// depend on them ("read this first, then this").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TourStop {
    /// 1-indexed position in the recommended reading order.
    pub order: usize,
    /// Relative or absolute file path as stored in the graph.
    pub file_path: String,
    /// Language detected during indexing.
    pub language: String,
    /// Names of non-file symbols defined in this file (functions, classes, etc.).
    pub symbols: Vec<String>,
    /// Files this file imports (should be read before this one).
    pub imports_from: Vec<String>,
    /// Files that import this file.
    pub imported_by: Vec<String>,
    /// Human-readable explanation of why this file is at this position.
    pub reason: String,
}
