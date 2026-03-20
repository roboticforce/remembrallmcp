//! Serde types for the recall ground truth TOML files.

use serde::Deserialize;
use uuid::Uuid;

/// Top-level document.
#[derive(Debug, Deserialize)]
pub struct GroundTruth {
    pub queries: Vec<QueryCase>,
}

/// One test query with expected results.
#[derive(Debug, Deserialize)]
pub struct QueryCase {
    pub id: String,
    pub description: String,
    pub query: String,
    pub category: String,
    /// "semantic", "fulltext", or "hybrid" - documents the expected dominant search path.
    /// Informational only; the harness always runs full hybrid search.
    #[allow(dead_code)]
    pub method: String,
    /// Ordered list of memory IDs that are correct results.
    #[serde(default)]
    pub relevant_ids: Vec<Uuid>,
    /// Result must appear within this rank position. 999 = don't care.
    #[serde(default = "default_max_rank")]
    pub max_rank: usize,
    /// If true, the result set must be empty.
    #[serde(default)]
    pub expect_empty: bool,
    /// If true, assert no duplicate IDs in results.
    #[serde(default)]
    pub assert_no_duplicate_ids: bool,
    /// If true, the query must complete without a system error.
    #[serde(default = "default_true")]
    pub assert_no_error: bool,
    /// Optional ordered IDs that must appear in this relative order.
    #[serde(default)]
    pub assert_rank_order: Vec<Uuid>,
    /// Optional score threshold: no result should have score >= this value (false-positive guard).
    pub min_score_threshold: Option<f32>,
    /// Expected latency budget in milliseconds. None means no latency assertion.
    pub latency_budget_ms: Option<u64>,
    /// Optional query-time filters.
    #[serde(default)]
    pub filters: QueryFilters,
}

#[derive(Debug, Deserialize, Default)]
pub struct QueryFilters {
    #[serde(default)]
    pub memory_types: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub team: Option<String>,
}

fn default_max_rank() -> usize {
    999
}

fn default_true() -> bool {
    true
}
