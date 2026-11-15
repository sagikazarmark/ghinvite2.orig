//! Actual SQLite, envelope and protected-session boundaries for operator rotation.
use anykms_core::Ciphertext;
use ghinvite_core::purpose;
use ghinvite_core::storage::{
    ContinuationStorage, Error, Result,
    attempt_continuations::StoredContinuation,
    continuation_rotation::{PAGE_SIZE, Phase, ScanRow},
};
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_web::continuation_rotation::{Failure, Report, page};
use kekring::{Aad, Keyring};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn ring(current: u32, keys: &[(u32, u8)]) -> Keyring {
    let mut b = Keyring::builder("p1");
    for (version, key) in keys {
        b = b.generation(*version, [*key; 32]);
    }
    b.current(current).build().unwrap()
}
fn context(binding: &str) -> Aad {
    Aad::try_from(&purpose::AttemptContinuation {
        scope: "rotate-test",
        binding,
    })
    .unwrap()
}
fn seal(ring: &Keyring, binding: &str) -> String {
    ring.envelope()
        .encrypt_sync(b"ROTATION_PRIVATE_PLAINTEXT", &context(binding))
        .unwrap()
        .to_base64()
}
async fn seed(s: &dyn ContinuationStorage, r: &Keyring, n: usize) {
    for i in 0..n {
        let id = format!("row-{i}");
        s.retain_attempt_continuation("rotate-test", &id, &id, &seal(r, &id), 100, 0)
            .await
            .unwrap();
    }
}
async fn pass(s: &dyn ContinuationStorage, r: &Keyring, phase: Phase) -> Report {
    let mut token = None;
    for _ in 0..20 {
        let report = page(s, r, 42, phase, token.as_deref(), 200).await.unwrap();
        if report.phase_finished {
            return report;
        }
        assert!(!report.complete);
        token = report.next;
    }
    panic!("bounded fixture must finish");
}
#[tokio::test]
async fn add_promote_rewrap_verify_remove_includes_expired_rows_without_changing_retention() {
    let s = SqlxStorage::in_memory().await.unwrap();
    let old = ring(1, &[(1, 11)]);
    let current = ring(2, &[(1, 11), (2, 22)]);
    seed(&s, &old, PAGE_SIZE + 2).await;
    let before = s.continuation_rotation_page(None).await.unwrap();
    let needs = pass(&s, &current, Phase::Verify).await;
    assert_eq!(needs.counts.old, (PAGE_SIZE + 2) as u64);
    assert!(!needs.complete);
    let rewritten = pass(&s, &current, Phase::Rewrap).await;
    assert_eq!(rewritten.counts.rewrapped, (PAGE_SIZE + 2) as u64);
    assert!(!rewritten.complete, "rewrap is never the completion gate");
    let done = pass(&s, &current, Phase::Verify).await;
    assert!(done.complete);
    assert_eq!(done.counts.old, 0);
    assert_eq!(done.counts.bad, 0);
    let removed = ring(2, &[(2, 22)]);
    for after in s.continuation_rotation_page(None).await.unwrap() {
        let prior = before.iter().find(|r| r.id == after.id).unwrap();
        assert_eq!(
            (&prior.scope, &prior.id, &prior.binding, prior.expires_at),
            (&after.scope, &after.id, &after.binding, after.expires_at)
        );
        assert_ne!(prior.payload, after.payload);
        let ct = Ciphertext::from_base64(after.payload.as_ref().unwrap()).unwrap();
        assert_eq!(
            &*removed
                .envelope()
                .decrypt_sync(&ct, &context(after.binding.as_ref().unwrap()))
                .unwrap(),
            b"ROTATION_PRIVATE_PLAINTEXT"
        );
    }
    assert!(
        s.get_attempt_continuation("rotate-test", "row-0", 200)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn corrupt_current_unknown_removed_and_cross_context_rows_block_completion_without_changes() {
    let s = SqlxStorage::in_memory().await.unwrap();
    let current = ring(2, &[(1, 11), (2, 22)]);
    for (id, payload) in [
        ("malformed", "not-an-envelope".into()),
        ("unknown", seal(&ring(3, &[(3, 33)]), "unknown")),
        ("copied", seal(&current, "different-context")),
        ("current-corrupt", {
            let mut raw = seal(&current, "current-corrupt").into_bytes();
            let i = raw.len() - 4;
            raw[i] = if raw[i] == b'A' { b'B' } else { b'A' };
            String::from_utf8(raw).unwrap()
        }),
    ] {
        s.retain_attempt_continuation("rotate-test", id, id, &payload, 100, 0)
            .await
            .unwrap();
    }
    let before = s.continuation_rotation_page(None).await.unwrap();
    let result = pass(&s, &current, Phase::Rewrap).await;
    assert_eq!(result.counts.bad, 4);
    assert_eq!(result.counts.rewrapped, 0);
    let report = pass(&s, &current, Phase::Verify).await;
    assert!(!report.complete);
    assert_eq!(report.counts.bad, 4);
    let json = serde_json::to_string(&report).unwrap();
    for secret in [
        "ROTATION_PRIVATE_PLAINTEXT",
        "different-context",
        "not-an-envelope",
    ] {
        assert!(!json.contains(secret));
    }
    for (a, b) in before
        .iter()
        .zip(s.continuation_rotation_page(None).await.unwrap())
    {
        assert_eq!(a.payload, b.payload);
    }
    let s = SqlxStorage::in_memory().await.unwrap();
    seed(&s, &ring(1, &[(1, 11)]), 1).await;
    assert_eq!(
        pass(&s, &ring(2, &[(2, 22)]), Phase::Verify)
            .await
            .counts
            .bad,
        1
    );
}
#[tokio::test]
async fn sealed_progress_is_operator_phase_fingerprint_and_fixed_lifetime_bound() {
    let s = SqlxStorage::in_memory().await.unwrap();
    let r = ring(2, &[(1, 11), (2, 22)]);
    seed(&s, &r, 6).await;
    let first = page(&s, &r, 42, Phase::Verify, None, 200).await.unwrap();
    let token = first.next.unwrap();
    for (operator, phase, now) in [
        (43, Phase::Verify, 200),
        (42, Phase::Rewrap, 200),
        (42, Phase::Verify, 3801),
        (42, Phase::Verify, 199),
    ] {
        assert_eq!(
            page(&s, &r, operator, phase, Some(&token), now).await.err(),
            Some(Failure::InvalidProgress)
        );
    }
    let changed = ring(2, &[(1, 11), (2, 22), (3, 33)]);
    assert_eq!(
        page(&s, &changed, 42, Phase::Verify, Some(&token), 200)
            .await
            .err(),
        Some(Failure::InvalidProgress)
    );
    assert_eq!(
        page(&s, &r, 42, Phase::Verify, Some("bad"), 200)
            .await
            .err(),
        Some(Failure::InvalidProgress)
    );
    assert!(
        page(&s, &r, 42, Phase::Verify, Some(&token), 3800)
            .await
            .unwrap()
            .complete
    );
}
#[tokio::test]
async fn any_mutation_between_pages_requires_full_restart_even_when_remaining_rows_are_current() {
    for delete in [false, true] {
        let s = SqlxStorage::in_memory().await.unwrap();
        let r = ring(2, &[(1, 11), (2, 22)]);
        seed(&s, &r, 6).await;
        let first = page(&s, &r, 42, Phase::Verify, None, 200).await.unwrap();
        if delete {
            s.release_attempt_continuation("rotate-test", "row-0")
                .await
                .unwrap();
        } else {
            s.retain_attempt_continuation("rotate-test", "new", "new", &seal(&r, "new"), 100, 0)
                .await
                .unwrap();
        }
        assert_eq!(
            page(&s, &r, 42, Phase::Verify, first.next.as_deref(), 200)
                .await
                .err(),
            Some(Failure::RestartRequired)
        );
        assert!(pass(&s, &r, Phase::Verify).await.complete);
    }
}
struct FaultStore {
    inner: SqlxStorage,
    once: AtomicBool,
    lost_ack: bool,
}
#[async_trait::async_trait]
impl ContinuationStorage for FaultStore {
    async fn retain_attempt_continuation(
        &self,
        s: &str,
        id: &str,
        b: &str,
        p: &str,
        e: i64,
        n: i64,
    ) -> Result<StoredContinuation> {
        self.inner
            .retain_attempt_continuation(s, id, b, p, e, n)
            .await
    }
    async fn get_attempt_continuation(
        &self,
        s: &str,
        id: &str,
        n: i64,
    ) -> Result<Option<StoredContinuation>> {
        self.inner.get_attempt_continuation(s, id, n).await
    }
    async fn list_attempt_continuations(&self, s: &str, n: i64) -> Result<Vec<StoredContinuation>> {
        self.inner.list_attempt_continuations(s, n).await
    }
    async fn release_attempt_continuation(&self, s: &str, id: &str) -> Result<()> {
        self.inner.release_attempt_continuation(s, id).await
    }
    async fn continuation_rotation_page(&self, a: Option<i64>) -> Result<Vec<ScanRow>> {
        self.inner.continuation_rotation_page(a).await
    }
    async fn continuation_rotation_epoch(&self) -> Result<i64> {
        self.inner.continuation_rotation_epoch().await
    }
    async fn rewrap_attempt_continuation(
        &self,
        old: &ScanRow,
        new: &str,
        epoch: i64,
    ) -> Result<bool> {
        if self.once.swap(false, Ordering::SeqCst) {
            if self.lost_ack {
                assert!(
                    self.inner
                        .rewrap_attempt_continuation(old, new, epoch)
                        .await?
                );
                return Err(Error::Database("ROTATION_PRIVATE_ACK_ERROR".into()));
            }
            // A real competing update wins. Its bytes and original retention
            // must survive the stale sweep (not merely an injected false bool).
            assert!(
                self.inner
                    .rewrap_attempt_continuation(old, "concurrent-writer", epoch)
                    .await?
            );
        }
        self.inner
            .rewrap_attempt_continuation(old, new, epoch)
            .await
    }
}
#[tokio::test]
async fn committed_lost_ack_recovers_on_fresh_pass_and_real_cas_race_never_overwrites_winner() {
    for lost_ack in [false, true] {
        let s = FaultStore {
            inner: SqlxStorage::in_memory().await.unwrap(),
            once: AtomicBool::new(true),
            lost_ack,
        };
        let r = ring(2, &[(1, 11), (2, 22)]);
        seed(&s, &ring(1, &[(1, 11)]), 1).await;
        let failure = page(&s, &r, 42, Phase::Rewrap, None, 200)
            .await
            .err()
            .unwrap();
        assert_eq!(
            failure,
            if lost_ack {
                Failure::Unavailable
            } else {
                Failure::RestartRequired
            }
        );
        let fresh = pass(&s, &r, Phase::Verify).await;
        assert_eq!(fresh.complete, lost_ack);
        if !lost_ack {
            assert_eq!(
                s.continuation_rotation_page(None).await.unwrap()[0]
                    .payload
                    .as_deref(),
                Some("concurrent-writer")
            );
        }
    }
}
#[tokio::test]
async fn empty_table_is_verified_not_inferred_from_expiry() {
    let s = SqlxStorage::in_memory().await.unwrap();
    let r = ring(1, &[(1, 11)]);
    let report = pass(&s, &r, Phase::Verify).await;
    assert!(report.complete);
    assert_eq!(report.counts.checked, 0);
}

mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
async fn identity(
    store: &ghinvite_web::session_store::ProtectedStore<ghinvite_web_server::SqliteBackend>,
    id: u64,
) -> tower_sessions::session::Id {
    let s = tower_sessions::Session::new(None, Arc::new(store.clone()), None);
    ghinvite_web::session_store::establish_authenticated_lifetime(&s)
        .await
        .unwrap();
    ghinvite_web::session::save(
        &s,
        &ghinvite_web::session::Session {
            user_id: id,
            login: format!("u-{id}"),
            csrf_token: Some("rotation-csrf".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    s.save().await.unwrap();
    s.id().unwrap()
}
#[tokio::test]
async fn protected_operator_route_rechecks_authority_csrf_and_body_only_progress() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let r = ring(2, &[(1, 11), (2, 22)]);
    seed(s.as_ref(), &ring(1, &[(1, 11)]), 1).await;
    let mut config = ghinvite_web::WebConfig::for_local_dev_with_secret([17; 32]);
    config.keyring = Arc::new(r);
    config.operator_ids.insert(42);
    let backend = ghinvite_web_server::SqliteBackend::new(
        sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap(),
    );
    backend.migrate().await.unwrap();
    let store = ghinvite_web::session_store::ProtectedStore::new(backend, config.keyring.clone());
    let op = identity(&store, 42).await;
    let admin = identity(&store, 77).await;
    let state = crate::common::sign_in::app_state(
        s.clone(),
        Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
        Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        config,
    );
    let app = common::build_app(state, store);
    let epoch = s.continuation_rotation_epoch().await.unwrap();
    for (cookie, body, expected) in [
        (None, "phase=rewrap", StatusCode::NOT_FOUND),
        (Some(admin), "phase=rewrap", StatusCode::NOT_FOUND),
        (
            Some(op),
            "phase=rewrap&csrf_token=wrong",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut q = Request::post("/console/keyring/continuations")
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(id) = cookie {
            q = q.header("cookie", format!("id={id}"));
        }
        let response = app
            .clone()
            .oneshot(q.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert!(
            response.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
        assert_eq!(s.continuation_rotation_epoch().await.unwrap(), epoch);
    }
    for phase in ["rewrap", "verify"] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/console/keyring/continuations")
                    .header("cookie", format!("id={op}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "phase={phase}&csrf_token=rotation-csrf"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("no-store")
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["complete"], phase == "verify");
        assert!(!String::from_utf8_lossy(&bytes).contains("ROTATION_PRIVATE_PLAINTEXT"));
    }
}

#[tokio::test]
async fn raw_insert_behind_cursor_and_oversized_rows_cannot_pass_verification() {
    let path = std::env::temp_dir().join(format!(
        "rotation-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let s = SqlxStorage::at_path(&path).await.unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    let r = ring(2, &[(1, 11), (2, 22)]);
    seed(&s, &r, 6).await;
    let first = page(&s, &r, 42, Phase::Verify, None, 200).await.unwrap();
    sqlx::query("INSERT INTO attempt_continuations(rowid,scope,id,binding,payload,expires_at) VALUES(-1,'rotate-test','behind','behind',?,100)")
        .bind(seal(&ring(1,&[(1,11)]),"behind")).execute(&pool).await.unwrap();
    assert_eq!(
        page(&s, &r, 42, Phase::Verify, first.next.as_deref(), 200)
            .await
            .err(),
        Some(Failure::RestartRequired)
    );
    let report = pass(&s, &r, Phase::Verify).await;
    assert_eq!(report.counts.old, 1);
    assert!(!report.complete);
    sqlx::query(
        "UPDATE attempt_continuations SET payload=printf('%.*c',4194305,'x') WHERE id='behind'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let row = s.continuation_rotation_page(None).await.unwrap().remove(0);
    assert_eq!(row.oversized, 1);
    assert!(row.payload.is_none());
    let report = pass(&s, &r, Phase::Verify).await;
    assert_eq!(report.counts.bad, 1);
    assert!(!report.complete);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT length(payload) FROM attempt_continuations WHERE id='behind'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        4194305
    );
    pool.close().await;
    drop(s);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn epoch_overflow_fails_original_write_atomically() {
    let path = std::env::temp_dir().join(format!(
        "rotation-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let s = SqlxStorage::at_path(&path).await.unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    seed(&s, &ring(1, &[(1, 11)]), 1).await;
    let before = s.continuation_rotation_page(None).await.unwrap().remove(0);
    sqlx::query("UPDATE continuation_rotation_epoch SET revision=9007199254740991")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        s.rewrap_attempt_continuation(&before, "replacement", 9007199254740991)
            .await
            .is_err()
    );
    assert!(
        s.release_attempt_continuation("rotate-test", "row-0")
            .await
            .is_err()
    );
    let after = s.continuation_rotation_page(None).await.unwrap().remove(0);
    assert_eq!(after.payload, before.payload);
    assert_eq!(after.revision, 9007199254740991);
    pool.close().await;
    drop(s);
    std::fs::remove_file(path).unwrap();
}
