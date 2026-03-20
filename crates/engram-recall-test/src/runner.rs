//! Query runner: executes each ground-truth query against the live database
//! and produces a QueryScore.

use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use engram_core::embed::Embedder;
use engram_core::memory::store::MemoryStore;
use engram_core::memory::types::{MemoryQuery, MemoryType, Scope};

use crate::ground_truth::QueryCase;
use crate::scorer::{self, QueryScore};

/// Execute one ground-truth query and score the results.
pub async fn run_query(
    case: &QueryCase,
    store: &MemoryStore,
    embedder: &dyn Embedder,
) -> Result<QueryScore> {
    let t0 = Instant::now();

    // Convert filter strings to MemoryType values (ignore unknown)
    let memory_types: Option<Vec<MemoryType>> = if case.filters.memory_types.is_empty() {
        None
    } else {
        let parsed: Vec<MemoryType> = case
            .filters
            .memory_types
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        Some(parsed)
    };

    let scope: Option<Scope> = if case.filters.project.is_some()
        || case.filters.team.is_some()
    {
        Some(Scope {
            organization: None,
            team: case.filters.team.clone(),
            project: case.filters.project.clone(),
        })
    } else {
        None
    };

    let tags: Option<Vec<String>> = if case.filters.tags.is_empty() {
        None
    } else {
        Some(case.filters.tags.clone())
    };

    let mq = MemoryQuery {
        query: case.query.clone(),
        memory_types,
        scope,
        tags,
        limit: Some(20),
        min_similarity: Some(0.0),
    };

    // Generate query embedding
    let embedding = engram_core::tokio_block_on_embed(embedder, &case.query)?;

    // Execute hybrid search
    let search_result = store.search_hybrid(embedding, &mq).await;

    let elapsed_ms = t0.elapsed().as_millis() as u64;

    // --- Evaluate result ---
    let mut failure_reasons: Vec<String> = Vec::new();

    match &search_result {
        Err(e) => {
            if case.assert_no_error {
                failure_reasons.push(format!("search returned error: {e}"));
            }
            return Ok(QueryScore {
                query_id: case.id.clone(),
                recall_at_5: 0.0,
                precision_at_5: 0.0,
                mrr: 0.0,
                latency_ms: elapsed_ms,
                passed: false,
                failure_reasons,
            });
        }
        Ok(results) => {
            let result_ids: Vec<Uuid> = results.iter().map(|r| r.memory.id).collect();

            // Assert no duplicates
            if case.assert_no_duplicate_ids && scorer::has_duplicates(&result_ids) {
                failure_reasons.push("duplicate memory IDs in results".into());
            }

            // Assert empty
            if case.expect_empty && !result_ids.is_empty() {
                failure_reasons.push(format!(
                    "expected empty results but got {} entries",
                    result_ids.len()
                ));
            }

            // Max rank assertion
            if !case.relevant_ids.is_empty() && !case.expect_empty {
                match scorer::first_relevant_rank(&result_ids, &case.relevant_ids) {
                    None => {
                        failure_reasons.push(format!(
                            "no relevant result in top {} (relevant: {:?})",
                            result_ids.len(),
                            &case.relevant_ids[..1]
                        ));
                    }
                    Some(rank) if rank > case.max_rank => {
                        failure_reasons.push(format!(
                            "first relevant result at rank {} > max_rank {}",
                            rank, case.max_rank
                        ));
                    }
                    _ => {}
                }
            }

            // Rank order assertion
            if !case.assert_rank_order.is_empty() {
                if !scorer::check_rank_order(&result_ids, &case.assert_rank_order) {
                    failure_reasons.push(format!(
                        "rank order violated: expected {:?} in order",
                        &case.assert_rank_order
                    ));
                }
            }

            // False-positive threshold (for no-results queries)
            if let Some(threshold) = case.min_score_threshold {
                if let Some(top) = results.first() {
                    if top.score >= threshold {
                        failure_reasons.push(format!(
                            "false positive: top score {:.3} >= threshold {:.3}",
                            top.score, threshold
                        ));
                    }
                }
            }

            // Latency budget
            if let Some(budget) = case.latency_budget_ms {
                if elapsed_ms > budget {
                    failure_reasons.push(format!(
                        "latency {}ms exceeds budget {}ms",
                        elapsed_ms, budget
                    ));
                }
            }

            let r5 = scorer::recall_at_k(&result_ids, &case.relevant_ids, 5);
            let p5 = scorer::precision_at_k(&result_ids, &case.relevant_ids, 5);
            let m = scorer::mrr(&result_ids, &case.relevant_ids);

            Ok(QueryScore {
                query_id: case.id.clone(),
                recall_at_5: r5,
                precision_at_5: p5,
                mrr: m,
                latency_ms: elapsed_ms,
                passed: failure_reasons.is_empty(),
                failure_reasons,
            })
        }
    }
}
