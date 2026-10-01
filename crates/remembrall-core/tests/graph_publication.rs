//! Requires an explicitly provisioned disposable PostgreSQL instance. Never falls
//! back to DATABASE_URL or the application configuration, and never skips tests.
use chrono::Utc;
use remembrall_core::graph::{publication::refresh_project, store::GraphStore, types::*};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::time::Duration;
use uuid::Uuid;

async fn fixture() -> (PgPool, GraphStore, String) {
    let url = std::env::var("REMEMBRALL_GRAPH_TEST_DATABASE_URL")
        .expect("set REMEMBRALL_GRAPH_TEST_DATABASE_URL to an explicitly isolated test database");
    let expected_data = std::env::var("REMEMBRALL_GRAPH_TEST_DATA_DIRECTORY")
        .expect("set REMEMBRALL_GRAPH_TEST_DATA_DIRECTORY to the independently provisioned PostgreSQL data directory");
    let expected_database = std::env::var("REMEMBRALL_GRAPH_TEST_DATABASE_NAME")
        .expect("set REMEMBRALL_GRAPH_TEST_DATABASE_NAME to the independently provisioned disposable database");
    assert!(
        expected_database.ends_with("_test"),
        "disposable database name must end with _test"
    );
    assert!(
        std::path::Path::new(&expected_data).is_absolute(),
        "expected data directory must be absolute"
    );
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await
        .unwrap();
    let (database, data): (String, String) =
        sqlx::query_as("SELECT current_database(), current_setting('data_directory')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(database, expected_database);
    assert_eq!(data, expected_data);
    let schema = format!("graph_test_{}", Uuid::new_v4().simple());
    let store = GraphStore::new(pool.clone(), schema.clone()).unwrap();
    store.init().await.unwrap();
    (pool, store, schema)
}
fn symbol(project: &str, name: &str) -> Symbol {
    Symbol {
        id: Uuid::new_v4(),
        name: name.into(),
        symbol_type: SymbolType::Function,
        file_path: format!("/{name}.ts"),
        start_line: Some(1),
        end_line: Some(1),
        language: "typescript".into(),
        project: project.into(),
        signature: None,
        file_mtime: Utc::now(),
        layer: None,
        parent_symbol_id: None,
        moniker: None,
    }
}
async fn ids(pool: &PgPool, schema: &str, project: &str) -> Vec<Uuid> {
    sqlx::query_scalar(&format!(
        "SELECT id FROM {schema}.symbols WHERE project=$1 ORDER BY id"
    ))
    .bind(project)
    .fetch_all(pool)
    .await
    .unwrap()
}
async fn cleanup(pool: PgPool, schema: String) {
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn concurrent_generations_serialize_and_readers_see_old_snapshot() {
    let (pool, store, schema) = fixture().await;
    let old = symbol("same", "old");
    store
        .begin_project_update("same")
        .await
        .unwrap()
        .replace(&[old.clone()], &[])
        .await
        .unwrap();
    let first = store.begin_project_update("same").await.unwrap();
    let second_store = GraphStore::new(pool.clone(), schema.clone()).unwrap();
    let mut second =
        tokio::spawn(async move { second_store.begin_project_update("same").await.unwrap() });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut second)
            .await
            .is_err(),
        "another process/connection must wait"
    );
    assert_eq!(ids(&pool, &schema, "same").await, vec![old.id]);
    let a = symbol("same", "a");
    first.replace(&[a.clone()], &[]).await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ids(&pool, &schema, "same").await, vec![a.id]);
    let b = symbol("same", "b");
    second.replace(&[b.clone()], &[]).await.unwrap();
    assert_eq!(ids(&pool, &schema, "same").await, vec![b.id]);
    cleanup(pool, schema).await;
}

#[tokio::test]
async fn failed_insert_after_delete_rolls_back_and_releases_lock() {
    let (pool, store, schema) = fixture().await;
    let old = symbol("target", "old");
    let foreign = symbol("other", "foreign");
    store
        .begin_project_update("target")
        .await
        .unwrap()
        .replace(&[old.clone()], &[])
        .await
        .unwrap();
    store
        .begin_project_update("other")
        .await
        .unwrap()
        .replace(&[foreign.clone()], &[])
        .await
        .unwrap();
    let mut collision = symbol("target", "collision");
    collision.id = foreign.id;
    assert!(
        store
            .begin_project_update("target")
            .await
            .unwrap()
            .replace(&[collision], &[])
            .await
            .is_err()
    );
    let lease = tokio::time::timeout(Duration::from_secs(3), store.begin_project_update("target"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ids(&pool, &schema, "target").await, vec![old.id]);
    assert_eq!(ids(&pool, &schema, "other").await, vec![foreign.id]);
    drop(lease);
    cleanup(pool, schema).await;
}

#[tokio::test]
async fn projects_do_not_block_each_other_and_dropped_lease_releases() {
    let (pool, store, schema) = fixture().await;
    let lease = store.begin_project_update("one").await.unwrap();
    let other = tokio::time::timeout(Duration::from_secs(2), store.begin_project_update("two"))
        .await
        .unwrap()
        .unwrap();
    other.replace(&[symbol("two", "kept")], &[]).await.unwrap();
    drop(lease);
    let recovered = tokio::time::timeout(Duration::from_secs(2), store.begin_project_update("one"))
        .await
        .unwrap()
        .unwrap();
    recovered.replace(&[], &[]).await.unwrap();
    assert_eq!(ids(&pool, &schema, "two").await.len(), 1);
    cleanup(pool, schema).await;
}

#[tokio::test]
async fn watcher_refresh_rebuilds_inbound_links_and_preserves_graph_on_parse_failure() {
    let (pool, store, schema) = fixture().await;
    let root = std::env::temp_dir().join(format!("graph-refresh-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("target.ts"),
        "export function target() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("caller.ts"),
        "import { target } from './target';\nexport function caller() { return target(); }\n",
    )
    .unwrap();
    refresh_project(&store, &root, "watched").await.unwrap();
    let query = format!(
        "SELECT count(*) FROM {schema}.relationships r JOIN {schema}.symbols s ON s.id=r.source_id JOIN {schema}.symbols t ON t.id=r.target_id WHERE s.project='watched' AND s.name='caller' AND t.name='target' AND r.rel_type='calls'"
    );
    let before: i64 = sqlx::query_scalar(&query).fetch_one(&pool).await.unwrap();
    assert_eq!(before, 1);
    let old_ids = ids(&pool, &schema, "watched").await;
    std::fs::write(
        root.join("target.ts"),
        "export function target() { return 2; }\n",
    )
    .unwrap();
    refresh_project(&store, &root, "watched").await.unwrap();
    let after: i64 = sqlx::query_scalar(&query).fetch_one(&pool).await.unwrap();
    assert_eq!(
        after, 1,
        "unchanged caller must point to the newly indexed target"
    );
    let good_ids = ids(&pool, &schema, "watched").await;
    assert_ne!(old_ids, good_ids);
    std::fs::write(
        root.join("target.ts"),
        "export function target( { broken syntax !!!",
    )
    .unwrap();
    let error = refresh_project(&store, &root, "watched").await.unwrap_err();
    assert!(error.to_string().contains("target.ts"));
    assert_eq!(ids(&pool, &schema, "watched").await, good_ids);
    std::fs::remove_file(root.join("target.ts")).unwrap();
    refresh_project(&store, &root, "watched").await.unwrap();
    let remaining: i64 = sqlx::query_scalar(&query).fetch_one(&pool).await.unwrap();
    assert_eq!(remaining, 0);
    std::fs::remove_dir_all(root).unwrap();
    cleanup(pool, schema).await;
}

#[tokio::test]
async fn legacy_indexer_pool_one_preserves_custom_parser_and_rolls_back_metadata() {
    use remembrall_core::error::{RemembrallError, Result};
    use remembrall_core::indexer::{CodeParser, Indexer, IndexerConfig, ParseResult};
    struct CustomParser;
    impl CodeParser for CustomParser {
        fn parse(&self, path: &str, source: &str, _language: &str) -> Result<ParseResult> {
            if source == "fail" {
                return Err(RemembrallError::InvalidInput("custom parse failure".into()));
            }
            let mut sym = symbol("custom", source);
            sym.file_path = path.into();
            Ok(ParseResult {
                symbols: vec![sym],
                relationships: vec![],
            })
        }
    }
    let (pool, _store, schema) = fixture().await;
    let url = std::env::var("REMEMBRALL_GRAPH_TEST_DATABASE_URL").unwrap();
    let single = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("graph-custom-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.custom"), "first").unwrap();
    std::fs::write(root.join("ignored.ts"), "invalid built in source").unwrap();
    let indexer = Indexer::new(
        single.clone(),
        schema.clone(),
        IndexerConfig {
            root_path: root.clone(),
            project: "custom".into(),
            extensions: vec!["custom".into()],
            ignore_patterns: vec![],
        },
    )
    .unwrap();
    indexer.init().await.unwrap();
    let stats = tokio::time::timeout(Duration::from_secs(3), indexer.run(&CustomParser))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stats.files_scanned, 1);
    assert_eq!(stats.symbols_added, 1);
    let before = ids(&pool, &schema, "custom").await;
    let query = format!("SELECT file_path, mtime FROM {schema}.file_index ORDER BY file_path");
    let old_metadata: Vec<(String, chrono::DateTime<Utc>)> =
        sqlx::query_as(&query).fetch_all(&pool).await.unwrap();
    // Fail after DELETE+INSERT of symbols, when the same transaction writes tracking rows.
    sqlx::query(&format!("ALTER TABLE {schema}.file_index ADD CONSTRAINT reject_tracking CHECK (project <> 'custom') NOT VALID"))
        .execute(&pool).await.unwrap();
    std::fs::write(root.join("a.custom"), "second").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), indexer.run(&CustomParser))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(ids(&pool, &schema, "custom").await, before);
    let unchanged: Vec<(String, chrono::DateTime<Utc>)> =
        sqlx::query_as(&query).fetch_all(&pool).await.unwrap();
    assert_eq!(unchanged, old_metadata);
    std::fs::write(root.join("a.custom"), "fail").unwrap();
    assert!(indexer.run(&CustomParser).await.is_err());
    assert_eq!(ids(&pool, &schema, "custom").await, before);
    single.close().await;
    std::fs::remove_dir_all(root).unwrap();
    cleanup(pool, schema).await;
}

#[tokio::test]
async fn waiting_refresh_reads_source_only_after_project_lock_and_schema_is_independent() {
    let (pool, store, schema) = fixture().await;
    let root = std::env::temp_dir().join(format!("graph-ordered-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source.ts"), "export function stale() {}\n").unwrap();
    let lease = store.begin_project_update("ordered").await.unwrap();
    let second = GraphStore::new(pool.clone(), schema.clone()).unwrap();
    let path = root.clone();
    let mut refresh = tokio::spawn(async move { refresh_project(&second, &path, "ordered").await });
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut refresh)
            .await
            .is_err()
    );
    let other_schema = format!("graph_test_{}", Uuid::new_v4().simple());
    let other = GraphStore::new(pool.clone(), other_schema.clone()).unwrap();
    other.init().await.unwrap();
    let independent = tokio::time::timeout(
        Duration::from_secs(2),
        other.begin_project_update("ordered"),
    )
    .await
    .unwrap()
    .unwrap();
    independent
        .replace(&[symbol("ordered", "independent")], &[])
        .await
        .unwrap();
    std::fs::write(root.join("source.ts"), "export function latest() {}\n").unwrap();
    lease.replace(&[], &[]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), refresh)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .find_symbol("latest", None, Some("ordered"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .find_symbol("stale", None, Some("ordered"))
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query(&format!("DROP SCHEMA {other_schema} CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
    cleanup(pool, schema).await;
}
