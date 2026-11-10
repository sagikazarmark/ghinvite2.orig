use crate::SqlxStorage;
use serde_json::{Value, json};

#[tokio::test]
async fn raw_invitation_timing_cannot_be_cleared_rebound_or_changed_below_millisecond_precision() {
    let s = SqlxStorage::in_memory().await.unwrap();
    ghinvite_core::storage::test_suite::scenario_invitation_timing(s.clone()).await;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT delivery_id,receipt FROM delivery_outcomes")
            .fetch_all(&s.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    for (key, raw) in rows {
        let original: Value = serde_json::from_str(&raw).unwrap();
        // Test first-observation INSERT validation in rolled-back transactions.
        for (field, bad) in [
            ("created_at", json!("2026-05-04T13:00:01.000000001Z")),
            ("observed_at", json!("2026-05-04T12:59:59.999999999Z")),
            ("created_at", json!("invalid")),
            ("upstream_id", json!(998)),
            ("owned_lifetime_seconds", json!(0)),
        ] {
            let mut tx = s.pool.begin().await.unwrap();
            sqlx::query("DELETE FROM delivery_outcomes WHERE delivery_id=?1")
                .bind(&key)
                .execute(&mut *tx)
                .await
                .unwrap();
            let mut candidate = original.clone();
            candidate["invitation_timing"][field] = bad;
            let err = sqlx::query(
                "INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES(?1,?2,?3)",
            )
            .bind(&key)
            .bind(
                original["create"]["command"]["request_id"]
                    .as_str()
                    .unwrap(),
            )
            .bind(candidate.to_string())
            .execute(&mut *tx)
            .await
            .unwrap_err();
            assert!(
                err.to_string().contains("invitation timing conflict"),
                "{err}"
            );
            tx.rollback().await.unwrap();
        }

        for (field, bad) in [
            ("created_at", json!("2026-05-04T13:00:00.000000001Z")),
            ("observed_at", json!("2026-05-04T13:00:01.000000001Z")),
            ("upstream_id", json!(998)),
            ("owned_lifetime_seconds", json!(7200)),
        ] {
            let mut changed = original.clone();
            changed["revision"] = json!(99);
            changed["invitation_timing"][field] = bad;
            let err = sqlx::query("UPDATE delivery_outcomes SET receipt=?1 WHERE delivery_id=?2")
                .bind(changed.to_string())
                .bind(&key)
                .execute(&s.pool)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("invitation timing conflict"),
                "{err}"
            );
        }
        let mut cleared = original.clone();
        cleared["revision"] = json!(99);
        cleared.as_object_mut().unwrap().remove("invitation_timing");
        let err = sqlx::query("UPDATE delivery_outcomes SET receipt=?1 WHERE delivery_id=?2")
            .bind(cleared.to_string())
            .bind(&key)
            .execute(&s.pool)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("invitation timing conflict"),
            "{err}"
        );
        let after: String =
            sqlx::query_scalar("SELECT receipt FROM delivery_outcomes WHERE delivery_id=?1")
                .bind(&key)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(after, raw);
    }
}
