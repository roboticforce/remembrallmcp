use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;
use super::types::*;

/// Code graph storage backed by Postgres adjacency tables + recursive CTEs.
pub struct GraphStore {
    pool: PgPool,
    schema: String,
}

impl GraphStore {
    pub fn new(pool: PgPool, schema: String) -> Self {
        Self { pool, schema }
    }

    /// Initialize graph tables.
    pub async fn init(&self) -> Result<()> {
        // Ensure schema exists
        sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {}", self.schema))
            .execute(&self.pool)
            .await?;

        sqlx::query(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS {schema}.symbols (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                name TEXT NOT NULL,
                symbol_type TEXT NOT NULL,
                file_path TEXT NOT NULL,
                start_line INTEGER,
                end_line INTEGER,
                language TEXT NOT NULL,
                project TEXT NOT NULL,
                signature TEXT,
                file_mtime TIMESTAMPTZ NOT NULL,
                created_at TIMESTAMPTZ DEFAULT NOW()
            )
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS {schema}.relationships (
                source_id UUID NOT NULL REFERENCES {schema}.symbols(id) ON DELETE CASCADE,
                target_id UUID NOT NULL REFERENCES {schema}.symbols(id) ON DELETE CASCADE,
                rel_type TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 1.0,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                PRIMARY KEY (source_id, target_id, rel_type)
            )
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        // Indexes for fast traversal
        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON {schema}.symbols (file_path);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON {schema}.symbols (name, symbol_type);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_relationships_source ON {schema}.relationships (source_id);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_relationships_target ON {schema}.relationships (target_id);
            "#,
            schema = self.schema,
        ))
        .execute(&self.pool)
        .await?;

        tracing::info!("Graph store initialized in schema '{}'", self.schema);
        Ok(())
    }

    /// Insert or update a symbol. Returns the ID.
    pub async fn upsert_symbol(&self, symbol: &Symbol) -> Result<Uuid> {
        let symbol_type = symbol.symbol_type.to_string();

        let (id,) = sqlx::query_as::<_, (Uuid,)>(&format!(
            r#"
            INSERT INTO {schema}.symbols
                (id, name, symbol_type, file_path, start_line, end_line, language, project, signature, file_mtime)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                symbol_type = EXCLUDED.symbol_type,
                start_line = EXCLUDED.start_line,
                end_line = EXCLUDED.end_line,
                signature = EXCLUDED.signature,
                file_mtime = EXCLUDED.file_mtime
            RETURNING id
            "#,
            schema = self.schema,
        ))
        .bind(symbol.id)
        .bind(&symbol.name)
        .bind(&symbol_type)
        .bind(&symbol.file_path)
        .bind(symbol.start_line)
        .bind(symbol.end_line)
        .bind(&symbol.language)
        .bind(&symbol.project)
        .bind(&symbol.signature)
        .bind(symbol.file_mtime)
        .fetch_one(&self.pool)
        .await?;

        Ok(id)
    }

    /// Add a relationship between two symbols.
    pub async fn add_relationship(&self, rel: &Relationship) -> Result<()> {
        let rel_type = rel.rel_type.to_string();

        sqlx::query(&format!(
            r#"
            INSERT INTO {schema}.relationships (source_id, target_id, rel_type, confidence)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (source_id, target_id, rel_type) DO UPDATE SET
                confidence = EXCLUDED.confidence
            "#,
            schema = self.schema,
        ))
        .bind(rel.source_id)
        .bind(rel.target_id)
        .bind(&rel_type)
        .bind(rel.confidence)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Impact analysis: find all symbols affected by a change, using recursive CTE.
    /// This is the killer feature - "what breaks if I change this function?"
    pub async fn impact_analysis(
        &self,
        symbol_id: Uuid,
        direction: Direction,
        max_depth: i32,
    ) -> Result<Vec<ImpactResult>> {
        if let Direction::Both = direction {
            let mut upstream = Box::pin(self.impact_analysis(symbol_id, Direction::Upstream, max_depth)).await?;
            let downstream = Box::pin(self.impact_analysis(symbol_id, Direction::Downstream, max_depth)).await?;
            upstream.extend(downstream);
            return Ok(upstream);
        }

        let (join_col, match_col) = match direction {
            Direction::Upstream => ("target_id", "source_id"),
            Direction::Downstream => ("source_id", "target_id"),
            Direction::Both => unreachable!(),
        };

        let sql = format!(
            r#"
            WITH RECURSIVE impact AS (
                -- Base case: direct relationships
                SELECT
                    r.{match_col} AS symbol_id,
                    r.rel_type,
                    r.confidence,
                    1 AS depth,
                    ARRAY[r.{join_col}, r.{match_col}] AS path
                FROM {schema}.relationships r
                WHERE r.{join_col} = $1

                UNION ALL

                -- Recursive: follow the chain
                SELECT
                    r.{match_col} AS symbol_id,
                    r.rel_type,
                    r.confidence * i.confidence AS confidence,
                    i.depth + 1 AS depth,
                    i.path || r.{match_col} AS path
                FROM {schema}.relationships r
                JOIN impact i ON r.{join_col} = i.symbol_id
                WHERE i.depth < $2
                AND NOT r.{match_col} = ANY(i.path)  -- prevent cycles
            )
            SELECT
                s.id, s.name, s.symbol_type, s.file_path, s.start_line, s.end_line,
                s.language, s.project, s.signature, s.file_mtime,
                i.depth, i.path, i.rel_type, i.confidence
            FROM impact i
            JOIN {schema}.symbols s ON s.id = i.symbol_id
            ORDER BY i.depth ASC, i.confidence DESC
            "#,
            schema = self.schema,
        );

        let rows = sqlx::query_as::<_, ImpactRow>(&sql)
            .bind(symbol_id)
            .bind(max_depth)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(|r| r.into_impact_result()).collect())
    }

    /// Find a symbol by name, optionally filtered by type and/or project.
    pub async fn find_symbol(
        &self,
        name: &str,
        symbol_type: Option<&SymbolType>,
        project: Option<&str>,
    ) -> Result<Vec<Symbol>> {
        let type_filter = symbol_type.map(|t| t.to_string());

        // Build WHERE clauses dynamically based on which filters are provided.
        let mut param_idx = 2u32;
        let mut clauses = String::new();

        if type_filter.is_some() {
            clauses.push_str(&format!(" AND symbol_type = ${param_idx}"));
            param_idx += 1;
        }
        if project.is_some() {
            clauses.push_str(&format!(" AND project = ${param_idx}"));
        }

        let sql = format!(
            r#"
            SELECT id, name, symbol_type, file_path, start_line, end_line,
                   language, project, signature, file_mtime
            FROM {schema}.symbols
            WHERE name = $1
            {clauses}
            "#,
            schema = self.schema,
        );

        let mut query = sqlx::query_as::<_, SymbolRow>(&sql).bind(name);
        if let Some(ref t) = type_filter {
            query = query.bind(t);
        }
        if let Some(p) = project {
            query = query.bind(p);
        }

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.into_symbol()).collect())
    }

    /// Remove all symbols and relationships for a given file (for incremental re-index).
    pub async fn remove_file(&self, file_path: &str, project: &str) -> Result<u64> {
        let result = sqlx::query(&format!(
            "DELETE FROM {schema}.symbols WHERE file_path = $1 AND project = $2",
            schema = self.schema,
        ))
        .bind(file_path)
        .bind(project)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

#[derive(sqlx::FromRow)]
struct SymbolRow {
    id: Uuid,
    name: String,
    symbol_type: String,
    file_path: String,
    start_line: Option<i32>,
    end_line: Option<i32>,
    language: String,
    project: String,
    signature: Option<String>,
    file_mtime: chrono::DateTime<chrono::Utc>,
}

impl SymbolRow {
    fn into_symbol(self) -> Symbol {
        Symbol {
            id: self.id,
            name: self.name,
            symbol_type: self.symbol_type.parse().unwrap_or(SymbolType::Function),
            file_path: self.file_path,
            start_line: self.start_line,
            end_line: self.end_line,
            language: self.language,
            project: self.project,
            signature: self.signature,
            file_mtime: self.file_mtime,
        }
    }
}

#[derive(sqlx::FromRow)]
struct ImpactRow {
    id: Uuid,
    name: String,
    symbol_type: String,
    file_path: String,
    start_line: Option<i32>,
    end_line: Option<i32>,
    language: String,
    project: String,
    signature: Option<String>,
    file_mtime: chrono::DateTime<chrono::Utc>,
    depth: i32,
    path: Vec<Uuid>,
    rel_type: String,
    confidence: f32,
}

impl ImpactRow {
    fn into_impact_result(self) -> ImpactResult {
        ImpactResult {
            symbol: Symbol {
                id: self.id,
                name: self.name,
                symbol_type: self.symbol_type.parse().unwrap_or(SymbolType::Function),
                file_path: self.file_path,
                start_line: self.start_line,
                end_line: self.end_line,
                language: self.language,
                project: self.project,
                signature: self.signature,
                file_mtime: self.file_mtime,
            },
            depth: self.depth,
            path: self.path,
            relationship: self.rel_type.parse().unwrap_or(RelationType::Calls),
            confidence: self.confidence,
        }
    }
}
