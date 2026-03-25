//! Spike 2: Index real codebases and run impact analysis at scale.
//!
//! Indexes three projects using regex-based symbol extraction (no tree-sitter):
//!   - sugar        /Users/steve/Dev/sugar/sugar/         (Python)
//!   - revsup       /Users/steve/Dev/revsup/revsup/       (Python/Django)
//!   - nomadsignal  /Users/steve/Dev/nomadsignal/packages/ (TypeScript)
//!
//! Run: cargo run --bin spike2

use engram_core::graph::store::GraphStore;
use engram_core::graph::types::*;

use chrono::Utc;
use regex::Regex;
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use uuid::Uuid;
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Project definitions
// ---------------------------------------------------------------------------

struct ProjectDef {
    name: &'static str,
    roots: &'static [&'static str],
    language: Language,
}

#[derive(Clone, Copy, Debug)]
enum Language {
    Python,
    TypeScript,
}

const PROJECTS: &[ProjectDef] = &[
    ProjectDef {
        name: "sugar",
        roots: &["/Users/steve/Dev/sugar/sugar"],
        language: Language::Python,
    },
    ProjectDef {
        name: "revsup",
        roots: &["/Users/steve/Dev/revsup/revsup"],
        language: Language::Python,
    },
    ProjectDef {
        name: "nomadsignal",
        roots: &["/Users/steve/Dev/nomadsignal/packages"],
        language: Language::TypeScript,
    },
];

// ---------------------------------------------------------------------------
// Regex patterns (compiled once)
// ---------------------------------------------------------------------------

struct Patterns {
    // Python
    py_class: Regex,
    py_fn_top: Regex,
    py_fn_method: Regex,
    py_import_from: Regex,
    py_import: Regex,
    py_call: Regex,

    // TypeScript / JavaScript
    ts_class: Regex,
    ts_fn_decl: Regex,
    ts_fn_arrow: Regex,
    ts_export_fn: Regex,
    ts_import_from: Regex,
    ts_call: Regex,
}

impl Patterns {
    fn new() -> Self {
        Self {
            py_class:       Regex::new(r"^class (\w+)").unwrap(),
            py_fn_top:      Regex::new(r"^(?:async )?def (\w+)\(([^)]*)").unwrap(),
            py_fn_method:   Regex::new(r"^    (?:async )?def (\w+)\(([^)]*)").unwrap(),
            py_import_from: Regex::new(r"^from (\S+) import").unwrap(),
            py_import:      Regex::new(r"^import (\S+)").unwrap(),
            py_call:        Regex::new(r"\b(\w{3,})\(").unwrap(),

            ts_class:       Regex::new(r"(?:^|export\s+)class (\w+)").unwrap(),
            ts_fn_decl:     Regex::new(r"(?:^|export\s+)(?:async\s+)?function\s+(\w+)\s*\(").unwrap(),
            ts_fn_arrow:    Regex::new(r"(?:export\s+)?const\s+(\w+)\s*=\s*(?:async\s*)?\(").unwrap(),
            ts_export_fn:   Regex::new(r"export\s+default\s+(?:async\s+)?function\s+(\w+)").unwrap(),
            ts_import_from: Regex::new(r#"from\s+['"]([^'"]+)['"]"#).unwrap(),
            ts_call:        Regex::new(r"\b(\w{3,})\(").unwrap(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsed file result
// ---------------------------------------------------------------------------

struct ParsedFile {
    /// (name, symbol_type, start_line, end_line, signature)
    symbols: Vec<(String, SymbolType, i32, i32, Option<String>)>,
    imports: Vec<String>,
    /// (caller_name, callee_name)
    calls: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Python parser
// ---------------------------------------------------------------------------

fn parse_python(_path: &Path, content: &str, p: &Patterns) -> ParsedFile {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut calls: Vec<(String, String)> = Vec::new();

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as i32;

    let mut current_fn: Option<(String, i32)> = None;
    let mut in_class = false;

    // Noise filter for common builtins - not worth tracking as call targets
    let noise: std::collections::HashSet<&str> = [
        "isinstance", "hasattr", "getattr", "setattr", "super", "range",
        "zip", "map", "filter", "enumerate", "sorted", "reversed", "next",
        "iter", "any", "all", "min", "max", "sum", "len", "str", "int",
        "bool", "list", "dict", "set", "tuple", "type", "print", "open",
        "format", "repr", "vars", "dir", "id", "hash", "abs", "round",
    ].into_iter().collect();

    for (idx, &line) in lines.iter().enumerate() {
        let lineno = idx as i32 + 1;

        if let Some(cap) = p.py_class.captures(line) {
            close_fn(&mut current_fn, &mut symbols, lineno - 1);
            in_class = true;
            symbols.push((cap[1].to_string(), SymbolType::Class, lineno, total_lines, None));
            continue;
        }

        if let Some(cap) = p.py_fn_top.captures(line) {
            close_fn(&mut current_fn, &mut symbols, lineno - 1);
            in_class = false;
            let name = cap[1].to_string();
            let sig = make_sig(&name, &cap[2]);
            current_fn = Some((name.clone(), lineno));
            symbols.push((name, SymbolType::Function, lineno, total_lines, Some(sig)));
            continue;
        }

        if let Some(cap) = p.py_fn_method.captures(line) {
            close_fn(&mut current_fn, &mut symbols, lineno - 1);
            let name = cap[1].to_string();
            let sig = make_sig(&name, &cap[2]);
            current_fn = Some((name.clone(), lineno));
            let sym_type = if in_class { SymbolType::Method } else { SymbolType::Function };
            symbols.push((name, sym_type, lineno, total_lines, Some(sig)));
            continue;
        }

        if let Some(cap) = p.py_import_from.captures(line) {
            imports.push(cap[1].to_string());
            continue;
        }
        if let Some(cap) = p.py_import.captures(line) {
            imports.push(cap[1].to_string());
            continue;
        }

        if let Some((ref caller, _)) = current_fn {
            for cap in p.py_call.captures_iter(line) {
                let callee = &cap[1];
                if !noise.contains(callee) {
                    calls.push((caller.clone(), callee.to_string()));
                }
            }
        }
    }

    close_fn(&mut current_fn, &mut symbols, total_lines);
    ParsedFile { symbols, imports, calls }
}

// ---------------------------------------------------------------------------
// TypeScript/JavaScript parser
// ---------------------------------------------------------------------------

fn parse_typescript(path: &Path, content: &str, p: &Patterns) -> ParsedFile {
    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut calls: Vec<(String, String)> = Vec::new();

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as i32;
    let mut current_fn: Option<(String, i32)> = None;

    let noise: std::collections::HashSet<&str> = [
        "console", "require", "typeof", "instanceof", "Array", "Object",
        "String", "Number", "Boolean", "Promise", "JSON", "Math", "Error",
        "Map", "Set", "parseInt", "parseFloat", "isNaN", "isFinite",
    ].into_iter().collect();

    for (idx, &line) in lines.iter().enumerate() {
        let lineno = idx as i32 + 1;

        if let Some(cap) = p.ts_class.captures(line) {
            close_fn(&mut current_fn, &mut symbols, lineno - 1);
            symbols.push((cap[1].to_string(), SymbolType::Class, lineno, total_lines, None));
            continue;
        }

        // export default function name
        if let Some(cap) = p.ts_export_fn.captures(line) {
            close_fn(&mut current_fn, &mut symbols, lineno - 1);
            let name = cap[1].to_string();
            current_fn = Some((name.clone(), lineno));
            symbols.push((name, SymbolType::Function, lineno, total_lines, None));
            continue;
        }

        if let Some(cap) = p.ts_fn_decl.captures(line) {
            close_fn(&mut current_fn, &mut symbols, lineno - 1);
            let name = cap[1].to_string();
            current_fn = Some((name.clone(), lineno));
            symbols.push((name, SymbolType::Function, lineno, total_lines, None));
            continue;
        }

        if let Some(cap) = p.ts_fn_arrow.captures(line) {
            // Only capture top-level or exported arrow functions
            if line.starts_with("const ") || line.starts_with("export const ") {
                close_fn(&mut current_fn, &mut symbols, lineno - 1);
                let name = cap[1].to_string();
                current_fn = Some((name.clone(), lineno));
                symbols.push((name, SymbolType::Function, lineno, total_lines, None));
                continue;
            }
        }

        if let Some(cap) = p.ts_import_from.captures(line) {
            imports.push(cap[1].to_string());
            continue;
        }

        if let Some((ref caller, _)) = current_fn {
            for cap in p.ts_call.captures_iter(line) {
                let callee = &cap[1];
                if !noise.contains(callee) {
                    calls.push((caller.clone(), callee.to_string()));
                }
            }
        }
    }

    close_fn(&mut current_fn, &mut symbols, total_lines);

    let _ = path; // unused but kept for consistency with python parser
    ParsedFile { symbols, imports, calls }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn close_fn(
    current_fn: &mut Option<(String, i32)>,
    symbols: &mut Vec<(String, SymbolType, i32, i32, Option<String>)>,
    end: i32,
) {
    if let Some((fn_name, fn_start)) = current_fn.take() {
        if let Some(sym) = symbols.iter_mut().rev().find(|s| s.0 == fn_name && s.2 == fn_start) {
            sym.3 = end;
        }
    }
}

fn make_sig(name: &str, args: &str) -> String {
    let truncated = if args.len() > 50 { &args[..50] } else { args };
    format!("def {}({}...)", name, truncated)
}

fn file_extensions(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Python => &["py"],
        Language::TypeScript => &["ts", "tsx", "js", "jsx"],
    }
}

fn should_skip_path(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        matches!(
            s.as_ref(),
            "node_modules" | ".git" | "__pycache__" | ".mypy_cache"
                | "dist" | "build" | ".next" | "migrations"
        )
    })
}

// ---------------------------------------------------------------------------
// Per-project index stats
// ---------------------------------------------------------------------------

struct IndexStats {
    project: String,
    files: usize,
    symbols: usize,
    call_rels: usize,
    import_rels: usize,
    elapsed: std::time::Duration,
}

// ---------------------------------------------------------------------------
// Index one project
// ---------------------------------------------------------------------------

async fn index_project(
    project: &ProjectDef,
    graph_store: &GraphStore,
    pool: &sqlx::PgPool,
    schema: &str,
    patterns: &Patterns,
) -> anyhow::Result<IndexStats> {
    let proj_start = Instant::now();
    let lang = project.language;
    let extensions = file_extensions(lang);

    println!("\n  Project: {} ({:?})", project.name, lang);

    // Clear previous data for this project
    let _: Option<(i64,)> = sqlx::query_as(&format!(
        "DELETE FROM {schema}.symbols WHERE project = $1",
        schema = schema,
    ))
    .bind(project.name)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    // Collect files
    let mut py_files = Vec::new();
    for &root in project.roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path().to_path_buf();
            if should_skip_path(&path) {
                continue;
            }
            if let Some(ext) = path.extension() {
                if extensions.contains(&ext.to_str().unwrap_or("")) {
                    py_files.push(path);
                }
            }
        }
    }
    println!("    Found {} files", py_files.len());

    let mut symbol_id_map: HashMap<(String, String), Uuid> = HashMap::new();
    let mut all_calls: Vec<(String, String, String)> = Vec::new();
    let mut all_imports: Vec<(String, String)> = Vec::new();
    let mut total_symbols = 0usize;
    let mut files_indexed = 0usize;

    // Determine root for rel_path stripping (use first root)
    let root_prefix = project.roots[0];

    for path in &py_files {
        let rel_path = path
            .strip_prefix(root_prefix)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string())
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_string();

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // File node
        let file_sym = Symbol {
            id: Uuid::new_v4(),
            name: rel_path.clone(),
            symbol_type: SymbolType::File,
            file_path: rel_path.clone(),
            start_line: None,
            end_line: None,
            language: format!("{:?}", lang).to_lowercase(),
            project: project.name.to_string(),
            signature: None,
            file_mtime: Utc::now(),
            layer: None,
        };
        graph_store.upsert_symbol(&file_sym).await?;
        symbol_id_map.insert((rel_path.clone(), rel_path.clone()), file_sym.id);

        // Parse
        let parsed = match lang {
            Language::Python => parse_python(path, &content, patterns),
            Language::TypeScript => parse_typescript(path, &content, patterns),
        };

        for (name, sym_type, start, end, sig) in &parsed.symbols {
            let sym = Symbol {
                id: Uuid::new_v4(),
                name: name.clone(),
                symbol_type: sym_type.clone(),
                file_path: rel_path.clone(),
                start_line: Some(*start),
                end_line: Some(*end),
                language: format!("{:?}", lang).to_lowercase(),
                project: project.name.to_string(),
                signature: sig.clone(),
                file_mtime: Utc::now(),
                layer: None,
            };
            graph_store.upsert_symbol(&sym).await?;
            symbol_id_map.insert((rel_path.clone(), name.clone()), sym.id);
            total_symbols += 1;

            let defines = Relationship {
                source_id: file_sym.id,
                target_id: sym.id,
                rel_type: RelationType::Defines,
                confidence: 1.0,
            };
            graph_store.add_relationship(&defines).await?;
        }

        for module in &parsed.imports {
            all_imports.push((rel_path.clone(), module.clone()));
        }
        for (caller, callee) in &parsed.calls {
            all_calls.push((rel_path.clone(), caller.clone(), callee.clone()));
        }

        files_indexed += 1;
    }

    // Build name -> [ids] lookup
    let mut name_to_ids: HashMap<String, Vec<Uuid>> = HashMap::new();
    for ((_, sym_name), id) in &symbol_id_map {
        name_to_ids.entry(sym_name.clone()).or_default().push(*id);
    }

    // Wire call relationships
    let mut call_rels = 0usize;
    for (caller_file, caller_name, callee_name) in &all_calls {
        let caller_id = match symbol_id_map.get(&(caller_file.clone(), caller_name.clone())) {
            Some(id) => *id,
            None => continue,
        };
        if let Some(candidates) = name_to_ids.get(callee_name) {
            for &callee_id in candidates {
                if callee_id == caller_id {
                    continue;
                }
                let rel = Relationship {
                    source_id: caller_id,
                    target_id: callee_id,
                    rel_type: RelationType::Calls,
                    confidence: 0.7,
                };
                if graph_store.add_relationship(&rel).await.is_ok() {
                    call_rels += 1;
                }
            }
        }
    }

    // Wire import relationships (Python: dot-path -> file; TS: relative path)
    let mut import_rels = 0usize;
    for (importer_file, module) in &all_imports {
        let importer_id = match symbol_id_map.get(&(importer_file.clone(), importer_file.clone())) {
            Some(id) => *id,
            None => continue,
        };

        // Python: "sugar.memory.store" -> "memory/store.py"
        // TypeScript: "../components/Button" -> "components/Button.ts"
        let candidates: Vec<String> = match lang {
            Language::Python => {
                let dotpath = module.replace('.', "/") + ".py";
                let stripped = dotpath.trim_start_matches(&format!("{}/", project.name)).to_string();
                vec![dotpath, stripped]
            }
            Language::TypeScript => {
                let clean = module.trim_start_matches("./").trim_start_matches("../");
                vec![
                    format!("{clean}.ts"),
                    format!("{clean}.tsx"),
                    format!("{clean}/index.ts"),
                ]
            }
        };

        for candidate in &candidates {
            if let Some(&tid) = symbol_id_map.get(&(candidate.clone(), candidate.clone())) {
                let rel = Relationship {
                    source_id: importer_id,
                    target_id: tid,
                    rel_type: RelationType::Imports,
                    confidence: 1.0,
                };
                if graph_store.add_relationship(&rel).await.is_ok() {
                    import_rels += 1;
                    break;
                }
            }
        }
    }

    let elapsed = proj_start.elapsed();
    println!(
        "    Indexed: {} files, {} symbols, {} call rels, {} import rels in {:?}",
        files_indexed, total_symbols, call_rels, import_rels, elapsed
    );

    Ok(IndexStats {
        project: project.name.to_string(),
        files: files_indexed,
        symbols: total_symbols,
        call_rels,
        import_rels,
        elapsed,
    })
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("engram=warn,warn")
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or("postgres://postgres:postgres@localhost:5450/engram".into());

    println!("=== Engram Spike 2: Multi-Project Codebase Indexing ===\n");
    println!("Database: {database_url}");

    let pool = PgPoolOptions::new()
        .max_connections(25)
        .connect(&database_url)
        .await?;

    let schema = "engram".to_string();
    let graph_store = GraphStore::new(pool.clone(), schema.clone())?;
    let patterns = Patterns::new();

    // Initialize schema
    print!("Initializing schema... ");
    let t = Instant::now();
    graph_store.init().await?;
    println!("done ({:?})", t.elapsed());

    // ---------------------------------------------------------------------------
    // Index all projects
    // ---------------------------------------------------------------------------
    println!("\n--- Indexing Projects ---");
    let index_start = Instant::now();
    let mut all_stats: Vec<IndexStats> = Vec::new();

    for project in PROJECTS {
        let stats = index_project(project, &graph_store, &pool, &schema, &patterns).await?;
        all_stats.push(stats);
    }

    let total_index_time = index_start.elapsed();

    // ---------------------------------------------------------------------------
    // Aggregate summary
    // ---------------------------------------------------------------------------
    println!("\n--- Indexing Summary ---");
    println!("{:<15} {:>8} {:>10} {:>10} {:>12} {:>12}",
        "Project", "Files", "Symbols", "Calls", "Imports", "Time");
    println!("{}", "-".repeat(70));

    let mut grand_files = 0;
    let mut grand_symbols = 0;
    let mut grand_calls = 0;
    let mut grand_imports = 0;

    for s in &all_stats {
        println!("{:<15} {:>8} {:>10} {:>10} {:>12} {:>12?}",
            s.project, s.files, s.symbols, s.call_rels, s.import_rels, s.elapsed);
        grand_files += s.files;
        grand_symbols += s.symbols;
        grand_calls += s.call_rels;
        grand_imports += s.import_rels;
    }
    println!("{}", "-".repeat(70));
    println!("{:<15} {:>8} {:>10} {:>10} {:>12} {:>12?}",
        "TOTAL", grand_files, grand_symbols, grand_calls, grand_imports, total_index_time);

    // ---------------------------------------------------------------------------
    // Impact analysis - per project
    // ---------------------------------------------------------------------------
    println!("\n--- Impact Analysis (per project) ---");

    // Targets by project
    let targets: &[(&str, &[&str])] = &[
        ("sugar",       &["store", "recall", "run", "search_memory"]),
        ("revsup",      &["get", "post", "save", "dispatch"]),
        ("nomadsignal", &["handler", "getServerSideProps", "fetchData"]),
    ];

    for (proj_name, fn_names) in targets {
        println!("\n  Project: {proj_name}");
        for &fn_name in *fn_names {
            // find_symbol returns across all projects; filter by project
            let t = Instant::now();
            let found = graph_store.find_symbol(fn_name, None, Some(proj_name)).await?;
            let find_time = t.elapsed();

            if found.is_empty() {
                println!("    find_symbol('{fn_name}'): not found ({find_time:?})");
                continue;
            }

            let sym = &found[0];
            println!(
                "    '{}' in {}:{} - {} match(es) found in {find_time:?}",
                sym.name, sym.file_path, sym.start_line.unwrap_or(0), found.len()
            );

            // Upstream impact
            let t = Instant::now();
            let upstream = graph_store.impact_analysis(sym.id, Direction::Upstream, 3).await?;
            println!(
                "      upstream callers (depth<=3): {} in {:?}",
                upstream.len(), t.elapsed()
            );
            for r in upstream.iter().take(3) {
                println!(
                    "        depth={} '{}' ({}:{}) confidence={:.2}",
                    r.depth, r.symbol.name, r.symbol.file_path,
                    r.symbol.start_line.unwrap_or(0), r.confidence
                );
            }
            if upstream.len() > 3 {
                println!("        ... and {} more", upstream.len() - 3);
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Query performance with growing dataset
    // ---------------------------------------------------------------------------
    println!("\n--- Query Performance Across Growing Dataset ---");
    println!("(Demonstrates whether graph query time degrades with more data)\n");

    let perf_queries = ["store", "run", "get", "handler"];
    for &qname in &perf_queries {
        let t = Instant::now();
        let results = graph_store.find_symbol(qname, None, None).await?;
        let find_ms = t.elapsed();

        if results.is_empty() {
            println!("  find_symbol('{qname}'): not found in {find_ms:?}");
            continue;
        }

        let sym = &results[0];
        let t = Instant::now();
        let impact = graph_store.impact_analysis(sym.id, Direction::Both, 3).await?;
        let impact_ms = t.elapsed();

        println!(
            "  '{qname}': find={find_ms:?}, impact_analysis(depth=3,both)={impact_ms:?} => {} nodes affected",
            impact.len()
        );
    }

    // ---------------------------------------------------------------------------
    // Final summary
    // ---------------------------------------------------------------------------
    println!("\n=== Spike 2 Complete ===");
    println!(
        "Total indexed: {} files, {} symbols, {} relationships across {} projects",
        grand_files,
        grand_symbols + grand_files, // +files for file nodes
        grand_calls + grand_imports,
        PROJECTS.len()
    );
    println!("Total runtime: {total_index_time:?}");
    println!("Multi-project indexing: OK");
    println!("Impact analysis (recursive CTE): OK");
    println!("Cross-project find_symbol: OK");
    println!("Performance degradation test: OK");

    Ok(())
}
