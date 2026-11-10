use super::*;
use ghinvite_core::storage::blocked_digest::DailyDigestStorage;

#[tokio::test]
async fn overview_daily_digest_scopes_counts_and_distinguishes_missing_stale_zero_and_failure() {
    let path = std::env::temp_dir().join(format!(
        "digest-console-{}.sqlite",
        ghinvite_core::InvitationLinkId::new()
    ));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap(),
    );
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let read = || async {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/console/accounts/acme")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        (status, response_html(response).await)
    };
    let (status, html) = read().await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("first daily delivery summary is not ready"));
    assert!(!html.contains("No blocked deliveries"));

    let now = Utc::now();
    let yesterday = now - Duration::days(1);
    storage.record_blocked_digest(yesterday).await.unwrap();
    for (account, count) in [(9001, 1), (9002, 2)] {
        let mut link = list_link(1);
        link.account_id = account;
        if account == 9002 {
            link.installation_id = 78;
        }
        storage.seed_link(&link).await.unwrap();
        for index in 0..count {
            sqlx::query("INSERT INTO blocked_digest_items(day,delivery_key,link_id,account_id,reason,kind,step) VALUES (?,?,?,?, 'mint_forbidden','repo','repo')")
                .bind(yesterday.date_naive().to_string()).bind(format!("{account}/{index}"))
                .bind(link.id.to_string()).bind(account as i64).execute(&pool).await.unwrap();
        }
    }
    let (status, html) = read().await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("latest available summary from"));
    assert!(html.contains("1: Installation credential could not be issued"));
    assert!(!html.contains("2: Installation credential"));
    assert!(!html.contains("3: Installation credential"));

    storage.record_blocked_digest(now).await.unwrap();
    let (status, html) = read().await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("No blocked deliveries in this summary"));
    assert!(!html.contains("latest available summary from"));
    sqlx::query("DROP TABLE blocked_digest_items")
        .execute(&pool)
        .await
        .unwrap();
    let (status, html) = read().await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(html.contains("The delivery summary could not be loaded"));
    assert!(!html.contains("No blocked deliveries"));
    assert!(!html.contains("blocked_digest_items"));
    pool.close().await;
    drop(app);
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
