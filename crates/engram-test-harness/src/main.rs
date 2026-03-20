//! Engram test harness binary.
//!
//! Validates parser quality against a ground truth TOML file.
//!
//! Usage:
//!   engram-test --project /path/to/project --ground-truth /path/to/ground-truth.toml

mod comparator;
mod ground_truth;
mod scorer;

use std::path::PathBuf;

use anyhow::{Context, Result};
use engram_core::parser::index_directory;

use comparator::{compare, ImpactResult};
use ground_truth::GroundTruth;
use scorer::Scores;

fn main() -> Result<()> {
    let (project_root, gt_path) = parse_args()?;

    // 1. Load ground truth.
    let gt_raw =
        std::fs::read_to_string(&gt_path).with_context(|| format!("reading {}", gt_path.display()))?;
    let ground_truth: GroundTruth =
        toml::from_str(&gt_raw).with_context(|| format!("parsing {}", gt_path.display()))?;

    // 2. Determine the directory to parse.
    let parse_dir = match &ground_truth.meta.root {
        Some(r) if !r.is_empty() && r != "." => project_root.join(r),
        _ => project_root.clone(),
    };

    // 3. Index the project.
    let index = index_directory(&parse_dir, &ground_truth.meta.project, None)
        .with_context(|| format!("indexing {}", parse_dir.display()))?;

    // 4. Compare.
    let cmp = compare(&index, &ground_truth);

    // 5. Score.
    let scores = scorer::compute(&cmp);

    // 6. Print report.
    print_report(&ground_truth, &scores, &cmp);

    Ok(())
}

// ---------------------------------------------------------------------------
// CLI argument parsing (no extra deps - hand-rolled)
// ---------------------------------------------------------------------------

fn parse_args() -> Result<(PathBuf, PathBuf)> {
    let args: Vec<String> = std::env::args().collect();

    let mut project: Option<PathBuf> = None;
    let mut ground_truth: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--project" => {
                i += 1;
                project = Some(PathBuf::from(args.get(i).context("--project requires a value")?));
            }
            "--ground-truth" => {
                i += 1;
                ground_truth = Some(PathBuf::from(
                    args.get(i).context("--ground-truth requires a value")?,
                ));
            }
            other => {
                anyhow::bail!("Unknown argument: {other}");
            }
        }
        i += 1;
    }

    let project = project.context("--project is required")?;
    let ground_truth = ground_truth.context("--ground-truth is required")?;

    Ok((project, ground_truth))
}

// ---------------------------------------------------------------------------
// Report formatting
// ---------------------------------------------------------------------------

fn print_report(gt: &GroundTruth, s: &Scores, cmp: &comparator::ComparisonResult) {
    println!("=== Engram Parser Quality Report ===");
    println!(
        "Project: {} ({}) v{}",
        gt.meta.project, gt.meta.language, gt.meta.version
    );
    println!();

    // Summary header.
    println!(
        "Symbols:        {}/{} found ({:.1}%)",
        s.symbols_found, s.symbols_total, s.symbol_recall
    );

    let must_find_suffix = if s.must_find_missed > 0 {
        format!("  [{} must_find missed]", s.must_find_missed)
    } else {
        String::new()
    };
    println!(
        "Relationships:  {}/{} found ({:.1}%){}",
        s.relationships_found, s.relationships_total, s.relationship_recall, must_find_suffix
    );

    if s.imports_total > 0 {
        println!(
            "  Imports:      {}/{} resolved ({:.1}%)",
            s.imports_found, s.imports_total, s.import_resolution
        );
    }

    println!(
        "Impact:         {}/{} queries correct ({:.1}%)",
        s.impact_queries_correct, s.impact_queries_total, s.impact_score
    );
    println!(
        "Edge Cases:     {}/{} passed ({:.1}%)",
        s.edge_cases_passed, s.edge_cases_total, s.edge_case_score
    );

    // Dimension table.
    println!();
    println!(
        "{:<16}{:<8}{:<8}{}",
        "Dimension", "Score", "Weight", "Weighted"
    );
    println!(
        "{:<16}{:<8}{:<8}{}",
        "-----------", "-----", "------", "--------"
    );

    print_row("Sym Recall", s.symbol_recall, 0.15);
    print_row("Sym Precision", s.symbol_precision, 0.15);
    print_row("Rel Recall", s.relationship_recall, 0.15);
    print_row("Rel Precision", s.relationship_precision, 0.10);
    print_row("Import Res.", s.import_resolution, 0.15);
    print_row("Impact", s.impact_score, 0.15);
    print_row("Edge Cases", s.edge_case_score, 0.15);

    println!();
    println!("TOTAL: {:.1} / 100  Grade: {}", s.total, s.grade);

    // Failures section.
    let has_failures = cmp.symbol_results.iter().any(|r| !r.found)
        || cmp.relationship_results.iter().any(|r| !r.found)
        || cmp
            .impact_results
            .iter()
            .any(|r| !missed_in_impact(r).is_empty())
        || cmp.edge_case_results.iter().any(|r| !r.passed);

    if has_failures {
        println!();
        println!("=== Failures ===");

        for r in cmp.symbol_results.iter().filter(|r| !r.found) {
            println!("[MISS] Symbol: {}::{} ({})", r.file, r.name, r.kind);
        }

        for r in cmp.relationship_results.iter().filter(|r| !r.found) {
            println!(
                "[MISS] Relationship: {} --{}--> {} [{}]",
                r.source, r.kind, r.target, r.tier
            );
        }

        for r in &cmp.impact_results {
            let missed = missed_in_impact(r);
            if !missed.is_empty() {
                println!(
                    "[FAIL] Impact: \"What {}s {}?\" - missed: {}",
                    r.direction,
                    r.target,
                    missed.join(", ")
                );
            }
        }

        for r in cmp.edge_case_results.iter().filter(|r| !r.passed) {
            println!("[FAIL] Edge case: {} - {}", r.pattern, r.detail);
        }
    }
}

fn print_row(label: &str, score: f64, weight: f64) {
    let weighted = score * weight;
    println!(
        "{:<16}{:<8}{:<8}{:.1}",
        label,
        format!("{:.1}%", score),
        format!("{:.2}", weight),
        weighted
    );
}

/// Returns expected nodes that were not reached by the BFS.
fn missed_in_impact(r: &ImpactResult) -> Vec<String> {
    r.expected
        .iter()
        .filter(|exp| !r.actual.iter().any(|act| act.ends_with(exp.as_str())))
        .cloned()
        .collect()
}
