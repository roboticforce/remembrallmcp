use sqlx::PgPool;
use uuid::Uuid;

use crate::config::validate_schema_name;
use crate::error::{EngramError, Result};
use super::types::*;

/// Code graph storage backed by Postgres adjacency tables + recursive CTEs.
pub struct GraphStore {
    pool: PgPool,
    schema: String,
}

impl GraphStore {
    pub fn new(pool: PgPool, schema: String) -> Result<Self> {
        validate_schema_name(&schema)
            .map_err(EngramError::InvalidInput)?;
        Ok(Self { pool, schema })
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
                layer TEXT,
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
                (id, name, symbol_type, file_path, start_line, end_line, language, project, signature, file_mtime, layer)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                symbol_type = EXCLUDED.symbol_type,
                start_line = EXCLUDED.start_line,
                end_line = EXCLUDED.end_line,
                signature = EXCLUDED.signature,
                file_mtime = EXCLUDED.file_mtime,
                layer = EXCLUDED.layer
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
        .bind(&symbol.layer)
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
                s.language, s.project, s.signature, s.file_mtime, s.layer,
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
                   language, project, signature, file_mtime, layer
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

    /// Generate a topological ordering of files in a project, starting from entry points.
    ///
    /// Entry points are files with in-degree 0 in the import graph (nothing imports them),
    /// which are typically executables or top-level orchestrators. The algorithm runs
    /// Kahn's BFS topological sort so every dependency appears before the file that
    /// depends on it. Any files caught in a cycle are appended at the end.
    ///
    /// Returns at most `limit` stops (default 20).
    pub async fn generate_tour(&self, project: &str, limit: usize) -> Result<Vec<TourStop>> {
        // ------------------------------------------------------------------
        // 1. Fetch all file symbols for the project.
        // ------------------------------------------------------------------
        #[derive(sqlx::FromRow)]
        struct FileRow {
            id: uuid::Uuid,
            file_path: String,
            language: String,
        }

        let file_rows = sqlx::query_as::<_, FileRow>(&format!(
            r#"
            SELECT id, file_path, language
            FROM {schema}.symbols
            WHERE project = $1 AND symbol_type = 'file'
            "#,
            schema = self.schema,
        ))
        .bind(project)
        .fetch_all(&self.pool)
        .await?;

        if file_rows.is_empty() {
            return Ok(vec![]);
        }

        // Build index: file_path -> (uuid, language) - uuid kept for future use.
        let mut path_to_meta: std::collections::HashMap<String, (uuid::Uuid, String)> =
            std::collections::HashMap::new();
        for row in &file_rows {
            path_to_meta.insert(row.file_path.clone(), (row.id, row.language.clone()));
        }

        // ------------------------------------------------------------------
        // 2. Fetch all file-to-file import edges within the project.
        //    source_file imports target_file.
        // ------------------------------------------------------------------
        #[derive(sqlx::FromRow)]
        struct EdgeRow {
            source_file: String,
            target_file: String,
        }

        let edge_rows = sqlx::query_as::<_, EdgeRow>(&format!(
            r#"
            SELECT DISTINCT s.file_path AS source_file, t.file_path AS target_file
            FROM {schema}.relationships r
            JOIN {schema}.symbols s ON s.id = r.source_id
            JOIN {schema}.symbols t ON t.id = r.target_id
            WHERE r.rel_type = 'imports'
              AND s.project = $1
              AND t.project = $1
              AND s.symbol_type = 'file'
              AND t.symbol_type = 'file'
            "#,
            schema = self.schema,
        ))
        .bind(project)
        .fetch_all(&self.pool)
        .await?;

        // ------------------------------------------------------------------
        // 3. Build adjacency structures.
        //    imports_from[A] = files A imports (A depends on these).
        //    imported_by[A]  = files that import A.
        //    in_degree[A]    = number of files that import A.
        // ------------------------------------------------------------------
        let all_files: Vec<String> = file_rows.iter().map(|r| r.file_path.clone()).collect();

        let mut imports_from: std::collections::HashMap<String, Vec<String>> =
            all_files.iter().map(|f| (f.clone(), vec![])).collect();
        let mut imported_by: std::collections::HashMap<String, Vec<String>> =
            all_files.iter().map(|f| (f.clone(), vec![])).collect();

        for edge in &edge_rows {
            if imports_from.contains_key(&edge.source_file)
                && imported_by.contains_key(&edge.target_file)
            {
                imports_from
                    .entry(edge.source_file.clone())
                    .or_default()
                    .push(edge.target_file.clone());
                imported_by
                    .entry(edge.target_file.clone())
                    .or_default()
                    .push(edge.source_file.clone());
            }
        }

        // in_degree[f] = how many files import f.
        // Files with in_degree 0 are entry points (nobody imports them).
        let mut in_degree: std::collections::HashMap<String, usize> = all_files
            .iter()
            .map(|f| (f.clone(), imported_by[f].len()))
            .collect();

        // ------------------------------------------------------------------
        // 4. Fetch non-file symbol names grouped by file.
        // ------------------------------------------------------------------
        #[derive(sqlx::FromRow)]
        struct SymRow {
            file_path: String,
            name: String,
        }

        let sym_rows = sqlx::query_as::<_, SymRow>(&format!(
            r#"
            SELECT file_path, name
            FROM {schema}.symbols
            WHERE project = $1 AND symbol_type != 'file'
            ORDER BY file_path, start_line
            "#,
            schema = self.schema,
        ))
        .bind(project)
        .fetch_all(&self.pool)
        .await?;

        let mut file_symbols: std::collections::HashMap<String, Vec<String>> =
            all_files.iter().map(|f| (f.clone(), vec![])).collect();
        for row in sym_rows {
            file_symbols.entry(row.file_path).or_default().push(row.name);
        }

        // ------------------------------------------------------------------
        // 5. Kahn's BFS topological sort.
        //    Visit entry points (in_degree == 0) first. For each visited file
        //    decrement the in_degree of the files it imports; when a dependency
        //    reaches 0 it means all files that import it have been scheduled and
        //    it can now be added to the queue.
        // ------------------------------------------------------------------
        let mut initial_queue: Vec<String> = all_files
            .iter()
            .filter(|f| in_degree[*f] == 0)
            .cloned()
            .collect();
        initial_queue.sort();

        let mut queue: std::collections::VecDeque<String> =
            initial_queue.into_iter().collect();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut ordered: Vec<String> = Vec::new();

        while let Some(file) = queue.pop_front() {
            if visited.contains(&file) {
                continue;
            }
            visited.insert(file.clone());
            ordered.push(file.clone());

            // Decrement in_degree for everything this file imports.
            let mut deps: Vec<String> = imports_from
                .get(&file)
                .cloned()
                .unwrap_or_default();
            deps.sort();
            for dep in deps {
                let deg = in_degree.entry(dep.clone()).or_insert(1);
                if *deg > 0 {
                    *deg -= 1;
                }
                if *deg == 0 && !visited.contains(&dep) {
                    queue.push_back(dep);
                }
            }
        }

        // Append files not reached due to cycles.
        let mut remaining: Vec<String> = all_files
            .iter()
            .filter(|f| !visited.contains(*f))
            .cloned()
            .collect();
        remaining.sort();
        ordered.extend(remaining);

        // ------------------------------------------------------------------
        // 6. Build TourStop list, capped at limit.
        // ------------------------------------------------------------------
        let stops: Vec<TourStop> = ordered
            .into_iter()
            .take(limit)
            .enumerate()
            .map(|(idx, file)| {
                let order = idx + 1;
                let language = path_to_meta
                    .get(&file)
                    .map(|(_, lang)| lang.clone())
                    .unwrap_or_default();
                let symbols = file_symbols.get(&file).cloned().unwrap_or_default();
                let imp_from = imports_from.get(&file).cloned().unwrap_or_default();
                let imp_by = imported_by.get(&file).cloned().unwrap_or_default();

                let reason = if imp_by.is_empty() && imp_from.is_empty() {
                    "Standalone file - no import relationships recorded".to_string()
                } else if imp_by.is_empty() {
                    "Entry point - no other files import this".to_string()
                } else if imp_from.is_empty() {
                    format!(
                        "Core module - imported by {} file{}",
                        imp_by.len(),
                        if imp_by.len() == 1 { "" } else { "s" }
                    )
                } else {
                    let dep_names: Vec<&str> = imp_from
                        .iter()
                        .take(3)
                        .map(|s| s.rfind('/').map(|i| &s[i + 1..]).unwrap_or(s.as_str()))
                        .collect();
                    format!("Depends on: {} (read those first)", dep_names.join(", "))
                };

                TourStop {
                    order,
                    file_path: file,
                    language,
                    symbols,
                    imports_from: imp_from,
                    imported_by: imp_by,
                    reason,
                }
            })
            .collect();

        Ok(stops)
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
    layer: Option<String>,
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
            layer: self.layer,
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
    layer: Option<String>,
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
                layer: self.layer,
            },
            depth: self.depth,
            path: self.path,
            relationship: self.rel_type.parse().unwrap_or(RelationType::Calls),
            confidence: self.confidence,
        }
    }
}
