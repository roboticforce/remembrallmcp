//! Loads seed memories from TOML and inserts them into the test database.
//!
//! The seed TOML uses stable UUIDs so ground truth queries can reference them.
//! The `null_embedding` flag skips embedding generation, leaving the vector NULL
//! so we can test full-text-only fallback paths.
//! The `expires_days_ago` field inserts a memory that is already expired.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use engram_core::embed::Embedder;
use engram_core::memory::store::MemoryStore;
use engram_core::memory::types::{CreateMemory, MemoryType, Scope, Source};

// ---------------------------------------------------------------------------
// TOML types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SeedFile {
    pub memories: Vec<SeedMemory>,
}

#[derive(Debug, Deserialize)]
pub struct SeedMemory {
    pub id: Uuid,
    pub content: String,
    pub summary: Option<String>,
    pub memory_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub organization: String,
    pub importance: f32,
    /// How many days ago this was created (used to set created_at for recency tests).
    pub created_days_ago: i64,
    /// If set, the memory expires this many days in the past (i.e., already expired).
    pub expires_days_ago: Option<i64>,
    /// If true, insert without an embedding vector (NULL embedding).
    #[serde(default)]
    pub null_embedding: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Inserts all seed memories and returns a map of seed ID -> inserted UUID.
///
/// In most cases the inserted UUID matches the seed ID because we use `INSERT ...
/// ON CONFLICT DO NOTHING` with the provided ID. The returned map lets the harness
/// correlate ground truth IDs with database rows even if the implementation changes.
pub async fn seed(
    _store: &MemoryStore,
    pool: &sqlx::PgPool,
    schema: &str,
    embedder: &dyn Embedder,
    seed_file: &SeedFile,
) -> Result<HashMap<Uuid, Uuid>> {
    let mut id_map = HashMap::new();
    let now = Utc::now();

    // Batch embed all non-null-embedding memories for efficiency.
    let texts_to_embed: Vec<(usize, &str)> = seed_file
        .memories
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.null_embedding)
        .map(|(i, m)| (i, m.content.as_str()))
        .collect();

    let texts: Vec<&str> = texts_to_embed.iter().map(|(_, t)| *t).collect();
    let embeddings = if texts.is_empty() {
        vec![]
    } else {
        embedder
            .embed_batch(&texts)
            .context("batch embedding seed memories")?
    };

    // Build an index map: original memory index -> embedding
    let mut embedding_by_mem_idx: HashMap<usize, Vec<f32>> = HashMap::new();
    for (batch_pos, (mem_idx, _)) in texts_to_embed.iter().enumerate() {
        embedding_by_mem_idx.insert(*mem_idx, embeddings[batch_pos].clone());
    }

    for (idx, seed_mem) in seed_file.memories.iter().enumerate() {
        let memory_type: MemoryType = seed_mem
            .memory_type
            .parse()
            .map_err(|e| anyhow::anyhow!("parsing memory_type for seed ID {}: {}", seed_mem.id, e))?;

        let expires_at = seed_mem.expires_days_ago.map(|d| now - Duration::days(d));
        let created_at = now - Duration::days(seed_mem.created_days_ago);

        let input = CreateMemory {
            content: seed_mem.content.clone(),
            summary: seed_mem.summary.clone(),
            memory_type,
            source: Source {
                system: "test-harness".to_string(),
                identifier: seed_mem.id.to_string(),
                author: None,
            },
            scope: Scope {
                organization: if seed_mem.organization.is_empty() {
                    None
                } else {
                    Some(seed_mem.organization.clone())
                },
                team: if seed_mem.team.is_empty() {
                    None
                } else {
                    Some(seed_mem.team.clone())
                },
                project: if seed_mem.project.is_empty() {
                    None
                } else {
                    Some(seed_mem.project.clone())
                },
            },
            tags: seed_mem.tags.clone(),
            metadata: None,
            importance: Some(seed_mem.importance),
            expires_at,
        };

        let embedding = if seed_mem.null_embedding {
            vec![] // store() with empty vec will be treated as NULL below
        } else {
            embedding_by_mem_idx
                .get(&idx)
                .cloned()
                .unwrap_or_default()
        };

        // Insert with stable ID. We bypass the normal store() to use our fixed UUID
        // and to set created_at accurately for recency tests.
        insert_with_fixed_id(pool, schema, seed_mem, &input, &embedding, created_at, expires_at)
            .await
            .with_context(|| format!("inserting seed memory {}", seed_mem.id))?;

        id_map.insert(seed_mem.id, seed_mem.id);
        tracing::debug!("seeded memory {} ({})", seed_mem.id, seed_mem.memory_type);
    }

    Ok(id_map)
}

/// Insert a memory with a fixed UUID and controlled timestamps.
/// Uses INSERT ... ON CONFLICT (id) DO NOTHING so re-runs are idempotent.
async fn insert_with_fixed_id(
    pool: &sqlx::PgPool,
    schema: &str,
    seed: &SeedMemory,
    input: &CreateMemory,
    embedding: &[f32],
    created_at: chrono::DateTime<Utc>,
    expires_at: Option<chrono::DateTime<Utc>>,
) -> Result<()> {
    use pgvector::Vector;

    let memory_type = input.memory_type.to_string();
    let metadata = serde_json::Value::Object(Default::default());
    let fingerprint = compute_fingerprint(&input.content);

    // NULL embedding when the slice is empty.
    let vec_opt: Option<Vector> = if embedding.is_empty() {
        None
    } else {
        Some(Vector::from(embedding.to_vec()))
    };

    sqlx::query(&format!(
        r#"
        INSERT INTO {schema}.memories
            (id, content, summary, memory_type, source_system, source_identifier,
             source_author, scope_organization, scope_team, scope_project,
             tags, metadata, importance, content_fingerprint, embedding,
             created_at, updated_at, expires_at)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$16,$17)
        ON CONFLICT (id) DO NOTHING
        "#,
        schema = schema,
    ))
    .bind(seed.id)
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
    .bind(input.importance.unwrap_or(0.5))
    .bind(&fingerprint)
    .bind(&vec_opt)
    .bind(created_at)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(())
}

fn compute_fingerprint(content: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    let normalized: String = content
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
