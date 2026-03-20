//! Smoke test: index a directory (no database) and print extracted symbols.
//!
//! Usage:
//!   cargo run --bin parser_smoke -- /path/to/project project_name
//!   cargo run --bin parser_smoke -- /Users/steve/Dev/sugar/sugar sugarai
//!   cargo run --bin parser_smoke -- /Users/steve/Dev/nomadsignal/src nomadsignal

use engram_core::graph::types::SymbolType;
use engram_core::parser::index_directory;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (dir, project) = match args.len() {
        3 => (args[1].as_str(), args[2].as_str()),
        2 => (args[1].as_str(), "smoke"),
        _ => {
            eprintln!("Usage: parser_smoke <directory> [project]");
            std::process::exit(1);
        }
    };

    let result = index_directory(dir, project, None)?;

    let files: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.symbol_type == SymbolType::File)
        .collect();
    let functions: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.symbol_type == SymbolType::Function)
        .collect();
    let classes: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.symbol_type == SymbolType::Class)
        .collect();
    let methods: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.symbol_type == SymbolType::Method)
        .collect();

    println!("=== Parser Smoke Test: {} ===", dir);
    println!(
        "Files parsed: {}  |  Skipped: {}",
        result.files_parsed, result.files_skipped
    );
    println!(
        "Symbols: {} total  ({} files, {} functions, {} classes, {} methods)",
        result.symbols.len(),
        files.len(),
        functions.len(),
        classes.len(),
        methods.len(),
    );
    println!("Relationships: {}", result.relationships.len());

    println!("\n--- Functions (first 20) ---");
    for sym in functions.iter().take(20) {
        println!(
            "  [{lang}] {name}  ({file}:{start})",
            lang = sym.language,
            name = sym.name,
            file = sym.file_path,
            start = sym.start_line.unwrap_or(0),
        );
        if let Some(sig) = &sym.signature {
            println!("    sig: {sig}");
        }
    }

    println!("\n--- Classes (first 10) ---");
    for sym in classes.iter().take(10) {
        println!(
            "  [{lang}] {name}  ({file}:{start})",
            lang = sym.language,
            name = sym.name,
            file = sym.file_path,
            start = sym.start_line.unwrap_or(0),
        );
    }

    Ok(())
}
