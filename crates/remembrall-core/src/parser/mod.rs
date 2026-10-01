//! Source code parsers using tree-sitter.
//!
//! Supported languages: Python (.py), TypeScript (.ts, .tsx), JavaScript (.js, .jsx),
//! Rust (.rs), Ruby (.rb), Go (.go), Java (.java), Kotlin (.kt, .kts), C# (.cs).
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
//! - [`parse_csharp_file`] - C#
//! - [`parse_file`] - unified dispatch by file extension
//! - [`index_directory`] - walk a directory and parse all supported files
//!
//! # Example
//!
//! ```no_run
//! use remembrall_core::parser::index_directory;
//!
//! let result = index_directory("/path/to/project", "my_project", None).unwrap();
//! println!("{} symbols, {} relationships", result.symbols.len(), result.relationships.len());
//! ```

mod go;
mod java;
mod kotlin;
mod csharp;
mod python;
mod ruby;
mod rust;
mod typescript;
mod walker;

pub use go::parse_go_file;
pub use java::parse_java_file;
pub use kotlin::parse_kotlin_file;
pub use csharp::parse_csharp_file;
pub use python::{parse_python_file, FileParseResult, RawImport};
pub use ruby::parse_ruby_file;
pub use rust::parse_rust_file;
pub use typescript::{parse_ts_file, TsLang};
pub use walker::{index_directory, index_directory_strict, IndexResult};

use chrono::{DateTime, Utc};

/// Dispatch a single file to the appropriate language parser by file extension.
///
/// Returns `None` if the extension is not supported. This is the single place
/// that maps extensions to parsers - adding a new language requires:
///
/// 1. Write the parser module (e.g., `parser/mylang.rs`).
/// 2. Declare it as `mod mylang;` above and re-export its parse function.
/// 3. Add the extension to `crate::indexer::supported_extensions()`.
/// 4. Add a match arm here.
///
/// No changes to `walker.rs` are needed.
pub fn parse_file(
    ext: &str,
    file_path: &str,
    source: &str,
    project: &str,
    mtime: DateTime<Utc>,
) -> Option<FileParseResult> {
    match ext {
        "py" => Some(parse_python_file(file_path, source, project, mtime)),
        "rs" => Some(parse_rust_file(file_path, source, project, mtime)),
        "rb" => Some(parse_ruby_file(file_path, source, project, mtime)),
        "go" => Some(parse_go_file(file_path, source, project, mtime)),
        "java" => Some(parse_java_file(file_path, source, project, mtime)),
        "kt" | "kts" => Some(parse_kotlin_file(file_path, source, project, mtime)),
        "cs" => Some(parse_csharp_file(file_path, source, project, mtime)),
        ext => {
            let lang = TsLang::from_extension(ext)?;
            Some(parse_ts_file(file_path, source, project, mtime, lang))
        }
    }
}
