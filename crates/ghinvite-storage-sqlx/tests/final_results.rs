use ghinvite_core::storage::test_suite::scenario_final_result_projection;
use ghinvite_storage_sqlx::SqlxStorage;

#[tokio::test]
async fn projected_result_columns_match_retained_final_classes() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-results-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    scenario_final_result_projection(storage.clone()).await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    let rows:Vec<(String,String,Option<String>)>=sqlx::query_as("SELECT r.justification,d.kind,d.final_result FROM deliveries d JOIN invitation_requests r ON r.id=d.request_id ORDER BY r.justification,d.kind").fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 18);
    for (name, kind, result) in rows {
        assert!(matches!(kind.as_str(), "repo" | "org"));
        let expected = match name.as_str() {
            "accepted" => Some("satisfied"),
            "owned_declined" => Some("declined"),
            "foreign_disappeared" | "own_expiry" | "expired" => Some("lapsed_invite"),
            "link_revoked" | "foreign_stopped" | "failed" => Some("unfulfillable"),
            "unknown" => None,
            _ => panic!("unexpected case {name}"),
        };
        assert_eq!(result.as_deref(), expected, "{kind}/{name}");
    }
    pool.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
