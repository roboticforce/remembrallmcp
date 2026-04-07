//! Query runner: executes each ground-truth query against the live database
//! and produces a QueryScore.

use std::time::Instant;

use anyhow::Result;
use uuid::Uuid;

use remembrall_core::embed::Embedder;
use remembrall_core::memory::store::MemoryStore;
use remembrall_core::memory::types::{MatchType, MemoryQuery, MemorySearchResult, MemoryType, Scope};

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
    let embedding = remembrall_core::tokio_block_on_embed(embedder, &case.query)?;

    let search_result: Result<Vec<MemorySearchResult>> = match case.method.as_str() {
        "semantic" => run_semantic_query(store, &embedding, &mq).await,
        "fulltext" => run_fulltext_query(store, &mq).await,
        _ => store.search_hybrid(embedding, &mq).await.map_err(Into::into),
    };

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

async fn run_semantic_query(
    store: &MemoryStore,
    embedding: &[f32],
    query: &MemoryQuery,
) -> Result<Vec<MemorySearchResult>> {
    let rows = store
        .search_semantic(
            embedding.to_vec(),
            query.limit.unwrap_or(20),
            query.min_similarity.unwrap_or(0.0) as f64,
            query.scope.as_ref(),
        )
        .await?;

    hydrate_and_filter_results(store, rows, query, MatchType::Semantic).await
}

async fn run_fulltext_query(
    store: &MemoryStore,
    query: &MemoryQuery,
) -> Result<Vec<MemorySearchResult>> {
    let rows = store
        .search_fulltext(&query.query, query.limit.unwrap_or(20))
        .await?;

    hydrate_and_filter_results(store, rows, query, MatchType::FullText).await
}

async fn hydrate_and_filter_results(
    store: &MemoryStore,
    rows: Vec<(Uuid, f64)>,
    query: &MemoryQuery,
    match_type: MatchType,
) -> Result<Vec<MemorySearchResult>> {
    let mut results = Vec::new();

    for (id, score) in rows {
        let memory = match store.get_readonly(id).await {
            Ok(memory) => memory,
            Err(_) => continue,
        };

        if let Some(ref types) = query.memory_types {
            if !types.iter().any(|t| t.to_string() == memory.memory_type.to_string()) {
                continue;
            }
        }

        if let Some(ref req_tags) = query.tags {
            if !req_tags.iter().all(|t| memory.tags.contains(t)) {
                continue;
            }
        }

        if let Some(ref scope) = query.scope {
            if scope.project.is_some() && scope.project != memory.scope.project {
                continue;
            }
            if scope.team.is_some() && scope.team != memory.scope.team {
                continue;
            }
            if scope.organization.is_some() && scope.organization != memory.scope.organization {
                continue;
            }
        }

        results.push(MemorySearchResult {
            memory,
            score: score as f32,
            match_type: match_type.clone(),
        });
    }

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(query.limit.unwrap_or(20) as usize);

    Ok(results)
}
