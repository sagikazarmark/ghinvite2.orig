//! AC-SES-1: rollout, retirement, binding, and operator diagnostics through HTTP.
use anykms_keyring::Keyring;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ghinvite_github::mocks::MockTransport;
use ghinvite_web::{
    WebConfig, session,
    session_store::{Backend, ProtectedStore},
};
use ghinvite_web_server::SqliteBackend;
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use tower_sessions::{
    SessionStore,
    session::{Id, Record},
};

fn ring(current: u32, generations: &[(u32, u8)]) -> Arc<Keyring> {
    let mut builder = Keyring::builder("p1");
    for (version, byte) in generations {
        builder = builder.generation(*version, [*byte; 32]).unwrap();
    }
    Arc::new(builder.current(current).unwrap().build().unwrap())
}
async fn backend() -> SqliteBackend {
    let backend = SqliteBackend::new(sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap());
    backend.migrate().await.unwrap();
    backend
}
async fn create(store: ProtectedStore<SqliteBackend>, user: u64) -> Id {
    let tower = tower_sessions::Session::new(None, Arc::new(store), None);
    ghinvite_web::session_store::establish_authenticated_lifetime(&tower)
        .await
        .unwrap();
    session::save(
        &tower,
        &session::Session {
            user_id: user,
            login: "octocat".into(),
            csrf_token: Some("csrf".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tower.save().await.unwrap();
    tower.id().unwrap()
}
async fn app(backend: SqliteBackend, keyring: Arc<Keyring>) -> axum::Router {
    let mut config = WebConfig::with_keyring(keyring.clone());
    config.operator_ids.insert(42);
    let state = crate::common::sign_in::app_state(
        Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        ),
        Arc::new(MockTransport::scripted(vec![])),
        Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        config,
    );
    common::build_app(state, ProtectedStore::new(backend, keyring))
}
async fn get(app: &axum::Router, id: Option<Id>) -> axum::response::Response {
    let mut request = Request::builder().uri("/console/keyring");
    if let Some(id) = id {
        request = request.header("cookie", format!("id={id}"));
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
#[tokio::test]
async fn generations_overlap_then_retire_without_extending_lifetime() {
    let backend = backend().await;
    let old = ring(1, &[(1, 7)]);
    let both = ring(2, &[(1, 7), (2, 8)]);
    let retired = ring(2, &[(2, 8)]);
    let old_store = ProtectedStore::new(backend.clone(), old.clone());
    let id = create(old_store.clone(), 42).await;
    let original = old_store.load(&id).await.unwrap().unwrap();
    let upgraded = ProtectedStore::new(backend.clone(), both.clone());
    assert_eq!(
        upgraded.load(&id).await.unwrap().unwrap().expiry_date,
        original.expiry_date
    );
    let diagnostic = app(backend.clone(), both.clone()).await;
    let response = get(&diagnostic, Some(id)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec()
        )
        .unwrap(),
        both.fingerprint().to_string()
    );
    let new_id = create(upgraded, 42).await;
    assert!(
        ProtectedStore::new(backend.clone(), retired.clone())
            .load(&new_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        ProtectedStore::new(backend.clone(), retired.clone())
            .load(&id)
            .await
            .unwrap()
            .is_none()
    );
    let retired_app = app(backend.clone(), retired).await;
    assert_eq!(
        get(&retired_app, Some(id)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert!(backend.get(&id).await.unwrap().is_some());
    assert!(!backend.is_revoked(&id).await.unwrap());
}
#[tokio::test]
async fn lagging_isolate_returns_503_preserves_cookie_and_record_configuration_returns_500() {
    let backend = backend().await;
    let current = ring(2, &[(1, 7), (2, 8)]);
    let id = create(ProtectedStore::new(backend.clone(), current), 42).await;
    let raw = backend.get(&id).await.unwrap().unwrap();
    let lagging = app(backend.clone(), ring(1, &[(1, 7)])).await;
    let response = get(&lagging, Some(id)).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!response.headers().contains_key("set-cookie"));
    assert_eq!(backend.get(&id).await.unwrap().unwrap(), raw);
    assert!(!backend.is_revoked(&id).await.unwrap());
    let changed_bytes = app(backend.clone(), ring(2, &[(2, 9)])).await;
    assert_eq!(
        get(&changed_bytes, Some(id)).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(backend.get(&id).await.unwrap().unwrap(), raw);
}
#[tokio::test]
async fn diagnostic_is_numeric_operator_only_and_session_copy_confers_no_authority() {
    let backend = backend().await;
    let keyring = ring(1, &[(1, 7)]);
    let store = ProtectedStore::new(backend.clone(), keyring.clone());
    let operator = create(store.clone(), 42).await;
    let regular = create(store.clone(), 43).await;
    let copied = Id::default();
    backend
        .put(
            &copied,
            &backend.get(&operator).await.unwrap().unwrap(),
            chrono::Utc::now().timestamp() + 1000,
        )
        .await
        .unwrap();
    let app = app(backend, keyring).await;
    for id in [None, Some(regular), Some(copied)] {
        assert_eq!(get(&app, id).await.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(get(&app, Some(operator)).await.status(), StatusCode::OK);
}
#[test]
fn every_session_ttl_is_bounded_by_thirty_days() {
    use ghinvite_web::session_store::session_ttl;
    for deadline in [i64::MIN, 0, 1, 60, 61, 1800, 2592000, 2592001, i64::MAX] {
        assert!((61..=30 * 24 * 60 * 60).contains(&session_ttl(deadline, 0)));
    }
    assert_eq!(session_ttl(1800, 0), 1800);
}

mod common;

/// Seal `record` as deployments before GM-09 did: verbatim, so a session
/// payload keeps whatever fields it was written with.
async fn put_verbatim(backend: &SqliteBackend, keyring: &Keyring, record: &Record) {
    use anykms_keyring::ContextEnvelopeSync;
    use anykms_keyring::{EncryptionContext, purposes};
    use anykms_recipes::envelope::Padding;
    let id = record.id.to_string();
    let context = EncryptionContext::try_from(&purposes::Session { session_id: &id }).unwrap();
    let ciphertext = keyring
        .context_envelope(Padding::NONE)
        .and_then(|envelope| envelope.encrypt_sync(&serde_json::to_vec(record).unwrap(), &context))
        .unwrap();
    backend
        .put(
            &record.id,
            &ciphertext.into_bytes(),
            record.expiry_date.unix_timestamp(),
        )
        .await
        .unwrap();
}

/// Sessions signed in through legacy OAuth (before GM-09) carry the user's
/// token and OAuth state. They stay signed in with the same identity and
/// lifetime, nothing reads the token, and the next write of the record,
/// whatever it changes, stores the session without it.
#[tokio::test]
async fn legacy_sign_in_sessions_stay_signed_in_and_lose_their_token_on_the_next_write() {
    let backend = backend().await;
    let keyring = ring(1, &[(1, 7)]);
    let store = ProtectedStore::new(backend.clone(), keyring.clone());
    let id = create(store.clone(), 42).await;
    let mut legacy = store.load(&id).await.unwrap().unwrap();
    let payload = legacy.data.get_mut("ghinvite").unwrap();
    payload["access_token"] = serde_json::json!("ghu_LEGACY_TOKEN_MIGRATION_PRIVATE");
    payload["oauth_csrf"] = serde_json::json!("LEGACY_STATE_PRIVATE");
    put_verbatim(&backend, &keyring, &legacy).await;

    let loaded = store.load(&id).await.unwrap().unwrap();
    assert_eq!(
        loaded.data["ghinvite"]["access_token"], "ghu_LEGACY_TOKEN_MIGRATION_PRIVATE",
        "the old record is stored as written until something writes it"
    );
    assert_eq!(loaded.expiry_date, legacy.expiry_date);
    let operator = app(backend.clone(), keyring.clone()).await;
    let response = get(&operator, Some(id)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("set-cookie"));

    // A write that touches another part of the record still drops them.
    let mut touched = loaded.clone();
    touched
        .data
        .insert("unrelated".into(), serde_json::json!("value"));
    store.save(&touched).await.unwrap();
    let saved = store.load(&id).await.unwrap().unwrap();
    let session = &saved.data["ghinvite"];
    assert!(session.get("access_token").is_none());
    assert!(session.get("oauth_csrf").is_none());
    assert_eq!(
        (&session["user_id"], &session["login"]),
        (&serde_json::json!(42), &serde_json::json!("octocat"))
    );
    assert_eq!(
        saved.data["ghinvite_lifetime"],
        legacy.data["ghinvite_lifetime"]
    );
    assert_eq!(saved.expiry_date, legacy.expiry_date);
    assert!(!format!("{:?}", saved.data).contains("PRIVATE"));
    assert_eq!(get(&operator, Some(id)).await.status(), StatusCode::OK);
}
