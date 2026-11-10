//! Raw upgrade preservation, independent of the current typed projection model.
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;

const CUTOVER: &str = include_str!("../../../migrations/0023_canonical_delivery_identity.sql");
const SEED: &str = include_str!("../../../tests/fixtures/canonical-delivery-migration.sql");

async fn fixture() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await
        .unwrap();
    for migration in sqlx::migrate!("../../migrations")
        .iter()
        .filter(|m| m.version <= 22)
    {
        sqlx::raw_sql(&migration.sql).execute(&pool).await.unwrap();
    }
    sqlx::raw_sql(SEED).execute(&pool).await.unwrap();
    pool
}

async fn snapshot(pool: &SqlitePool) -> BTreeMap<String, Vec<String>> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    ).fetch_all(pool).await.unwrap();
    let mut result = BTreeMap::new();
    for table in tables {
        assert!(table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .filter(|name| table != "deliveries" || name != "invitation_id")
            .collect::<Vec<_>>();
        let rows = sqlx::query_scalar(&format!(
            "SELECT json_array({}) AS raw FROM {table} ORDER BY raw",
            columns.join(","),
        ))
        .fetch_all(pool)
        .await
        .unwrap();
        result.insert(table, rows);
    }
    result
}

#[tokio::test]
async fn delivery_identity_rebuild_preserves_raw_history_and_guards() {
    let pool = fixture().await;
    let missing_clock = "SELECT count(*) FROM operational_delivery_steps WHERE create_outcome='created' AND settlement IS NULL AND sent_at IS NULL";
    assert_eq!(
        sqlx::query_scalar::<_, i64>(missing_clock)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let before = snapshot(&pool).await;
    let mut transaction = pool.begin().await.unwrap();
    sqlx::raw_sql(CUTOVER)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(missing_clock)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(snapshot(&pool).await, before);
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('deliveries')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(!columns.iter().any(|name| name == "invitation_id"));
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    for sql in [
        "UPDATE deliveries SET upstream_invitation_id=999",
        "UPDATE deliveries SET tracking_stopped_reason=NULL",
        "INSERT INTO organization_step_telemetry VALUES('missing','org',NULL,NULL)",
        "UPDATE delivery_outcomes SET receipt=json_set(receipt,'$.create.command.requester_id',999)",
    ] {
        assert!(
            sqlx::query(sql).execute(&pool).await.is_err(),
            "guard accepted {sql}"
        );
    }
    assert_eq!(snapshot(&pool).await, before);
}

#[tokio::test]
async fn bridge_only_audit_binding_refuses_cutover_and_rolls_back() {
    let pool = fixture().await;
    sqlx::query("DELETE FROM delivery_outcomes WHERE delivery_id NOT LIKE 'org:%'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    let mut transaction = pool.begin().await.unwrap();
    let error = sqlx::raw_sql(CUTOVER)
        .execute(&mut *transaction)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint failed"));
    transaction.rollback().await.unwrap();
    assert_eq!(snapshot(&pool).await, before);
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('deliveries')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(columns.iter().any(|name| name == "invitation_id"));
}
