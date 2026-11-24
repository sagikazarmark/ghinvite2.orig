use ghinvite_core::storage::test_suite::scenario_request_reissue;
use ghinvite_storage_sqlx::SqlxStorage;
#[tokio::test]
async fn raw_reissue_requires_exact_immutable_revision_proof() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-reissue-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    scenario_request_reissue(storage.clone()).await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    let id: String = sqlx::query_scalar("SELECT id FROM invitation_requests LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    // An ordinary completion remains legal; a later reopening needs a new proof.
    sqlx::query("UPDATE invitation_requests SET state='lapsed',projection_revision=7,projection_content=json_set(projection_content,'$.state','lapsed','$.revision',7) WHERE id=?").bind(&id).execute(&pool).await.unwrap();
    for update in [
        "state='approved',projection_revision=8,projection_content=json_set(projection_content,'$.state','approved','$.revision',8)",
        "state='approved',projection_revision=4",
        "decided_by=701",
    ] {
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE invitation_requests SET {update} WHERE id=?"
            )))
            .bind(&id)
            .execute(&pool)
            .await
            .is_err()
        );
    }
    assert!(
        sqlx::query(
            "UPDATE request_reissues SET after_revision=after_revision+2 WHERE request_id=?"
        )
        .bind(&id)
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM request_reissues WHERE request_id=?")
            .bind(&id)
            .execute(&pool)
            .await
            .is_err()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM invitation_requests WHERE id=?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "lapsed");
    pool.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
