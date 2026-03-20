//! Comparator: matches an IndexResult against a GroundTruth and produces
//! structured comparison results consumed by the scorer.

use std::collections::{HashMap, HashSet, VecDeque};

use engram_core::graph::types::{RelationType, Symbol, SymbolType};
use engram_core::parser::IndexResult;

use crate::ground_truth::{EdgeCase, GroundTruth, ImpactQuery};

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct ComparisonResult {
    pub symbol_results: Vec<SymbolResult>,
    pub relationship_results: Vec<RelationshipResult>,
    pub impact_results: Vec<ImpactResult>,
    pub edge_case_results: Vec<EdgeCaseResult>,
}

#[derive(Debug)]
pub struct SymbolResult {
    pub file: String,
    pub name: String,
    pub kind: String,
    pub found: bool,
}

#[derive(Debug)]
pub struct RelationshipResult {
    pub kind: String,
    pub source: String,
    pub target: String,
    pub tier: String,
    pub found: bool,
}

#[derive(Debug)]
pub struct ImpactResult {
    pub target: String,
    pub direction: String,
    /// Nodes actually reached by BFS (as "file::name").
    pub actual: HashSet<String>,
    /// Nodes declared in the ground truth as expected.
    pub expected: HashSet<String>,
}

#[derive(Debug)]
pub struct EdgeCaseResult {
    pub pattern: String,
    #[allow(dead_code)]
    pub pass_condition: String,
    pub passed: bool,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compare(index: &IndexResult, ground_truth: &GroundTruth) -> ComparisonResult {
    let mut result = ComparisonResult::default();

    result.symbol_results = compare_symbols(index, ground_truth);
    result.relationship_results = compare_relationships(index, ground_truth);
    result.impact_results = compare_impact_queries(index, ground_truth);
    result.edge_case_results = compare_edge_cases(index, ground_truth);

    result
}

// ---------------------------------------------------------------------------
// Symbol comparison
// ---------------------------------------------------------------------------

fn compare_symbols(index: &IndexResult, ground_truth: &GroundTruth) -> Vec<SymbolResult> {
    ground_truth
        .symbols
        .iter()
        .map(|expected| {
            let expected_kind = parse_symbol_type(&expected.kind);
            // Ground truth may use qualified names like "Context.invoke", "Controller::new",
            // or "Sidekiq::Launcher#run" (Ruby instance methods). Try several bare-name forms.
            let bare_name_dot = expected.name.rsplit('.').next().unwrap_or(&expected.name);
            let bare_name_colons = expected
                .name
                .rsplit("::")
                .next()
                .unwrap_or(&expected.name);
            // Ruby: "Sidekiq::Launcher#run" -> after last '::' is "Launcher#run";
            // strip up to '#' to get "run".
            let bare_name_hash: Option<&str> = bare_name_colons
                .find('#')
                .map(|pos| &bare_name_colons[pos + 1..]);
            // Also handle "Launcher#run" (no '::' prefix) -> "run"
            let bare_name_hash_direct: Option<&str> = expected
                .name
                .find('#')
                .map(|pos| &expected.name[pos + 1..]);
            let found = index.symbols.iter().any(|sym| {
                if !sym.file_path.ends_with(&expected.file) {
                    return false;
                }
                let name_ok = sym.name == expected.name
                    || sym.name == bare_name_dot
                    || sym.name == bare_name_colons
                    || bare_name_hash.map_or(false, |n| sym.name == n)
                    || bare_name_hash_direct.map_or(false, |n| sym.name == n);
                if !name_ok {
                    return false;
                }
                // Function and Method are interchangeable for qualified names
                // (e.g. ground truth uses "Function" for Rust impl methods stored
                // as SymbolType::Method by the parser).
                let kind_ok = expected_kind.as_ref().map_or(false, |k| {
                    &sym.symbol_type == k
                        || (*k == SymbolType::Function
                            && sym.symbol_type == SymbolType::Method)
                        || (*k == SymbolType::Method
                            && sym.symbol_type == SymbolType::Function)
                });
                kind_ok
            });
            SymbolResult {
                file: expected.file.clone(),
                name: expected.name.clone(),
                kind: expected.kind.clone(),
                found,
            }
        })
        .collect()
}

fn parse_symbol_type(s: &str) -> Option<SymbolType> {
    match s {
        "File" => Some(SymbolType::File),
        "Function" => Some(SymbolType::Function),
        "Class" | "Struct" | "Enum" | "Trait" | "Interface" | "Module" | "Object" => Some(SymbolType::Class),
        "Method" => Some(SymbolType::Method),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Relationship comparison
// ---------------------------------------------------------------------------

fn compare_relationships(
    index: &IndexResult,
    ground_truth: &GroundTruth,
) -> Vec<RelationshipResult> {
    // Build a fast lookup: symbol_id -> Symbol for source/target resolution.
    let id_to_sym: HashMap<_, _> = index.symbols.iter().map(|s| (s.id, s)).collect();

    ground_truth
        .relationships
        .iter()
        .map(|expected| {
            let rel_type = parse_rel_type(&expected.kind);

            let found = rel_type.map_or(false, |rt| {
                // For Imports: the parser emits file-to-file relationships (source File symbol
                // -> target File symbol).  Ground truth may use "file::symbol" for the target
                // (particularly for Rust, which imports named items from a file).  We treat the
                // file portion of the target ref as sufficient for an Imports match.
                let is_import = rt == RelationType::Imports;
                let is_call = rt == RelationType::Calls;

                // Detect whether the expected target is a bare stdlib/external name
                // (no "::" and no "/" path separator).  For such names the parser
                // emits a synthetic deterministic UUID via Uuid::new_v5 rather than
                // a Symbol record, so id_to_sym won't contain it.
                let tgt_is_bare_name = !expected.target.contains("::")
                    && !expected.target.contains('/');
                let tgt_bare_uuid = if tgt_is_bare_name {
                    Some(uuid::Uuid::new_v5(
                        &uuid::Uuid::NAMESPACE_OID,
                        expected.target.as_bytes(),
                    ))
                } else {
                    None
                };

                // Pre-compute the set of target symbol IDs that match the expected target
                // (for use in the Calls fallback path).  This avoids O(n²) scanning.
                let matching_tgt_ids: HashSet<uuid::Uuid> = if is_call && !tgt_is_bare_name {
                    index
                        .symbols
                        .iter()
                        .filter(|s| sym_matches_ref(s, &expected.target))
                        .map(|s| s.id)
                        .collect()
                } else {
                    HashSet::new()
                };

                index.relationships.iter().any(|rel| {
                    if rel.rel_type != rt {
                        return false;
                    }
                    let src_sym = id_to_sym.get(&rel.source_id);
                    let Some(src) = src_sym else { return false; };
                    if !sym_matches_ref(src, &expected.source) {
                        return false;
                    }

                    // Match the target.
                    if is_import {
                        // Accept a match if the file portion of the expected target
                        // matches the actual target's file_path, regardless of symbol
                        // name.  This handles both file-only refs ("src/foo.py") and
                        // file::symbol refs ("src/foo.py::Bar") where the parser only
                        // records a file-level import.
                        // Split at FIRST "::" so "src/foo.rs::Type::method" -> "src/foo.rs".
                        let tgt_file = if let Some(pos) = expected.target.find("::") {
                            &expected.target[..pos]
                        } else {
                            expected.target.as_str()
                        };
                        id_to_sym
                            .get(&rel.target_id)
                            .map_or(false, |tgt| tgt.file_path.ends_with(tgt_file))
                    } else if tgt_is_bare_name {
                        // Stdlib/external target: match via deterministic UUID or
                        // by name if it happens to be in the symbol table.
                        if tgt_bare_uuid.map_or(false, |u| rel.target_id == u) {
                            return true;
                        }
                        id_to_sym
                            .get(&rel.target_id)
                            .map_or(false, |tgt| name_matches(&tgt.name, &expected.target))
                    } else if is_call {
                        // For Calls relationships, cross-file resolution may have assigned
                        // the wrong UUID when multiple symbols share the same bare name
                        // (e.g., multiple `start` methods across files). Accept the match
                        // if the relationship target is ANY symbol that matches the expected
                        // target by file + name, not just by the exact UUID that the walker
                        // happened to assign.
                        if matching_tgt_ids.contains(&rel.target_id) {
                            return true;
                        }
                        // Also handle synthetic (unresolved) UUIDs: check if the method
                        // name portion of the target matches, and accept lower-confidence.
                        id_to_sym
                            .get(&rel.target_id)
                            .map_or(false, |tgt| sym_matches_ref(tgt, &expected.target))
                    } else {
                        id_to_sym
                            .get(&rel.target_id)
                            .map_or(false, |tgt| sym_matches_ref(tgt, &expected.target))
                    }
                })
            });

            RelationshipResult {
                kind: expected.kind.clone(),
                source: expected.source.clone(),
                target: expected.target.clone(),
                tier: expected.tier.clone(),
                found,
            }
        })
        .collect()
}

fn parse_rel_type(s: &str) -> Option<RelationType> {
    match s {
        "Calls" => Some(RelationType::Calls),
        "Imports" => Some(RelationType::Imports),
        "Defines" => Some(RelationType::Defines),
        "Inherits" => Some(RelationType::Inherits),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Impact query comparison
// ---------------------------------------------------------------------------

fn compare_impact_queries(index: &IndexResult, ground_truth: &GroundTruth) -> Vec<ImpactResult> {
    // Build adjacency lists indexed by symbol id.
    // upstream_adj[x] = set of y such that y -> x (who points at x)
    // downstream_adj[x] = set of y such that x -> y (what x points at)
    let mut downstream: HashMap<uuid::Uuid, Vec<uuid::Uuid>> = HashMap::new();
    let mut upstream: HashMap<uuid::Uuid, Vec<uuid::Uuid>> = HashMap::new();

    for rel in &index.relationships {
        downstream
            .entry(rel.source_id)
            .or_default()
            .push(rel.target_id);
        upstream
            .entry(rel.target_id)
            .or_default()
            .push(rel.source_id);
    }

    let id_to_sym: HashMap<_, _> = index.symbols.iter().map(|s| (s.id, s)).collect();

    ground_truth
        .impact_queries
        .iter()
        .map(|query| run_impact_query(query, index, &id_to_sym, &upstream, &downstream))
        .collect()
}

fn run_impact_query(
    query: &ImpactQuery,
    index: &IndexResult,
    id_to_sym: &HashMap<uuid::Uuid, &Symbol>,
    upstream: &HashMap<uuid::Uuid, Vec<uuid::Uuid>>,
    downstream: &HashMap<uuid::Uuid, Vec<uuid::Uuid>>,
) -> ImpactResult {
    let expected: HashSet<String> = query.expected.iter().cloned().collect();

    // Find the starting symbol.
    // Both "file/path.py::SymbolName" and "file/path.py" (file-only) are supported.
    let start_sym = index
        .symbols
        .iter()
        .find(|s| sym_matches_ref(s, &query.target));

    let Some(start) = start_sym else {
        return ImpactResult {
            target: query.target.clone(),
            direction: query.direction.clone(),
            actual: HashSet::new(),
            expected,
        };
    };

    // BFS up to `hops` depth.
    let adj = if query.direction == "upstream" {
        upstream
    } else {
        downstream
    };

    let mut visited: HashSet<uuid::Uuid> = HashSet::new();
    let mut queue: VecDeque<(uuid::Uuid, u32)> = VecDeque::new();
    queue.push_back((start.id, 0));
    visited.insert(start.id);

    let mut actual: HashSet<String> = HashSet::new();

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth >= query.hops {
            continue;
        }
        if let Some(neighbors) = adj.get(&node_id) {
            for &neighbor_id in neighbors {
                if visited.contains(&neighbor_id) {
                    continue;
                }
                visited.insert(neighbor_id);
                if let Some(sym) = id_to_sym.get(&neighbor_id) {
                    // Represent as a relative "file::name" key.
                    // We store the full file_path - callers match using ends_with in the scorer.
                    actual.insert(format!("{}::{}", sym.file_path, sym.name));
                }
                queue.push_back((neighbor_id, depth + 1));
            }
        }
    }

    ImpactResult {
        target: query.target.clone(),
        direction: query.direction.clone(),
        actual,
        expected,
    }
}

// ---------------------------------------------------------------------------
// Edge case comparison
// ---------------------------------------------------------------------------

fn compare_edge_cases(index: &IndexResult, ground_truth: &GroundTruth) -> Vec<EdgeCaseResult> {
    let id_to_sym: HashMap<_, _> = index.symbols.iter().map(|s| (s.id, s)).collect();

    ground_truth
        .edge_cases
        .iter()
        .map(|ec| run_edge_case(ec, index, &id_to_sym))
        .collect()
}

fn run_edge_case(
    ec: &EdgeCase,
    index: &IndexResult,
    _id_to_sym: &HashMap<uuid::Uuid, &Symbol>,
) -> EdgeCaseResult {
    match ec.pass_condition.as_str() {
        "symbol_exists" => {
            let sym_ref = ec.expected_symbol.as_deref().unwrap_or("");
            let passed = if let Some((file, name)) = split_ref(sym_ref) {
                index
                    .symbols
                    .iter()
                    .any(|s| s.file_path.ends_with(file) && name_matches(&s.name, name))
            } else {
                false
            };
            EdgeCaseResult {
                pattern: ec.pattern.clone(),
                pass_condition: ec.pass_condition.clone(),
                passed,
                detail: if passed {
                    format!("Found symbol: {sym_ref}")
                } else {
                    format!("Symbol not found: {sym_ref}")
                },
            }
        }
        "relationship_exists" => {
            let rel_ref = ec.expected_relationship.as_deref().unwrap_or("");
            // expected_relationship format: "src::Name --Kind--> tgt::Name"
            // Parse with a simple heuristic: split on " --" and "-->"
            let passed = check_relationship_exists(rel_ref, index);
            EdgeCaseResult {
                pattern: ec.pattern.clone(),
                pass_condition: ec.pass_condition.clone(),
                passed,
                detail: if passed {
                    format!("Found relationship: {rel_ref}")
                } else {
                    format!("Relationship not found: {rel_ref}")
                },
            }
        }
        "file_parsed" => {
            // Check that a File symbol exists for the given file path.
            let file_ref = ec.file.as_deref().unwrap_or("");
            let passed = index
                .symbols
                .iter()
                .any(|s| s.symbol_type == SymbolType::File && s.file_path.ends_with(file_ref));
            EdgeCaseResult {
                pattern: ec.pattern.clone(),
                pass_condition: ec.pass_condition.clone(),
                passed,
                detail: if passed {
                    format!("File parsed: {file_ref}")
                } else {
                    format!("File not parsed: {file_ref}")
                },
            }
        }
        other => EdgeCaseResult {
            pattern: ec.pattern.clone(),
            pass_condition: ec.pass_condition.clone(),
            passed: false,
            detail: format!("Unknown pass_condition: {other}"),
        },
    }
}

/// Parse "src/file.py::SrcName --Kind--> tgt/file.py::TgtName" and check it exists.
///
/// Handles three target formats:
/// 1. `file::SymbolName`  - file path + symbol name (split at first "::")
/// 2. `file/path.rb`      - file-only ref (no "::", match any symbol in that file)
/// 3. `BareStdlibName`    - no path separator at all, match by symbol name only
///    (used for stdlib parents like `Interrupt` or `Forwardable`)
fn check_relationship_exists(rel_ref: &str, index: &IndexResult) -> bool {
    // Split on " --" to get (src_side, "Kind--> tgt_side")
    let Some(arrow_start) = rel_ref.find(" --") else {
        return false;
    };
    let src_part = &rel_ref[..arrow_start];
    let rest = &rel_ref[arrow_start + 3..]; // skip " --"

    // rest is "Kind--> tgt_side"
    let Some(arrow_end) = rest.find("-->") else {
        return false;
    };
    let kind = &rest[..arrow_end];
    let tgt_part = rest[arrow_end + 3..].trim();

    let rel_type = parse_rel_type(kind.trim());
    let Some(rt) = rel_type else {
        return false;
    };

    // Parse source ref.  Supports both "file::name" and file-only "file/path.rb".
    let src_str = src_part.trim();
    let (src_file, src_name): (&str, Option<&str>) = if let Some((f, n)) = split_ref(src_str) {
        (f, Some(n))
    } else {
        // File-only ref: match any symbol in that file.
        (src_str, None)
    };

    let id_to_sym: HashMap<_, _> = index.symbols.iter().map(|s| (s.id, s)).collect();

    // Determine target matching strategy based on whether tgt_part contains "::".
    // A ref with "/" is a file path; without "/" or "::" it's a bare stdlib name.
    let tgt_has_separator = tgt_part.contains("::");
    let tgt_is_file_only = !tgt_has_separator && tgt_part.contains('/');
    let tgt_is_bare_name = !tgt_has_separator && !tgt_is_file_only;

    index.relationships.iter().any(|rel| {
        if rel.rel_type != rt {
            return false;
        }
        let src_sym = id_to_sym.get(&rel.source_id);
        let src_ok = src_sym.map_or(false, |src| {
            if !src.file_path.ends_with(src_file) {
                return false;
            }
            // If src_name is None it's a file-only ref - match any symbol in that file.
            src_name.map_or(true, |n| name_matches(&src.name, n))
        });
        if !src_ok {
            return false;
        }

        // Match the target symbol using the appropriate strategy.
        if tgt_is_bare_name {
            // Stdlib / external: match any symbol (including synthetic UUID-based ones
            // that won't be in id_to_sym) by checking the target name against the
            // deterministic UUID we generate for unknown names.
            // The parser uses Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes()).
            let expected_id =
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, tgt_part.as_bytes());
            if rel.target_id == expected_id {
                return true;
            }
            // Also check if it's in id_to_sym (e.g. defined in the same project).
            id_to_sym
                .get(&rel.target_id)
                .map_or(false, |tgt| name_matches(&tgt.name, tgt_part))
        } else if tgt_is_file_only {
            // File-only: match any symbol whose file_path ends with the target path.
            // For Imports relationships the source is a file symbol; the target may
            // also be just a file symbol.
            id_to_sym
                .get(&rel.target_id)
                .map_or(false, |tgt| tgt.file_path.ends_with(tgt_part))
        } else {
            // Standard file::name format.
            let Some((tgt_file, tgt_name)) = split_ref(tgt_part) else {
                return false;
            };
            id_to_sym.get(&rel.target_id).map_or(false, |tgt| {
                tgt.file_path.ends_with(tgt_file) && name_matches(&tgt.name, tgt_name)
            })
        }
    })
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Split "relative/path.py::SymbolName" at the first "::" separator.
/// Returns (file_path, symbol_name).
///
/// Using the first "::" ensures that Rust-style refs like
/// "src/foo.rs::Type::method" correctly give file="src/foo.rs" and
/// symbol="Type::method", because file paths never contain "::".
pub fn split_ref(r: &str) -> Option<(&str, &str)> {
    let pos = r.find("::")?;
    Some((&r[..pos], &r[pos + 2..]))
}

/// Check if a symbol matches a reference, handling qualified names.
///
/// Supported forms:
/// - `"Context.invoke"` (Python): match symbol named "invoke" or "Context.invoke"
/// - `"Controller::new"` (Rust):  match symbol named "new" or "Controller::new"
/// - `"Sidekiq::Launcher#run"` (Ruby instance method): match symbol named "run"
/// - `"bare_name"`:               exact match only
fn name_matches(sym_name: &str, ref_name: &str) -> bool {
    if sym_name == ref_name {
        return true;
    }
    // Try bare name after last '.' (Python qualified: "Context.invoke" -> "invoke")
    let bare_dot = ref_name.rsplit('.').next().unwrap_or(ref_name);
    if sym_name == bare_dot {
        return true;
    }
    // Try bare name after last '::' (Rust qualified: "Controller::new" -> "new")
    if let Some(pos) = ref_name.rfind("::") {
        let bare_colons = &ref_name[pos + 2..];
        if sym_name == bare_colons {
            return true;
        }
        // Also strip '#' after '::' for Ruby: "Sidekiq::Launcher#run" -> after last "::" is
        // "Launcher#run"; then strip up to '#' to get "run".
        if let Some(hash_pos) = bare_colons.find('#') {
            let after_hash = &bare_colons[hash_pos + 1..];
            if sym_name == after_hash {
                return true;
            }
        }
    }
    // Try bare name after '#' (Ruby instance method: "Launcher#run" -> "run")
    if let Some(hash_pos) = ref_name.find('#') {
        let after_hash = &ref_name[hash_pos + 1..];
        if sym_name == after_hash {
            return true;
        }
    }
    false
}

/// Check whether a parsed Symbol matches a ground-truth reference string.
///
/// Two formats are supported:
///
/// 1. `"relative/path.py::SymbolName"` - the symbol's file_path must end with
///    "relative/path.py" AND its name must match "SymbolName" (bare or qualified).
///    Rust-style multi-segment: `"src/foo.rs::Type::method"` splits at the FIRST
///    `::` so file=`"src/foo.rs"`, sym=`"Type::method"`.
///
/// 2. `"relative/path.py"` (no "::") - the symbol's file_path must end with
///    "relative/path.py".  Any symbol in that file satisfies the match; the
///    File-type symbol is the canonical match for file-to-file import edges,
///    but other symbols (e.g. the specific imported name) are also accepted.
fn sym_matches_ref(sym: &Symbol, r: &str) -> bool {
    if let Some(pos) = r.find("::") {
        // Split at the FIRST "::" so that Rust-style "file.rs::Type::method"
        // gives file="file.rs" and sym="Type::method".
        let file_part = &r[..pos];
        let name_part = &r[pos + 2..];
        sym.file_path.ends_with(file_part) && name_matches(&sym.name, name_part)
    } else {
        // Format 2: file only - match any symbol whose file_path ends with r.
        sym.file_path.ends_with(r)
    }
}
