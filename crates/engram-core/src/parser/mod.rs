//! Source code parsers using tree-sitter.
//!
//! Supported languages: Python (.py), TypeScript (.ts, .tsx), JavaScript (.js, .jsx),
//! Rust (.rs), Ruby (.rb), Go (.go), Java (.java), Kotlin (.kt, .kts).
//!
//! # Entry points
//!
//! - [`parse_python_file`] - Python
//! - [`parse_ts_file`] - TypeScript/JavaScript
//! - [`parse_rust_file`] - Rust
//! - [`parse_ruby_file`] - Ruby
//! - [`parse_go_file`] - Go
//! - [`parse_java_file`] - Java
//! - [`parse_kotlin_file`] - Kotlin
//! - [`index_directory`] - walk a directory and parse all supported files
//!
//! # Example
//!
//! ```no_run
//! use engram_core::parser::index_directory;
//!
//! let result = index_directory("/path/to/project", "my_project", None).unwrap();
//! println!("{} symbols, {} relationships", result.symbols.len(), result.relationships.len());
//! ```

mod go;
mod java;
mod kotlin;
mod python;
mod ruby;
mod rust;
mod typescript;
mod walker;

pub use go::parse_go_file;
pub use java::parse_java_file;
pub use kotlin::parse_kotlin_file;
pub use python::{parse_python_file, FileParseResult};
pub use ruby::parse_ruby_file;
pub use rust::parse_rust_file;
pub use typescript::{parse_ts_file, TsLang};
pub use walker::{index_directory, IndexResult};
