//! Actual SQLite atomic consumption and production keyring envelopes.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ghinvite_core::clock::Clock;
use ghinvite_core::purpose;
use ghinvite_core::storage::{
    self,
    pending_signin::{PendingSignInKey, PendingSignInStorage, StoredPendingSignIn},
};
use ghinvite_web::signin::{
    PendingSignIn, PendingSignInStore, StateUnavailable, storage::SqlPendingSignInStore,
};
use kekring::{Aad, Keyring};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, Ordering},
};
const STATE: &str = "STATE_MARKER_0123456789012345678901234567890123456789";
const VERIFIER: &str = "PKCE_MARKER_01234567890123456789012345678901234567890123456789";
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
fn pending(issued_at: i64) -> PendingSignIn {
    serde_json::from_value(json!({"state":STATE,"verifier":VERIFIER,"issued_at":issued_at}))
        .unwrap()
}
fn key(id: &str) -> PendingSignInKey {
    let mut hash = Sha256::new();
    hash.update(b"p1:pending-signin:v1\0");
    hash.update(id);
    PendingSignInKey::from_bytes(hash.finalize().into())
}
struct Fixture {
    path: std::path::PathBuf,
    sql: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    pool: sqlx::SqlitePool,
    ring: Arc<Keyring>,
    clock: Arc<TestClock>,
}
impl Fixture {
    async fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pending-signin-{}.sqlite",
            ghinvite_core::RequestId::new()
        ));
        let sql = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
                .await
                .unwrap(),
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        Self {
            path,
            sql,
            pool,
            ring: Arc::new(
                Keyring::builder("p1")
                    .generation(1, [7; 32])
                    .current(1)
                    .build()
                    .unwrap(),
            ),
            clock: Arc::new(TestClock(AtomicI64::new(1_000_000))),
        }
    }
    fn store(&self) -> SqlPendingSignInStore {
        SqlPendingSignInStore::new(self.sql.clone(), self.ring.clone())
            .with_clock(self.clock.clone())
    }
    async fn count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM pending_signins")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn encrypted_sqlite_rows_and_concurrent_independent_consumers() {
    let f = Fixture::new().await;
    let id = tower_sessions::session::Id::default().to_string();
    let store = f.store();
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    let row: (String, String, i64) =
        sqlx::query_as("SELECT session_key,ciphertext,expires_at FROM pending_signins")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(row.0, key(&id).as_hex());
    assert_eq!(row.2, f.clock.now() + 1800);
    for secret in [STATE, VERIFIER, &id] {
        assert!(!format!("{row:?}").contains(secret));
    }
    let bytes = STANDARD.decode(&row.1).unwrap();
    assert!(
        !bytes
            .windows(STATE.len())
            .any(|part| part == STATE.as_bytes())
    );
    assert!(
        !bytes
            .windows(VERIFIER.len())
            .any(|part| part == VERIFIER.as_bytes())
    );
    let independent = SqlPendingSignInStore::new(
        Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&f.path)
                .await
                .unwrap(),
        ),
        f.ring.clone(),
    )
    .with_clock(f.clock.clone());
    let (a, b) = tokio::join!(store.take(&id), independent.take(&id));
    let values = [a.unwrap(), b.unwrap()];
    assert_eq!(values.iter().filter(|value| value.is_some()).count(), 1);
    let value = values.into_iter().flatten().next().unwrap();
    assert_eq!(
        serde_json::to_value(value).unwrap(),
        json!({"state":STATE,"verifier":VERIFIER,"issued_at":f.clock.now()})
    );
    assert!(store.take(&id).await.unwrap().is_none());
    assert_eq!(f.count().await, 0);
}

#[tokio::test]
async fn expiration_and_metadata_tampering_never_extend_authority() {
    let f = Fixture::new().await;
    let id = tower_sessions::session::Id::default().to_string();
    let store = f.store();
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    f.clock.0.fetch_add(1800, Ordering::SeqCst);
    assert!(store.take(&id).await.unwrap().is_none());
    assert_eq!(f.count().await, 0);
    assert!(
        store
            .replace(&id, pending(f.clock.now() + 1))
            .await
            .is_err()
    );
    assert!(
        store
            .replace(&id, pending(f.clock.now() - 1800))
            .await
            .is_err()
    );
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    sqlx::query("UPDATE pending_signins SET expires_at=expires_at+1")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(store.take(&id).await.is_err());
    assert_eq!(f.count().await, 1);
    assert!(
        store
            .replace("callback-controlled-id", pending(f.clock.now()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn moved_session_and_other_purpose_ciphertexts_cannot_be_consumed() {
    let f = Fixture::new().await;
    let a = tower_sessions::session::Id::default().to_string();
    let b = tower_sessions::session::Id::default().to_string();
    let store = f.store();
    store.replace(&a, pending(f.clock.now())).await.unwrap();
    sqlx::query("INSERT INTO pending_signins SELECT ?1,ciphertext,expires_at FROM pending_signins WHERE session_key=?2")
        .bind(key(&b).as_hex()).bind(key(&a).as_hex()).execute(&f.pool).await.unwrap();
    assert!(store.take(&b).await.is_err());
    assert!(store.take(&a).await.unwrap().is_some());
    let context = Aad::try_from(&purpose::Session { session_id: &a }).unwrap();
    let ciphertext = f
        .ring
        .envelope()
        .encrypt_sync(
            &serde_json::to_vec(&pending(f.clock.now())).unwrap(),
            &context,
        )
        .unwrap();
    f.sql
        .replace_pending_signin(
            key(&a),
            &StoredPendingSignIn {
                ciphertext: STANDARD.encode(ciphertext.as_bytes()),
                expires_at: f.clock.now() + 1800,
            },
            f.clock.now(),
        )
        .await
        .unwrap();
    assert!(store.take(&a).await.is_err());
    assert_eq!(f.count().await, 2);
}
struct LostAcknowledgement {
    sql: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    lose: AtomicBool,
}
#[async_trait::async_trait]
impl PendingSignInStorage for LostAcknowledgement {
    async fn replace_pending_signin(
        &self,
        key: PendingSignInKey,
        record: &StoredPendingSignIn,
        now: i64,
    ) -> storage::Result<()> {
        self.sql.replace_pending_signin(key, record, now).await
    }
    async fn read_pending_signin(
        &self,
        key: PendingSignInKey,
    ) -> storage::Result<Option<StoredPendingSignIn>> {
        self.sql.read_pending_signin(key).await
    }
    async fn consume_pending_signin(
        &self,
        key: PendingSignInKey,
        expected: &StoredPendingSignIn,
    ) -> storage::Result<bool> {
        let row = self.sql.consume_pending_signin(key, expected).await?;
        if self.lose.swap(false, Ordering::SeqCst) {
            return Err(storage::pending_signin::unavailable());
        }
        Ok(row)
    }
}
#[tokio::test]
async fn lost_commit_acknowledgement_and_failed_delete_never_return_pending_material() {
    let f = Fixture::new().await;
    let id = tower_sessions::session::Id::default().to_string();
    let store = SqlPendingSignInStore::new(
        Arc::new(LostAcknowledgement {
            sql: f.sql.clone(),
            lose: AtomicBool::new(true),
        }),
        f.ring.clone(),
    )
    .with_clock(f.clock.clone());
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    assert!(matches!(
        store.take(&id).await,
        Err(StateUnavailable::Unavailable)
    ));
    assert_eq!(f.count().await, 0);
    assert!(store.take(&id).await.unwrap().is_none());
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_pending_delete BEFORE DELETE ON pending_signins BEGIN SELECT RAISE(ABORT,'STATE_MARKER private database detail'); END").execute(&f.pool).await.unwrap();
    let error = store.take(&id).await.unwrap_err();
    assert_eq!(error.to_string(), "sign-in state unavailable");
    assert_eq!(f.count().await, 1);
    sqlx::query("DROP TRIGGER reject_pending_delete")
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(store.take(&id).await.unwrap().is_some());
}

#[tokio::test]
async fn replacement_and_bounded_expiry_cleanup_keep_one_live_pending_record() {
    let f = Fixture::new().await;
    let id = tower_sessions::session::Id::default().to_string();
    let store = f.store();
    for n in 1..=105 {
        f.sql
            .replace_pending_signin(
                PendingSignInKey::from_bytes([n; 32]),
                &StoredPendingSignIn {
                    ciphertext: "YQ==".into(),
                    expires_at: 1,
                },
                0,
            )
            .await
            .unwrap();
    }
    assert_eq!(f.count().await, 105);
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    assert_eq!(f.count().await, 6);
    f.clock.0.fetch_add(1, Ordering::SeqCst);
    store.replace(&id, pending(f.clock.now())).await.unwrap();
    assert_eq!(f.count().await, 1);
    let value = store.take(&id).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(value).unwrap()["issued_at"],
        f.clock.now()
    );
}

#[tokio::test]
async fn lagging_generation_and_wrong_root_preserve_retryable_row() {
    let f = Fixture::new().await;
    let id = tower_sessions::session::Id::default().to_string();
    let newer = Arc::new(
        Keyring::builder("p1")
            .generation(1, [7; 32])
            .generation(2, [8; 32])
            .current(2)
            .build()
            .unwrap(),
    );
    let current = SqlPendingSignInStore::new(f.sql.clone(), newer).with_clock(f.clock.clone());
    current.replace(&id, pending(f.clock.now())).await.unwrap();
    assert!(matches!(
        f.store().take(&id).await,
        Err(StateUnavailable::KeyringRetry)
    ));
    assert_eq!(f.count().await, 1);
    assert!(current.take(&id).await.unwrap().is_some());
    f.store()
        .replace(&id, pending(f.clock.now()))
        .await
        .unwrap();
    let wrong = Arc::new(
        Keyring::builder("p1")
            .generation(1, [9; 32])
            .current(1)
            .build()
            .unwrap(),
    );
    let wrong = SqlPendingSignInStore::new(f.sql.clone(), wrong).with_clock(f.clock.clone());
    assert!(wrong.take(&id).await.is_err());
    assert_eq!(f.count().await, 1);
    assert!(f.store().take(&id).await.unwrap().is_some());
}

struct ReplaceBeforeConsume {
    sql: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    replacement: StoredPendingSignIn,
    once: AtomicBool,
    now: i64,
}
#[async_trait::async_trait]
impl PendingSignInStorage for ReplaceBeforeConsume {
    async fn replace_pending_signin(
        &self,
        key: PendingSignInKey,
        record: &StoredPendingSignIn,
        now: i64,
    ) -> storage::Result<()> {
        self.sql.replace_pending_signin(key, record, now).await
    }
    async fn read_pending_signin(
        &self,
        key: PendingSignInKey,
    ) -> storage::Result<Option<StoredPendingSignIn>> {
        self.sql.read_pending_signin(key).await
    }
    async fn consume_pending_signin(
        &self,
        key: PendingSignInKey,
        expected: &StoredPendingSignIn,
    ) -> storage::Result<bool> {
        if self.once.swap(false, Ordering::SeqCst) {
            self.sql
                .replace_pending_signin(key, &self.replacement, self.now)
                .await?;
        }
        self.sql.consume_pending_signin(key, expected).await
    }
}
#[tokio::test]
async fn replacement_between_decrypt_and_consume_is_not_deleted_or_returned() {
    let f = Fixture::new().await;
    let id = tower_sessions::session::Id::default().to_string();
    let normal = f.store();
    normal.replace(&id, pending(f.clock.now())).await.unwrap();
    let first = f.sql.read_pending_signin(key(&id)).await.unwrap().unwrap();
    f.clock.0.fetch_add(1, Ordering::SeqCst);
    normal.replace(&id, pending(f.clock.now())).await.unwrap();
    let replacement = f.sql.read_pending_signin(key(&id)).await.unwrap().unwrap();
    f.sql
        .replace_pending_signin(key(&id), &first, f.clock.now())
        .await
        .unwrap();
    let interleaved = SqlPendingSignInStore::new(
        Arc::new(ReplaceBeforeConsume {
            sql: f.sql.clone(),
            replacement,
            once: AtomicBool::new(true),
            now: f.clock.now(),
        }),
        f.ring.clone(),
    )
    .with_clock(f.clock.clone());
    assert!(interleaved.take(&id).await.unwrap().is_none());
    assert_eq!(f.count().await, 1);
    let latest = normal.take(&id).await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(latest).unwrap()["issued_at"],
        f.clock.now()
    );
    assert_eq!(f.count().await, 0);
}

#[tokio::test]
async fn concurrent_sqlite_callbacks_exchange_and_revoke_exactly_once() {
    use ghinvite_web::config::apps::SignInApp;
    use ghinvite_web::signin::{Callback, SignInError, SignInService};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let f = Fixture::new().await;
    // Use the real service clock; the persisted issuance originates at begin.
    let store = SqlPendingSignInStore::new(f.sql.clone(), f.ring.clone());
    let second = SqlPendingSignInStore::new(
        Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&f.path)
                .await
                .unwrap(),
        ),
        f.ring.clone(),
    );
    let id = tower_sessions::session::Id::default().to_string();
    let token = "ghu_MARKER_sqlite_callback_012345678901234567890";
    let github = MockServer::start().await;
    for mock in [
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token":token,"token_type":"bearer"})),
            ),
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"id":9,"login":"verified"})),
            ),
        Mock::given(method("DELETE"))
            .and(path("/applications/Iv1.signin/token"))
            .and(header(
                "authorization",
                format!("Basic {}", STANDARD.encode("Iv1.signin:fixture-secret")).as_str(),
            ))
            .and(body_json(json!({"access_token":token})))
            .respond_with(ResponseTemplate::new(204)),
    ] {
        github.register(mock.expect(1)).await;
    }
    let mut app = SignInApp::new(
        "Iv1.signin",
        "fixture-secret",
        "https://app.test/oauth/callback".parse().unwrap(),
    );
    app.web_base_url = Some(github.uri());
    app.api_base_url = Some(github.uri());
    let service = SignInService::new(&app).unwrap();
    let authorize = service.begin(&store, &id).await.unwrap();
    let state = url::Url::parse(authorize.as_str())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let (a, b) = tokio::join!(
        service.complete(
            &store,
            &id,
            Callback::code(state.clone(), "CODE_MARKER_sqlite".into())
        ),
        service.complete(
            &second,
            &id,
            Callback::code(state, "CODE_MARKER_sqlite".into())
        )
    );
    assert_eq!(
        [&a, &b].into_iter().filter(|value| value.is_ok()).count(),
        1
    );
    assert_eq!(
        [&a, &b]
            .into_iter()
            .filter(|value| matches!(value, Err(SignInError::NoPendingSignIn)))
            .count(),
        1
    );
    let identity = a.ok().or_else(|| b.ok()).unwrap();
    assert_eq!(
        serde_json::to_value(identity).unwrap(),
        json!({"github_user_id":9,"login":"verified"})
    );
    github.verify().await;
    assert_eq!(github.received_requests().await.unwrap().len(), 3);
    assert_eq!(f.count().await, 0);
}
