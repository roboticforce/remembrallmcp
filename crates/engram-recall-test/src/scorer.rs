//! IR scoring metrics for recall quality evaluation.
//!
//! Implements Recall@K, Precision@K, and MRR.
//! All metrics treat results as an ordered list (position 1 = best).

use uuid::Uuid;

/// Compute Recall@K: fraction of relevant IDs that appear in the top K results.
///
/// recall@K = |relevant ∩ top_K| / |relevant|
pub fn recall_at_k(results: &[Uuid], relevant: &[Uuid], k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0; // vacuously perfect
    }
    let top_k = &results[..k.min(results.len())];
    let hits = relevant
        .iter()
        .filter(|r| top_k.contains(r))
        .count();
    hits as f64 / relevant.len() as f64
}

/// Compute Precision@K: fraction of top K results that are relevant.
///
/// precision@K = |relevant ∩ top_K| / K
pub fn precision_at_k(results: &[Uuid], relevant: &[Uuid], k: usize) -> f64 {
    let top_k = &results[..k.min(results.len())];
    if top_k.is_empty() {
        return if relevant.is_empty() { 1.0 } else { 0.0 };
    }
    let hits = top_k.iter().filter(|r| relevant.contains(r)).count();
    hits as f64 / top_k.len() as f64
}

/// Compute Mean Reciprocal Rank for a single query.
///
/// MRR = 1 / rank_of_first_relevant_result, or 0 if none found.
pub fn mrr(results: &[Uuid], relevant: &[Uuid]) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    results
        .iter()
        .enumerate()
        .find(|(_, id)| relevant.contains(id))
        .map(|(pos, _)| 1.0 / (pos + 1) as f64)
        .unwrap_or(0.0)
}

/// Check that the first relevant result appears within max_rank.
pub fn first_relevant_rank(results: &[Uuid], relevant: &[Uuid]) -> Option<usize> {
    results
        .iter()
        .enumerate()
        .find(|(_, id)| relevant.contains(id))
        .map(|(pos, _)| pos + 1) // 1-indexed
}

/// Verify that IDs in assert_order appear in results in the given relative order.
/// Returns true if all IDs are found and each appears before the next.
pub fn check_rank_order(results: &[Uuid], order: &[Uuid]) -> bool {
    if order.len() < 2 {
        return true;
    }
    let positions: Vec<Option<usize>> = order
        .iter()
        .map(|id| results.iter().position(|r| r == id))
        .collect();
    // All must be found
    if positions.iter().any(|p| p.is_none()) {
        return false;
    }
    let positions: Vec<usize> = positions.into_iter().flatten().collect();
    positions.windows(2).all(|w| w[0] < w[1])
}

/// Check for duplicate IDs in results.
pub fn has_duplicates(results: &[Uuid]) -> bool {
    let mut seen = std::collections::HashSet::new();
    results.iter().any(|id| !seen.insert(id))
}

/// Aggregate scores across all queries.
#[derive(Debug, Default)]
pub struct AggregateScores {
    pub recall_at_5: f64,
    pub precision_at_5: f64,
    pub mrr: f64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub latency_p99_ms: u64,
    pub queries_total: usize,
    pub queries_passed: usize,
    pub queries_failed: usize,
}

impl AggregateScores {
    pub fn compute(per_query: &[QueryScore]) -> Self {
        let n = per_query.len();
        if n == 0 {
            return Self::default();
        }
        let recall_at_5 = per_query.iter().map(|q| q.recall_at_5).sum::<f64>() / n as f64;
        let precision_at_5 = per_query.iter().map(|q| q.precision_at_5).sum::<f64>() / n as f64;
        let mrr = per_query.iter().map(|q| q.mrr).sum::<f64>() / n as f64;

        let passed = per_query.iter().filter(|q| q.passed).count();
        let failed = n - passed;

        // Latency percentiles
        let mut latencies: Vec<u64> = per_query.iter().map(|q| q.latency_ms).collect();
        latencies.sort_unstable();
        let p50 = percentile(&latencies, 50);
        let p95 = percentile(&latencies, 95);
        let p99 = percentile(&latencies, 99);

        Self {
            recall_at_5,
            precision_at_5,
            mrr,
            latency_p50_ms: p50,
            latency_p95_ms: p95,
            latency_p99_ms: p99,
            queries_total: n,
            queries_passed: passed,
            queries_failed: failed,
        }
    }

    pub fn pass_rate(&self) -> f64 {
        if self.queries_total == 0 {
            return 100.0;
        }
        self.queries_passed as f64 / self.queries_total as f64 * 100.0
    }
}

#[derive(Debug)]
pub struct QueryScore {
    pub query_id: String,
    pub recall_at_5: f64,
    pub precision_at_5: f64,
    pub mrr: f64,
    pub latency_ms: u64,
    pub passed: bool,
    pub failure_reasons: Vec<String>,
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p as f64 / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
