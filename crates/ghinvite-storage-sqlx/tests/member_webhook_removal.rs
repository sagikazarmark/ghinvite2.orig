//! The additive removal preserves organization bindings and immutable audit facts.
#[tokio::test]
async fn obsolete_member_table_removed_without_rewriting_org_receipts() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let migrations = sqlx::migrate!("../../migrations");
    for migration in migrations.iter().filter(|m| m.version < 22) {
        sqlx::raw_sql(migration.sql.clone())
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO webhook_receipts(payload_sha256,app,event,action) VALUES(?,'repo','member','added')")
        .bind("b".repeat(64)).execute(&pool).await.unwrap();
    let retained = "{\"retained\":\"unchanged\"}";
    sqlx::query("INSERT INTO organization_webhook_receipts VALUES('org',?,1,604801,?)")
        .bind("c".repeat(64))
        .bind(retained)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,projection_content) VALUES('kept-audit',9010,'2026-10-17T12:00:00Z','invitation.accepted','system','github_invitation','original-id',?,?)")
        .bind(retained).bind(retained).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0022_remove_member_webhook_facade.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let obsolete: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='webhook_receipts'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(obsolete, 0);
    let row: (String, String, i64, i64, String) = sqlx::query_as("SELECT app,payload_sha256,received_at,expires_at,receipt FROM organization_webhook_receipts")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(
        row,
        ("org".into(), "c".repeat(64), 1, 604801, retained.into())
    );
    let audit: (String, i64, String, String, String) = sqlx::query_as(
        "SELECT id,account_id,occurred_at,metadata,projection_content FROM audit_events",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        audit,
        (
            "kept-audit".into(),
            9010,
            "2026-10-17T12:00:00Z".into(),
            retained.into(),
            retained.into()
        )
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
}
