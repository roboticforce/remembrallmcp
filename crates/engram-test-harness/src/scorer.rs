//! Scorer: converts ComparisonResult into weighted dimension scores and a final grade.

use crate::comparator::{ComparisonResult, ImpactResult};

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Scores {
    pub symbol_recall: f64,
    pub symbol_precision: f64,
    pub relationship_recall: f64,
    pub relationship_precision: f64,
    pub import_resolution: f64,
    pub impact_score: f64,
    pub edge_case_score: f64,
    pub total: f64,
    pub grade: char,

    // Raw counts for the summary header.
    pub symbols_found: usize,
    pub symbols_total: usize,
    pub relationships_found: usize,
    pub relationships_total: usize,
    pub must_find_missed: usize,
    pub imports_found: usize,
    pub imports_total: usize,
    pub impact_queries_correct: usize,
    pub impact_queries_total: usize,
    pub edge_cases_passed: usize,
    pub edge_cases_total: usize,
}

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

const W_SYM_RECALL: f64 = 0.15;
const W_SYM_PRECISION: f64 = 0.15;
const W_REL_RECALL: f64 = 0.15;
const W_REL_PRECISION: f64 = 0.10;
const W_IMPORT: f64 = 0.15;
const W_IMPACT: f64 = 0.15;
const W_EDGE: f64 = 0.15;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn compute(cmp: &ComparisonResult) -> Scores {
    // --- Symbols ---
    let sym_total = cmp.symbol_results.len();
    let sym_found = cmp.symbol_results.iter().filter(|r| r.found).count();
    let symbol_recall = pct(sym_found, sym_total);
    let symbol_precision = 100.0; // false positives not measured yet

    // --- Relationships ---
    let rel_total = cmp.relationship_results.len();
    let rel_found = cmp.relationship_results.iter().filter(|r| r.found).count();
    let must_find_missed = cmp
        .relationship_results
        .iter()
        .filter(|r| !r.found && r.tier == "must_find")
        .count();
    let relationship_recall = pct(rel_found, rel_total);
    let relationship_precision = 100.0; // false positives not measured yet

    // --- Import resolution (subset of relationships where kind == "Imports") ---
    let imports: Vec<_> = cmp
        .relationship_results
        .iter()
        .filter(|r| r.kind == "Imports")
        .collect();
    let imports_total = imports.len();
    let imports_found = imports.iter().filter(|r| r.found).count();
    let import_resolution = pct(imports_found, imports_total);

    // --- Impact analysis ---
    let impact_queries_total = cmp.impact_results.len();
    // A query is "correct" when recall >= 1.0 (every expected node was found).
    let impact_queries_correct = cmp
        .impact_results
        .iter()
        .filter(|r| impact_recall(r) >= 1.0)
        .count();
    // Average recall across all queries.
    let impact_score = if impact_queries_total == 0 {
        100.0
    } else {
        cmp.impact_results
            .iter()
            .map(|r| impact_recall(r) * 100.0)
            .sum::<f64>()
            / impact_queries_total as f64
    };

    // --- Edge cases ---
    let edge_cases_total = cmp.edge_case_results.len();
    let edge_cases_passed = cmp.edge_case_results.iter().filter(|r| r.passed).count();
    let edge_case_score = pct(edge_cases_passed, edge_cases_total);

    // --- Weighted total ---
    let total = symbol_recall * W_SYM_RECALL
        + symbol_precision * W_SYM_PRECISION
        + relationship_recall * W_REL_RECALL
        + relationship_precision * W_REL_PRECISION
        + import_resolution * W_IMPORT
        + impact_score * W_IMPACT
        + edge_case_score * W_EDGE;

    let grade = letter_grade(total);

    Scores {
        symbol_recall,
        symbol_precision,
        relationship_recall,
        relationship_precision,
        import_resolution,
        impact_score,
        edge_case_score,
        total,
        grade,
        symbols_found: sym_found,
        symbols_total: sym_total,
        relationships_found: rel_found,
        relationships_total: rel_total,
        must_find_missed,
        imports_found,
        imports_total,
        impact_queries_correct,
        impact_queries_total,
        edge_cases_passed,
        edge_cases_total,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute recall for a single impact query.
/// recall = |actual intersect expected| / |expected|
fn impact_recall(r: &ImpactResult) -> f64 {
    if r.expected.is_empty() {
        return 1.0;
    }
    // The actual set stores entries as "<abs_file_path>::<sym_name>".
    // The expected set stores entries as either:
    //   "relative/path.py::SymbolName"  (file::symbol format)
    //   "relative/path.py"              (file-only format)
    //
    // For file::symbol format: check act.ends_with(exp).
    // For file-only format:    check that the file portion of some act ends_with exp.
    let hits = r
        .expected
        .iter()
        .filter(|exp| {
            r.actual.iter().any(|act| {
                impact_entry_matches(act, exp)
            })
        })
        .count();
    hits as f64 / r.expected.len() as f64
}

/// Check whether an `actual` BFS entry (absolute path, stored as "abs_file::sym_name")
/// matches an `expected` ground-truth entry (relative path).
///
/// Handles three cases:
///
/// 1. File-only expected (`"src/foo.py"`):
///    Extract the file portion of `act` (before "::") and check ends_with.
///
/// 2. Single-segment symbol (`"src/foo.py::Command"`):
///    Direct suffix match on act.
///
/// 3. Multi-segment Rust symbol (`"src/foo.rs::Controller::run"`):
///    The parser stores just the bare name ("run"), so actual is "abs/foo.rs::run".
///    Split expected at first "::" -> file="src/foo.rs", sym="Controller::run".
///    Extract bare name after last "::" from sym -> "run".
///    Then check: act file portion ends_with expected file AND act sym == bare name.
fn impact_entry_matches(act: &str, exp: &str) -> bool {
    // Locate the "::" separator in act (always present; format is "file::name").
    let act_sep = act.find("::");
    let (act_file, act_sym) = if let Some(p) = act_sep {
        (&act[..p], &act[p + 2..])
    } else {
        (act, "")
    };

    if !exp.contains("::") {
        // Case 1: file-only expected
        return act_file.ends_with(exp);
    }

    // Split expected at FIRST "::"
    let exp_sep = exp.find("::").unwrap();
    let exp_file = &exp[..exp_sep];
    let exp_sym = &exp[exp_sep + 2..];

    if !act_file.ends_with(exp_file) {
        return false;
    }

    // Bare name from expected sym (after last "::" or last ".")
    let exp_bare = exp_sym
        .rfind("::")
        .map(|p| &exp_sym[p + 2..])
        .or_else(|| exp_sym.rfind('.').map(|p| &exp_sym[p + 1..]))
        .unwrap_or(exp_sym);

    // Accept if act_sym matches the full expected sym OR just the bare name.
    act_sym == exp_sym || act_sym == exp_bare
}

/// Convert counts to a percentage. Returns 100.0 when total is 0 (vacuously perfect).
fn pct(found: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        (found as f64 / total as f64) * 100.0
    }
}

fn letter_grade(score: f64) -> char {
    if score >= 90.0 {
        'A'
    } else if score >= 80.0 {
        'B'
    } else if score >= 70.0 {
        'C'
    } else if score >= 60.0 {
        'D'
    } else {
        'F'
    }
}
