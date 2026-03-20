//! Serde-deserializable types for ground truth TOML files.
//!
//! Expected TOML format:
//!
//! ```toml
//! [meta]
//! language = "python"
//! project = "click"
//! version = "8.3.1"
//! root = "src/click"
//!
//! [[symbols]]
//! file = "src/click/core.py"
//! name = "Command"
//! kind = "Class"
//!
//! [[relationships]]
//! kind = "Inherits"
//! source = "src/click/core.py::Group"
//! target = "src/click/core.py::Command"
//! tier = "must_find"
//!
//! [[impact_queries]]
//! target = "src/click/core.py::Command.invoke"
//! direction = "upstream"
//! expected = ["src/click/core.py::Group.invoke"]
//! hops = 1
//!
//! [[edge_cases]]
//! pattern = "decorated_function"
//! file = "src/click/decorators.py"
//! expected_symbol = "src/click/decorators.py::command"
//! pass_condition = "symbol_exists"
//! ```

use serde::Deserialize;

/// Top-level ground truth document.
#[derive(Debug, Deserialize)]
pub struct GroundTruth {
    pub meta: Meta,
    #[serde(default)]
    pub symbols: Vec<ExpectedSymbol>,
    #[serde(default)]
    pub relationships: Vec<ExpectedRelationship>,
    #[serde(default)]
    pub impact_queries: Vec<ImpactQuery>,
    #[serde(default)]
    pub edge_cases: Vec<EdgeCase>,
}

/// Project metadata block.
#[derive(Debug, Deserialize)]
pub struct Meta {
    pub language: String,
    pub project: String,
    pub version: String,
    /// Subdirectory to parse, relative to the project root passed on the CLI.
    /// If empty or ".", the project root itself is parsed.
    pub root: Option<String>,
}

/// A symbol that must exist in the index.
#[derive(Debug, Deserialize)]
pub struct ExpectedSymbol {
    /// Relative file path from the project root (e.g. "src/click/core.py").
    pub file: String,
    pub name: String,
    /// "File", "Function", "Class", or "Method"
    pub kind: String,
}

/// A relationship that must (or should) exist in the index.
#[derive(Debug, Deserialize)]
pub struct ExpectedRelationship {
    /// "Calls", "Imports", "Defines", or "Inherits"
    pub kind: String,
    /// "relative/path.py::SymbolName"
    pub source: String,
    /// "relative/path.py::SymbolName"
    pub target: String,
    /// "must_find" or "should_find"
    pub tier: String,
}

impl ExpectedRelationship {
    /// Split "some/path.py::SymbolName" into ("some/path.py", "SymbolName").
    /// Returns None if the separator is absent.
    /// Uses the FIRST "::" so "src/foo.rs::Type::method" gives
    /// ("src/foo.rs", "Type::method") correctly.
    pub fn parse_ref(r: &str) -> Option<(&str, &str)> {
        let pos = r.find("::")?;
        Some((&r[..pos], &r[pos + 2..]))
    }

    pub fn source_parts(&self) -> Option<(&str, &str)> {
        Self::parse_ref(&self.source)
    }

    pub fn target_parts(&self) -> Option<(&str, &str)> {
        Self::parse_ref(&self.target)
    }

    /// Returns true if source has no "::" separator (file-only reference).
    pub fn source_is_file_only(&self) -> bool {
        !self.source.contains("::")
    }

    /// Returns true if target has no "::" separator (file-only reference).
    pub fn target_is_file_only(&self) -> bool {
        !self.target.contains("::")
    }
}

/// An impact traversal query and its expected result set.
#[derive(Debug, Deserialize)]
pub struct ImpactQuery {
    /// "relative/path.py::SymbolName" - the starting node.
    pub target: String,
    /// "upstream" or "downstream"
    pub direction: String,
    /// Expected reachable nodes as "relative/path.py::SymbolName" strings.
    pub expected: Vec<String>,
    /// Maximum traversal depth.
    pub hops: u32,
}

/// A single edge-case check.
#[derive(Debug, Deserialize)]
pub struct EdgeCase {
    pub pattern: String,
    #[allow(dead_code)]
    pub file: Option<String>,
    /// "relative/path.py::SymbolName" for symbol_exists checks.
    pub expected_symbol: Option<String>,
    /// "relative/path.py::SymbolName" for relationship_exists checks.
    pub expected_relationship: Option<String>,
    /// "symbol_exists" or "relationship_exists"
    pub pass_condition: String,
}
