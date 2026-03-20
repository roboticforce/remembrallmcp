use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A code symbol (function, class, method, file).
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SymbolType {
    File,
    Function,
    Class,
    Method,
}

impl std::fmt::Display for SymbolType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File => write!(f, "file"),
            Self::Function => write!(f, "function"),
            Self::Class => write!(f, "class"),
            Self::Method => write!(f, "method"),
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
}

impl std::fmt::Display for RelationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Calls => write!(f, "calls"),
            Self::Imports => write!(f, "imports"),
            Self::Defines => write!(f, "defines"),
            Self::Inherits => write!(f, "inherits"),
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
