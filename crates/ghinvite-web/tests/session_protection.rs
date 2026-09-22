//! Contract tests at the session-store and raw persistence boundaries (#42).

use ghinvite_web::session_store::{Backend, ProtectedStore, SqliteBackend};
use tower_sessions::cookie::time::{Duration, OffsetDateTime};
use tower_sessions::{
    SessionStore,
    session::{Id, Record},
};

#[tokio::test]
async fn sqlite_records_are_protected_and_bound_to_the_requested_session() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool.clone());
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend, [7; 32]);
    let mut record = Record {
        id: Id::default(),
        data: [(
            "oauth_token".into(),
            serde_json::json!("secret-github-token"),
        )]
        .into(),
        expiry_date: OffsetDateTime::now_utc() + Duration::days(30),
    };
    store.create(&mut record).await.unwrap();
    let raw: Vec<u8> = sqlx::query_scalar("SELECT payload FROM protected_sessions WHERE id = ?")
        .bind(record.id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !raw.windows(b"secret-github-token".len())
            .any(|s| s == b"secret-github-token")
    );
    assert_eq!(
        store.load(&record.id).await.unwrap().unwrap().data["oauth_token"],
        "secret-github-token"
    );

    let other = Id::default();
    sqlx::query("INSERT INTO protected_sessions(id, payload, expires_at) SELECT ?, payload, expires_at FROM protected_sessions WHERE id = ?")
        .bind(other.to_string()).bind(record.id.to_string()).execute(&pool).await.unwrap();
    assert!(
        store.load(&other).await.unwrap().is_none(),
        "ciphertext substitution must fail closed"
    );
}

#[tokio::test]
async fn wrong_keys_tampering_and_unencrypted_records_fail_closed_without_destroying_records() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool.clone());
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend.clone(), [7; 32]);
    let session = tower_sessions::Session::new(None, std::sync::Arc::new(store.clone()), None);
    ghinvite_web::session::set_flash(&session, flash("private message"))
        .await
        .unwrap();
    session.save().await.unwrap();
    let id = session.id().unwrap();
    let raw = backend.get(&id).await.unwrap().unwrap();
    assert!(
        ProtectedStore::new(backend.clone(), [8; 32])
            .load(&id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(backend.get(&id).await.unwrap().unwrap(), raw);

    let mut tampered = raw.clone();
    *tampered.last_mut().unwrap() ^= 1;
    let mut changed_header = raw.clone();
    changed_header[20] ^= 1;
    let mut unknown_format = raw.clone();
    unknown_format[0] ^= 1;
    let plaintext = serde_json::to_vec(&store.load(&id).await.unwrap().unwrap()).unwrap();
    for invalid in [
        tampered,
        changed_header,
        unknown_format,
        raw[..10].to_vec(),
        plaintext,
    ] {
        backend
            .put(
                &id,
                &invalid,
                (OffsetDateTime::now_utc() + Duration::days(30)).unix_timestamp(),
            )
            .await
            .unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
        assert_eq!(backend.get(&id).await.unwrap().unwrap(), invalid);
        assert!(!backend.is_revoked(&id).await.unwrap());
    }
}

#[tokio::test]
async fn revocation_denies_a_late_write_even_when_the_session_record_is_recreated() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool.clone());
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend.clone(), [7; 32]);
    let session = tower_sessions::Session::new(None, std::sync::Arc::new(store.clone()), None);
    ghinvite_web::session_store::establish_authenticated_lifetime(&session)
        .await
        .unwrap();
    ghinvite_web::session::save(
        &session,
        &ghinvite_web::session::Session {
            login: "octocat".into(),
            access_token: "secret-token".into(),
            csrf_token: Some("csrf".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    session.save().await.unwrap();
    let id = session.id().unwrap();
    let stale_record = store.load(&id).await.unwrap().unwrap();
    let stale_payload = backend.get(&id).await.unwrap().unwrap();
    store.delete(&id).await.unwrap();
    assert!(store.save(&stale_record).await.is_err());
    // Models an already-dispatched put completing after revocation/deletion,
    // or a writer observing a cached negative marker lookup in KV.
    backend
        .put(
            &id,
            &stale_payload,
            stale_record.expiry_date.unix_timestamp(),
        )
        .await
        .unwrap();
    assert!(backend.get(&id).await.unwrap().is_some());
    assert!(store.load(&id).await.unwrap().is_none());
    let retention: i64 =
        sqlx::query_scalar("SELECT expires_at FROM session_revocations WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        retention
            >= (OffsetDateTime::now_utc() + Duration::days(31) - Duration::seconds(2))
                .unix_timestamp()
    );
    // A duplicate marker write must never shorten an existing safe deadline.
    sqlx::query("UPDATE session_revocations SET expires_at = expires_at + 60 WHERE id = ?")
        .bind(id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    store.delete(&id).await.unwrap();
    let duplicate_retention: i64 =
        sqlx::query_scalar("SELECT expires_at FROM session_revocations WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(duplicate_retention >= retention + 60);
}

#[tokio::test]
async fn writes_refresh_nonces_but_do_not_extend_absolute_lifetimes() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool);
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend.clone(), [7; 32]);
    let session = tower_sessions::Session::new(None, std::sync::Arc::new(store.clone()), None);
    ghinvite_web::session::set_flash(&session, flash("hello"))
        .await
        .unwrap();
    session.save().await.unwrap();
    let id = session.id().unwrap();
    let first = store.load(&id).await.unwrap().unwrap();
    assert!(first.expiry_date <= OffsetDateTime::now_utc() + Duration::minutes(30));
    let raw = backend.get(&id).await.unwrap().unwrap();
    let mut later_write = first.clone();
    later_write.expiry_date += Duration::days(365);
    store.save(&later_write).await.unwrap();
    assert_ne!(backend.get(&id).await.unwrap().unwrap(), raw);
    assert_eq!(
        store.load(&id).await.unwrap().unwrap().expiry_date,
        first.expiry_date
    );
    // Missing authenticated lifetime must fail, never mint 30 days at save time.
    let mut missing = Record {
        id: Id::default(),
        data: [("ghinvite".into(), serde_json::json!({"login": "octocat"}))].into(),
        expiry_date: later_write.expiry_date,
    };
    assert!(store.create(&mut missing).await.is_err());
}

fn flash(message: &str) -> ghinvite_web::session::Flash {
    ghinvite_web::session::Flash {
        level: ghinvite_web::session::FlashLevel::Success,
        message: message.into(),
    }
}

#[tokio::test]
async fn revocation_failure_is_reported_but_cleanup_failure_after_revocation_is_success() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool.clone());
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend.clone(), [7; 32]);
    let id = Id::default();
    sqlx::query("DROP TABLE protected_sessions")
        .execute(&pool)
        .await
        .unwrap();
    store.delete(&id).await.unwrap();
    assert!(backend.is_revoked(&id).await.unwrap());
    assert!(store.load(&id).await.unwrap().is_none());
    sqlx::query("DROP TABLE session_revocations")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.delete(&id).await.is_err());
    assert!(
        store.load(&id).await.is_err(),
        "unavailable revocation is not an absent marker"
    );
}

#[tokio::test]
async fn expired_snapshots_cannot_be_loaded_or_saved_even_if_backend_retains_them() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let backend = SqliteBackend::new(pool.clone());
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend.clone(), [7; 32]);
    // A valid historical anonymous session with two seconds left. Exercise the
    // authenticated absolute deadline independently of Tower/backend expiry.
    let issued_at = OffsetDateTime::now_utc().unix_timestamp() - 1798;
    let mut record = Record {
        id: Id::default(),
        data: [(
            "ghinvite_lifetime".into(),
            serde_json::json!({
                "issued_at": issued_at, "deadline": issued_at + 1800, "authenticated": false
            }),
        )]
        .into(),
        expiry_date: OffsetDateTime::now_utc() + Duration::days(365),
    };
    store.create(&mut record).await.unwrap();
    let payload = backend.get(&record.id).await.unwrap().unwrap();
    store.delete(&record.id).await.unwrap();
    let retained_until = (OffsetDateTime::now_utc() + Duration::days(365)).unix_timestamp();
    backend
        .put(&record.id, &payload, retained_until)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    // Model expiration cleanup of the marker: the ciphertext's own fixed
    // deadline must still reject a snapshot retained beyond storage expiry.
    sqlx::query("DELETE FROM session_revocations WHERE id = ?")
        .bind(record.id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    assert!(backend.get(&record.id).await.unwrap().is_some());
    assert!(store.load(&record.id).await.unwrap().is_none());
    assert!(store.save(&record).await.is_err());
}
