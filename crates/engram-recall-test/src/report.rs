//! Human-readable report printer for the recall test harness.

use crate::ground_truth::GroundTruth;
use crate::scorer::{AggregateScores, QueryScore};

pub fn print_report(gt: &GroundTruth, per_query: &[QueryScore], agg: &AggregateScores) {
    println!();
    println!("=== Engram Recall Quality Report ===");
    println!(
        "Queries: {}  Passed: {}  Failed: {}  Pass rate: {:.1}%",
        agg.queries_total, agg.queries_passed, agg.queries_failed, agg.pass_rate()
    );
    println!();

    // Per-category breakdown
    println!("{:<6} {:<8} {:<8} {:<8} {:<10} {}", "QID", "R@5", "P@5", "MRR", "Lat(ms)", "Status");
    println!("{}", "-".repeat(70));

    for qs in per_query {
        let case = gt.queries.iter().find(|q| q.id == qs.query_id);
        let category = case.map(|c| c.category.as_str()).unwrap_or("?");
        let status = if qs.passed {
            "PASS".to_string()
        } else {
            format!("FAIL  {}", qs.failure_reasons.join("; "))
        };
        println!(
            "{:<6} {:<8} {:<8} {:<8} {:<10} {}",
            format!("[{}]{}", category, qs.query_id),
            format!("{:.2}", qs.recall_at_5),
            format!("{:.2}", qs.precision_at_5),
            format!("{:.2}", qs.mrr),
            qs.latency_ms,
            status,
        );
    }

    println!();
    println!("--- Aggregate Metrics ---");
    println!(
        "Mean Recall@5:    {:.3}  (target >= 0.80)",
        agg.recall_at_5
    );
    println!(
        "Mean Precision@5: {:.3}  (target >= 0.60)",
        agg.precision_at_5
    );
    println!(
        "Mean MRR:         {:.3}  (target >= 0.70)",
        agg.mrr
    );
    println!();
    println!("--- Latency ---");
    println!("p50: {}ms  p95: {}ms  p99: {}ms  (budget: 50ms at p95)", agg.latency_p50_ms, agg.latency_p95_ms, agg.latency_p99_ms);
    println!();

    // Per-category summary
    print_category_summary(gt, per_query);

    println!();
    let grade = grade(agg);
    println!("GRADE: {}  (pass rate {:.1}%, MRR {:.2})", grade, agg.pass_rate(), agg.mrr);
}

fn print_category_summary(gt: &GroundTruth, per_query: &[QueryScore]) {
    let categories = ["A", "B", "C", "D", "E", "F"];
    let descriptions = [
        "A - Basic Search Quality",
        "B - Hybrid Search Effectiveness",
        "C - Filtering",
        "D - Edge Cases",
        "E - Ranking Quality",
        "F - Performance",
    ];

    println!("--- Category Summary ---");
    for (cat, desc) in categories.iter().zip(descriptions.iter()) {
        let cat_queries: Vec<&QueryScore> = per_query
            .iter()
            .filter(|qs| {
                gt.queries
                    .iter()
                    .any(|q| q.id == qs.query_id && q.category == *cat)
            })
            .collect();
        if cat_queries.is_empty() {
            continue;
        }
        let passed = cat_queries.iter().filter(|q| q.passed).count();
        let total = cat_queries.len();
        println!("  {}: {}/{} passed", desc, passed, total);
    }
}

fn grade(agg: &AggregateScores) -> &'static str {
    // Use a simple composite: weight pass rate 50%, MRR 30%, R@5 20%
    let score = agg.pass_rate() * 0.5
        + agg.mrr * 100.0 * 0.3
        + agg.recall_at_5 * 100.0 * 0.2;
    if score >= 90.0 {
        "A"
    } else if score >= 80.0 {
        "B"
    } else if score >= 70.0 {
        "C"
    } else if score >= 60.0 {
        "D"
    } else {
        "F"
    }
}
