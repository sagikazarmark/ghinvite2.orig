use crate::SqlxStorage;
use sqlx::Row;

#[tokio::test]
async fn raw_audit_version_guards_reject_mutation_and_malformed_inserts() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    ghinvite_core::storage::test_suite::scenario_canonical_delivery_audit(storage.clone()).await;
    let before = sqlx::query(
        "SELECT delivery_id,request_id,receipt FROM delivery_outcomes ORDER BY delivery_id",
    )
    .fetch_all(&storage.pool)
    .await
    .unwrap();
    for row in &before {
        let id: String = row.get("delivery_id");
        let receipt: String = row.get("receipt");
        let value: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        let opposite = if value["create"]["audit_version"] == "canonical_v1" {
            "\"legacy\""
        } else {
            "\"canonical_v1\""
        };
        for version in [opposite, "null", "123", "\"future\""] {
            let error=sqlx::query("UPDATE delivery_outcomes SET receipt=json_set(receipt,'$.create.audit_version',json(?1),'$.revision',json_extract(receipt,'$.revision')+1) WHERE delivery_id=?2")
                .bind(version).bind(&id).execute(&storage.pool).await.unwrap_err();
            assert!(
                error.to_string().contains("delivery audit version"),
                "{error}"
            );
            let current: String =
                sqlx::query_scalar("SELECT receipt FROM delivery_outcomes WHERE delivery_id=?1")
                    .bind(&id)
                    .fetch_one(&storage.pool)
                    .await
                    .unwrap();
            assert_eq!(current, receipt);
        }
        for version in ["null", "123", "\"future\""] {
            let error=sqlx::query("INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES ('raw-audit-control',?1,json_set(?2,'$.create.audit_version',json(?3)))")
                .bind(row.get::<String,_>("request_id")).bind(&receipt).bind(version).execute(&storage.pool).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("delivery audit version/identity conflict"),
                "{error}"
            );
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM delivery_outcomes")
            .fetch_one(&storage.pool)
            .await
            .unwrap(),
        before.len() as i64
    );
}
