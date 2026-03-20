use chrono::Utc;
use pgvector::Vector;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{EngramError, Result};
use super::types::*;

/// Core memory storage engine backed by Postgres + pgvector.
pub struct MemoryStore {
    pool: PgPool,
    schema: String,
}

impl MemoryStore {
    pub fn new(pool: PgPool, schema: String) -> Self {
        Self { pool, schema }
    }

    /// Initialize database schema (tables, indexes, extensions).
    pub async fn init(&self) -> Result<()> {
        // Enable pgvector extension
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await?;

        // Create schema
        sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {}", self.schema))
            .execute(&self.pool)
            .await?;

        // Memory entries table
        sqlx::query(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS {schema}.memories (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                content TEXT NOT NULL,
                summary TEXT,
                memory_type TEXT NOT NULL,
                source_system TEXT NOT NULL,
                source_identifier TEXT NOT NULL,
                source_author TEXT,
                scope_organization TEXT,
                scope_team TEXT,
                scope_project TEXT,
                tags TEXT[] DEFAULT '{{}}',
                metadata JSONB DEFAULT '{{}}',
                importance REAL DEFAULT 0.5,
                access_count INTEGER DEFAULT 0,
                content_fingerprint TEXT NOT NULL,
                embedding vector(384),
                created_at TIMESTAMPTZ DEFAULT NOW(),
                updated_at TIMESTAMPTZ DEFAULT NOW(),
                last_accessed_at TIMESTAMPTZ,
                expires_at TIMESTAMPTZ
            )
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        // Indexes
        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memories_type ON {schema}.memories (memory_type);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memories_scope ON {schema}.memories (scope_organization, scope_team, scope_project);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memories_tags ON {schema}.memories USING GIN (tags);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memories_fingerprint ON {schema}.memories (content_fingerprint);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        // HNSW index for fast vector similarity search
        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memories_embedding ON {schema}.memories
            USING hnsw (embedding vector_cosine_ops) WITH (m = 16, ef_construction = 64);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        // Full-text search index
        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_memories_fts ON {schema}.memories
            USING GIN (to_tsvector('english', coalesce(content, '') || ' ' || coalesce(summary, '')));
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        tracing::info!("Memory store initialized in schema '{}'", self.schema);
        Ok(())
    }

    /// Store a new memory. Returns the ID.
    pub async fn store(&self, input: CreateMemory, embedding: Vec<f32>) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let fingerprint = compute_fingerprint(&input.content);
        let memory_type = input.memory_type.to_string();
        let metadata = input.metadata.unwrap_or(serde_json::Value::Object(Default::default()));
        let importance = input.importance.unwrap_or(0.5);
        let vec = Vector::from(embedding);

        sqlx::query(&format!(
            r#"
            INSERT INTO {schema}.memories
                (id, content, summary, memory_type, source_system, source_identifier,
                 source_author, scope_organization, scope_team, scope_project,
                 tags, metadata, importance, content_fingerprint, embedding, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            "#,
            schema = self.schema,
        ))
        .bind(id)
        .bind(&input.content)
        .bind(&input.summary)
        .bind(&memory_type)
        .bind(&input.source.system)
        .bind(&input.source.identifier)
        .bind(&input.source.author)
        .bind(&input.scope.organization)
        .bind(&input.scope.team)
        .bind(&input.scope.project)
        .bind(&input.tags)
        .bind(&metadata)
        .bind(importance)
        .bind(&fingerprint)
        .bind(&vec)
        .bind(&input.expires_at)
        .execute(&self.pool)
        .await?;

        tracing::debug!("Stored memory {id} type={memory_type}");
        Ok(id)
    }

    /// Semantic search using pgvector cosine similarity.
    pub async fn search_semantic(
        &self,
        embedding: Vec<f32>,
        limit: i64,
        min_similarity: f64,
        scope: Option<&Scope>,
    ) -> Result<Vec<(Uuid, f64)>> {
        let vec = Vector::from(embedding);

        // Build scope filter
        let (scope_clause, org, team, project) = match scope {
            Some(s) => {
                let mut clause = String::new();
                if s.organization.is_some() {
                    clause.push_str(" AND scope_organization = $3");
                }
                if s.team.is_some() {
                    clause.push_str(" AND scope_team = $4");
                }
                if s.project.is_some() {
                    clause.push_str(" AND scope_project = $5");
                }
                (clause, s.organization.clone(), s.team.clone(), s.project.clone())
            }
            None => (String::new(), None, None, None),
        };

        let sql = format!(
            r#"
            SELECT id, 1 - (embedding <=> $1) AS similarity
            FROM {schema}.memories
            WHERE embedding IS NOT NULL
            AND 1 - (embedding <=> $1) >= $2
            {scope_clause}
            AND (expires_at IS NULL OR expires_at > NOW())
            ORDER BY embedding <=> $1
            LIMIT {limit}
            "#,
            schema = self.schema,
        );

        let rows = sqlx::query_as::<_, (Uuid, f64)>(&sql)
            .bind(&vec)
            .bind(min_similarity)
            .bind(&org)
            .bind(&team)
            .bind(&project)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows)
    }

    /// Full-text search using Postgres tsvector.
    pub async fn search_fulltext(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<(Uuid, f64)>> {
        let sql = format!(
            r#"
            SELECT id,
                   ts_rank(
                       to_tsvector('english', coalesce(content, '') || ' ' || coalesce(summary, '')),
                       plainto_tsquery('english', $1)
                   )::float8 AS rank
            FROM {schema}.memories
            WHERE to_tsvector('english', coalesce(content, '') || ' ' || coalesce(summary, ''))
                  @@ plainto_tsquery('english', $1)
            AND (expires_at IS NULL OR expires_at > NOW())
            ORDER BY rank DESC
            LIMIT $2
            "#,
            schema = self.schema,
        );

        let rows = sqlx::query_as::<_, (Uuid, f64)>(&sql)
            .bind(query)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows)
    }

    /// Hybrid search: combines semantic + full-text results using Reciprocal Rank Fusion.
    ///
    /// RRF score = Σ 1 / (k + rank_i) across all result lists.
    /// k=60 is the standard constant from the original RRF paper (Cormack et al. 2009).
    ///
    /// Results are deduplicated: each memory ID appears at most once.
    /// Applies type, tag, and scope filters. Expired memories are excluded by each
    /// sub-search query. The returned list is sorted descending by RRF score.
    pub async fn search_hybrid(
        &self,
        embedding: Vec<f32>,
        query: &MemoryQuery,
    ) -> Result<Vec<crate::memory::types::MemorySearchResult>> {
        use std::collections::HashMap;
        use crate::memory::types::{MatchType, MemorySearchResult};

        const K: f64 = 60.0;
        let limit = query.limit.unwrap_or(10);
        let min_similarity = query.min_similarity.unwrap_or(0.0) as f64;

        // --- Semantic results ---
        let sem_raw = self
            .search_semantic(embedding, limit * 2, min_similarity, query.scope.as_ref())
            .await?;

        // --- Full-text results ---
        let ft_raw = self.search_fulltext(&query.query, limit * 2).await?;

        // --- RRF fusion ---
        // Map: memory_id -> (rrf_score, best_match_type)
        let mut rrf: HashMap<uuid::Uuid, (f64, MatchType)> = HashMap::new();

        for (rank, (id, _score)) in sem_raw.iter().enumerate() {
            let entry = rrf.entry(*id).or_insert((0.0, MatchType::Semantic));
            entry.0 += 1.0 / (K + rank as f64 + 1.0);
        }

        for (rank, (id, _score)) in ft_raw.iter().enumerate() {
            let entry = rrf.entry(*id).or_insert((0.0, MatchType::FullText));
            if matches!(entry.1, MatchType::Semantic) {
                entry.1 = MatchType::Hybrid; // found by both
            }
            entry.0 += 1.0 / (K + rank as f64 + 1.0);
        }

        // Sort by RRF score descending
        let mut ranked: Vec<(uuid::Uuid, f64, MatchType)> = rrf
            .into_iter()
            .map(|(id, (score, mt))| (id, score, mt))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(limit as usize);

        // --- Apply post-filters (type, tags) and hydrate ---
        let mut results = Vec::new();
        for (id, rrf_score, match_type) in ranked {
            // Fetch the full memory record (also bumps access_count)
            let memory = match self.get(id).await {
                Ok(m) => m,
                Err(_) => continue, // row may have been deleted
            };

            // Filter by memory_type
            if let Some(ref types) = query.memory_types {
                if !types.iter().any(|t| t.to_string() == memory.memory_type.to_string()) {
                    continue;
                }
            }

            // Filter by tags (all requested tags must be present)
            if let Some(ref req_tags) = query.tags {
                if !req_tags.iter().all(|t| memory.tags.contains(t)) {
                    continue;
                }
            }

            results.push(MemorySearchResult {
                memory,
                score: rrf_score as f32,
                match_type,
            });
        }

        Ok(results)
    }

    /// Get a memory by ID. Increments access count.
    pub async fn get(&self, id: Uuid) -> Result<Memory> {
        let row = sqlx::query_as::<_, MemoryRow>(&format!(
            r#"
            UPDATE {schema}.memories
            SET access_count = access_count + 1, last_accessed_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
            schema = self.schema,
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| EngramError::NotFound(format!("Memory {id}")))?;

        Ok(row.into_memory())
    }

    /// Delete a memory by ID.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let result = sqlx::query(&format!(
            "DELETE FROM {schema}.memories WHERE id = $1",
            schema = self.schema,
        ))
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Check for duplicate by content fingerprint.
    pub async fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Uuid>> {
        let row = sqlx::query_as::<_, (Uuid,)>(&format!(
            "SELECT id FROM {schema}.memories WHERE content_fingerprint = $1 LIMIT 1",
            schema = self.schema,
        ))
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id,)| id))
    }

    /// Count all memories, optionally filtered by scope.
    pub async fn count(&self, scope: Option<&Scope>) -> Result<i64> {
        let sql = match scope {
            Some(s) => {
                let mut where_clauses = vec![];
                if s.organization.is_some() {
                    where_clauses.push("scope_organization = $1".to_string());
                }
                if s.team.is_some() {
                    where_clauses.push("scope_team = $2".to_string());
                }
                if s.project.is_some() {
                    where_clauses.push("scope_project = $3".to_string());
                }
                if where_clauses.is_empty() {
                    format!("SELECT COUNT(*) FROM {schema}.memories", schema = self.schema)
                } else {
                    format!(
                        "SELECT COUNT(*) FROM {schema}.memories WHERE {clauses}",
                        schema = self.schema,
                        clauses = where_clauses.join(" AND ")
                    )
                }
            }
            None => format!("SELECT COUNT(*) FROM {schema}.memories", schema = self.schema),
        };

        let (count,) = sqlx::query_as::<_, (i64,)>(&sql)
            .fetch_one(&self.pool)
            .await?;

        Ok(count)
    }
}

/// Internal row type for sqlx deserialization.
#[derive(sqlx::FromRow)]
struct MemoryRow {
    id: Uuid,
    content: String,
    summary: Option<String>,
    memory_type: String,
    source_system: String,
    source_identifier: String,
    source_author: Option<String>,
    scope_organization: Option<String>,
    scope_team: Option<String>,
    scope_project: Option<String>,
    tags: Vec<String>,
    metadata: serde_json::Value,
    importance: f32,
    access_count: i32,
    content_fingerprint: String,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    last_accessed_at: Option<chrono::DateTime<Utc>>,
    expires_at: Option<chrono::DateTime<Utc>>,
}

impl MemoryRow {
    fn into_memory(self) -> Memory {
        Memory {
            id: self.id,
            content: self.content,
            summary: self.summary,
            memory_type: self.memory_type.parse().unwrap_or(MemoryType::Decision),
            source: Source {
                system: self.source_system,
                identifier: self.source_identifier,
                author: self.source_author,
            },
            scope: Scope {
                organization: self.scope_organization,
                team: self.scope_team,
                project: self.scope_project,
            },
            tags: self.tags,
            metadata: self.metadata,
            importance: self.importance,
            access_count: self.access_count,
            content_fingerprint: self.content_fingerprint,
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_accessed_at: self.last_accessed_at,
            expires_at: self.expires_at,
        }
    }
}

/// Compute a fingerprint for deduplication.
pub fn compute_fingerprint_pub(content: &str) -> String {
    compute_fingerprint(content)
}

fn compute_fingerprint(content: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    // Normalize: lowercase, collapse whitespace
    let normalized: String = content
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
