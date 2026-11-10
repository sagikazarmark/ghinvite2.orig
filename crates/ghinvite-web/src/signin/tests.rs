use super::*;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
const CODE: &str = "OAUTH_CODE_MARKER_NOT_IDENTITY";
const TOKEN: &str = "ghu_MARKER_CALLBACK_MUST_NOT_ESCAPE_12345678901234567890";
const SECRET: &str = "CLIENT_SECRET_MARKER_NOT_IDENTITY";
#[derive(Default)]
struct Store {
    pending: Mutex<HashMap<String, PendingSignIn>>,
    fail: AtomicBool,
}
#[async_trait]
impl PendingSignInStore for Store {
    async fn replace(&self, id: &str, pending: PendingSignIn) -> Result<(), StateUnavailable> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StateUnavailable::Unavailable);
        }
        self.pending.lock().unwrap().insert(id.into(), pending);
        Ok(())
    }
    async fn take(&self, id: &str) -> Result<Option<PendingSignIn>, StateUnavailable> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(StateUnavailable::Unavailable);
        }
        let pending = self.pending.lock().unwrap().remove(id);
        // Force overlap: the other callback runs before the first exchange.
        tokio::task::yield_now().await;
        Ok(pending)
    }
}
struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
/// A sign-in service against a local GitHub stand-in, with its mocks mounted
/// in order. Each mock answers once.
async fn service(mocks: Vec<Mock>) -> (SignInService, MockServer, Arc<TestClock>) {
    let server = MockServer::start().await;
    for mock in mocks {
        server.register(mock).await;
    }
    let mut app = SignInApp::new(
        "Iv1.signin",
        SECRET,
        url::Url::parse("https://app.test/oauth/callback").unwrap(),
    );
    app.web_base_url = Some(server.uri());
    app.api_base_url = Some(server.uri());
    let mut service = SignInService::new(&app).unwrap();
    let clock = Arc::new(TestClock(AtomicI64::new(100)));
    service.clock = clock.clone();
    (service, server, clock)
}
/// Every mounted mock was used as expected, and nothing else was requested.
async fn assert_exhausted(server: &MockServer, requests: usize) {
    server.verify().await;
    assert_eq!(server.received_requests().await.unwrap().len(), requests);
}
fn exchange() -> Mock {
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token":TOKEN,"token_type":"bearer","refresh_token":"ghr_MARKER_DISCARD"}),
        ))
        .up_to_n_times(1)
        .expect(1)
}
fn user_answer(answer: ResponseTemplate) -> Mock {
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(answer)
        .up_to_n_times(1)
        .expect(1)
}
fn user() -> Mock {
    user_answer(
        ResponseTemplate::new(200).set_body_json(
            json!({"id":7,"login":"octocat","avatar_url":"https://example.com/unused"}),
        ),
    )
}
fn emails(answer: ResponseTemplate) -> Mock {
    Mock::given(method("GET"))
        .and(path("/user/emails"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(answer)
        .up_to_n_times(1)
        .expect(1)
}
fn revoke_answer(answer: ResponseTemplate) -> Mock {
    use base64::Engine;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.signin/token"))
        .and(header(
            "authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("Iv1.signin:{SECRET}"))
            )
            .as_str(),
        ))
        .and(body_json(json!({"access_token":TOKEN})))
        .respond_with(answer)
        .up_to_n_times(1)
        .expect(1)
}
fn revoke(status: u16) -> Mock {
    revoke_answer(ResponseTemplate::new(status))
}
async fn begin(service: &SignInService, store: &Store) -> String {
    let authorize = service.begin(store, "trusted-session-id").await.unwrap();
    let url = url::Url::parse(authorize.as_str()).unwrap();
    assert!(!url.query_pairs().any(|(k, _)| k == "scope"));
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "code_challenge_method" && v == "S256")
    );
    let pending = store.pending.lock().unwrap();
    let data = &pending["trusted-session-id"];
    use base64::Engine;
    use sha2::Digest;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(data.verifier.as_bytes()));
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "code_challenge" && v == challenge)
    );
    assert_eq!(format!("{data:?}"), "PendingSignIn([REDACTED])");
    url.query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned()
}
#[tokio::test]
async fn callback_reads_only_user_revokes_exact_token_and_returns_only_identity() {
    let (service, server, _) = service(vec![exchange(), user(), revoke(204)]).await;
    let store = Store::default();
    let state = begin(&service, &store).await;
    let verifier = store.pending.lock().unwrap()["trusted-session-id"]
        .verifier
        .to_string();
    let identity = service
        .complete(
            &store,
            "trusted-session-id",
            Callback::code(state.clone(), CODE.into()),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&identity).unwrap(),
        json!({"github_user_id":7,"login":"octocat"})
    );
    assert!(store.pending.lock().unwrap().is_empty());
    assert_eq!(
        service
            .complete(
                &store,
                "trusted-session-id",
                Callback::code(state, CODE.into())
            )
            .await
            .unwrap_err(),
        SignInError::NoPendingSignIn
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|r| (r.method.as_str(), r.url.path()))
            .collect::<Vec<_>>(),
        [
            ("POST", "/login/oauth/access_token"),
            ("GET", "/user"),
            ("DELETE", "/applications/Iv1.signin/token")
        ]
    );
    let form: HashMap<_, _> = url::form_urlencoded::parse(&requests[0].body)
        .into_owned()
        .collect();
    assert_eq!(form["code"], CODE);
    assert_eq!(form["code_verifier"], verifier);
    assert_eq!(form["redirect_uri"], "https://app.test/oauth/callback");
    assert_exhausted(&server, 3).await;
}
#[tokio::test]
async fn concurrent_callbacks_exchange_only_once() {
    let (service, server, _) = service(vec![exchange(), user(), revoke(204)]).await;
    let store = Store::default();
    let state = begin(&service, &store).await;
    let (a, b) = tokio::join!(
        service.complete(
            &store,
            "trusted-session-id",
            Callback::code(state.clone(), CODE.into())
        ),
        service.complete(
            &store,
            "trusted-session-id",
            Callback::code(state, CODE.into())
        )
    );
    assert!(matches!(
        (a, b),
        (Ok(_), Err(SignInError::NoPendingSignIn)) | (Err(SignInError::NoPendingSignIn), Ok(_))
    ));
    assert_exhausted(&server, 3).await;
}
#[tokio::test]
async fn every_user_read_failure_still_revokes_and_never_authenticates() {
    for response in [
        ResponseTemplate::new(401),
        ResponseTemplate::new(200).set_body_json(json!({"id":0,"login":"octocat"})),
        ResponseTemplate::new(200).set_body_json(json!({"id":7,"login":TOKEN})),
        ResponseTemplate::new(200).set_body_json(json!({"message":TOKEN})),
    ] {
        let (service, server, _) =
            service(vec![exchange(), user_answer(response), revoke(500)]).await;
        let store = Store::default();
        let state = begin(&service, &store).await;
        let error = service
            .complete(
                &store,
                "trusted-session-id",
                Callback::code(state, CODE.into()),
            )
            .await
            .unwrap_err();
        assert_eq!(error, SignInError::IdentityUnavailable);
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert_exhausted(&server, 3).await;
    }
}
#[tokio::test]
async fn mismatch_denial_expiry_future_and_cross_session_consume_without_exchange() {
    let (service, server, clock) = service(vec![]).await;
    let store = Store::default();
    for age in [1800, 1801, -1] {
        clock.0.store(100, Ordering::SeqCst);
        let state = begin(&service, &store).await;
        clock.0.store(100 + age, Ordering::SeqCst);
        assert_eq!(
            service
                .complete(
                    &store,
                    "trusted-session-id",
                    Callback::code(state, CODE.into())
                )
                .await
                .unwrap_err(),
            SignInError::InvalidState
        );
        assert!(store.pending.lock().unwrap().is_empty());
    }
    clock.0.store(100, Ordering::SeqCst);
    let state = begin(&service, &store).await;
    assert_eq!(
        service
            .complete(&store, "other-session", Callback::code(state, CODE.into()))
            .await
            .unwrap_err(),
        SignInError::NoPendingSignIn
    );
    assert_eq!(
        service
            .complete(
                &store,
                "trusted-session-id",
                Callback::code("wrong".into(), CODE.into())
            )
            .await
            .unwrap_err(),
        SignInError::InvalidState
    );
    let state = begin(&service, &store).await;
    assert_eq!(
        service
            .complete(
                &store,
                "trusted-session-id",
                Callback::denied(state, "access_denied")
            )
            .await
            .unwrap_err(),
        SignInError::Declined
    );
    assert!(store.pending.lock().unwrap().is_empty());
    assert_exhausted(&server, 0).await;
}
#[tokio::test]
async fn store_failures_never_redirect_or_exchange() {
    let (service, server, _) = service(vec![]).await;
    let store = Store::default();
    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        service.begin(&store, "trusted-session-id").await,
        Err(SignInError::StateUnavailable)
    ));
    assert_eq!(
        service
            .complete(
                &store,
                "trusted-session-id",
                Callback::code("state".into(), CODE.into())
            )
            .await
            .unwrap_err(),
        SignInError::StateUnavailable
    );
    assert_exhausted(&server, 0).await;
}
#[tokio::test]
async fn exchange_failure_is_consumed_and_does_not_revoke_without_token() {
    let (service, server, _) = service(vec![
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"error":CODE,"error_description":TOKEN})),
            )
            .expect(1),
    ])
    .await;
    let store = Store::default();
    let state = begin(&service, &store).await;
    assert_eq!(
        service
            .complete(
                &store,
                "trusted-session-id",
                Callback::code(state, CODE.into())
            )
            .await
            .unwrap_err(),
        SignInError::Exchange
    );
    assert!(store.pending.lock().unwrap().is_empty());
    assert_exhausted(&server, 1).await;
}
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
#[tokio::test]
async fn revoke_failure_logs_fixed_kind_and_preserves_identity_without_marker() {
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // This lib-test process has one diagnostic collector. A global dispatch
    // also observes work polled while other tests initialize callsites.
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let failure = revoke_answer(
        ResponseTemplate::new(422)
            .set_body_json(json!({"message":format!("{TOKEN} {CODE} {SECRET}")})),
    );
    let (service, server, _) = service(vec![exchange(), user(), failure]).await;
    let store = Store::default();
    let state = begin(&service, &store).await;
    let identity = service
        .complete(
            &store,
            "trusted-session-id",
            Callback::code(state, CODE.into()),
        )
        .await
        .unwrap();
    let recorded = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(recorded.contains("sign-in token revocation failed"));
    assert!(recorded.contains("error_kind"));
    assert!(recorded.contains("gh:api:validation"));
    let all = format!(
        "{recorded} {} {identity:?}",
        serde_json::to_string(&identity).unwrap()
    );
    for marker in [CODE, TOKEN, SECRET, "ghr_MARKER_DISCARD"] {
        assert!(!all.contains(marker));
    }
    assert_exhausted(&server, 3).await;
}

struct Restricted(DomainRestriction);
#[async_trait]
impl DomainContext for Restricted {
    async fn restriction(&self, user_id: u64) -> Result<Option<DomainRestriction>, SignInError> {
        assert_eq!(user_id, 7);
        Ok(Some(self.0.clone()))
    }
}
fn restriction() -> Restricted {
    Restricted(DomainRestriction {
        link_id: ghinvite_core::InvitationLinkId::new(),
        rules: ["EXAMPLE.com.", "*.bücher.example"]
            .into_iter()
            .map(|r| ghinvite_core::email_domain::DomainRule::parse(r).unwrap())
            .collect(),
    })
}
#[tokio::test]
async fn restricted_callback_retains_only_verified_normalized_matching_domains_and_revokes() {
    // `u@sub.bücher.example` would match `*.bücher.example` but is not verified.
    let emails = emails(ResponseTemplate::new(200).set_body_json(json!([
        {"email":"PRIVATE_MARKER@example.COM.","verified":true,"primary":true,"visibility":null},
        {"email":"other@example.com","verified":true,"primary":false,"visibility":null},
        {"email":"private@branch.BÜCHER.example.","verified":true,"primary":false,"visibility":null},
        {"email":"PRIVATE_UNMATCHED@secret.example","verified":true,"primary":false,"visibility":null},
        {"email":"n@users.noreply.github.com","verified":true,"primary":false,"visibility":null},
        {"email":"u@sub.bücher.example","verified":false,"primary":false,"visibility":null}
    ])));
    let (service, server, _) = service(vec![exchange(), user(), emails, revoke(204)]).await;
    let store = Store::default();
    let state = begin(&service, &store).await;
    let restriction = restriction();
    let result = service
        .complete_scoped(
            &store,
            "trusted-session-id",
            Callback::code(state, CODE.into()),
            Some(&restriction),
        )
        .await
        .unwrap();
    let evidence = result.domain_evidence.as_ref().unwrap();
    assert_eq!(evidence.link_id, restriction.0.link_id);
    assert_eq!(
        evidence
            .matched_domains
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>(),
        vec!["branch.xn--bcher-kva.example", "example.com"]
    );
    assert_eq!(evidence.checked_at.timestamp(), 100);
    let json = serde_json::to_string(&result).unwrap();
    for marker in [
        "PRIVATE",
        "secret.example",
        "noreply",
        TOKEN,
        CODE,
        SECRET,
        "@",
    ] {
        assert!(!json.contains(marker), "{marker}");
    }
    assert_exhausted(&server, 4).await;
}
#[tokio::test]
async fn restricted_email_failure_still_revokes_without_returning_identity_or_evidence() {
    for status in [401, 403, 503] {
        let (service, server, _) = service(vec![
            exchange(),
            user(),
            emails(ResponseTemplate::new(status)),
            revoke(204),
        ])
        .await;
        let store = Store::default();
        let state = begin(&service, &store).await;
        assert_eq!(
            service
                .complete_scoped(
                    &store,
                    "trusted-session-id",
                    Callback::code(state, CODE.into()),
                    Some(&restriction())
                )
                .await
                .unwrap_err(),
            SignInError::DomainUnavailable
        );
        assert_exhausted(&server, 4).await;
    }
}

/// More than one page of addresses is `Incomplete` in octologin: no evidence
/// is kept from a partial list, the next page is never read, and the token is
/// still revoked.
#[tokio::test]
async fn restricted_email_list_longer_than_one_page_is_unavailable_and_still_revokes() {
    let first = emails(
        ResponseTemplate::new(200)
            .set_body_json(json!([{"email":"private@example.com","verified":true,"primary":true}]))
            .insert_header(
                "link",
                "<https://api.github.com/user/emails?per_page=100&page=2>; rel=\"next\"",
            ),
    );
    let (service, server, _) = service(vec![exchange(), user(), first, revoke(204)]).await;
    let store = Store::default();
    let state = begin(&service, &store).await;
    assert_eq!(
        service
            .complete_scoped(
                &store,
                "trusted-session-id",
                Callback::code(state, CODE.into()),
                Some(&restriction())
            )
            .await
            .unwrap_err(),
        SignInError::DomainUnavailable
    );
    assert_exhausted(&server, 4).await;
}

#[tokio::test]
async fn github_profile_facts_are_optional_numeric_identity_bound_and_never_session_identity() {
    for (created, expected) in [
        (json!("2008-01-01T00:00:00Z"), Some(1_199_145_600)),
        // octologin keeps no pre-1970 creation time (none exists on GitHub).
        (json!("1960-01-01T00:00:00Z"), None),
        (json!("2099-01-01T00:00:00Z"), None),
        (json!("invalid"), None),
        (serde_json::Value::Null, None),
    ] {
        let profile = user_answer(ResponseTemplate::new(200).set_body_json(
            json!({"id":7,"login":"octocat","avatar_url":"https://untrusted.invalid/credential-echo","created_at":created}),
        ));
        let (service, server, clock) = service(vec![exchange(), profile, revoke(204)]).await;
        clock.0.store(1_790_000_000, Ordering::SeqCst);
        let store = Store::default();
        let state = begin(&service, &store).await;
        let completed = service
            .complete_scoped(
                &store,
                "trusted-session-id",
                Callback::code(state, CODE.into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(completed.identity.github_user_id, 7);
        assert_eq!(
            completed.avatar_url.as_deref(),
            Some("https://avatars.githubusercontent.com/u/7?s=80&v=4")
        );
        assert_eq!(
            completed.github_created_at.map(|at| at.timestamp()),
            expected
        );
        assert_eq!(
            serde_json::to_value(completed.identity).unwrap(),
            json!({"github_user_id":7,"login":"octocat"})
        );
        assert_exhausted(&server, 3).await;
    }
}

/// The callback's `error` is classified by octologin's `OAuthErrorCode`;
/// none of its text survives, and pending state is consumed without I/O.
#[tokio::test]
async fn provider_errors_are_classified_consumed_and_carry_no_text() {
    let (service, server, _) = service(vec![]).await;
    let store = Store::default();
    for (error, expected) in [
        ("access_denied", SignInError::Declined),
        (
            "redirect_uri_mismatch",
            SignInError::Provider(OAuthErrorCode::RedirectUriMismatch),
        ),
        (
            "contact PRIVATE_MARKER support",
            SignInError::Provider(OAuthErrorCode::Unrecognized),
        ),
    ] {
        let state = begin(&service, &store).await;
        let result = service
            .complete(&store, "trusted-session-id", Callback::denied(state, error))
            .await
            .unwrap_err();
        assert_eq!(result, expected);
        assert!(!format!("{result:?} {result}").contains("PRIVATE_MARKER"));
        assert!(store.pending.lock().unwrap().is_empty());
    }
    assert_exhausted(&server, 0).await;
}

/// GM-00 (PD-9): the encrypted `pending_signins` plaintext. Its `state` and
/// `verifier` come from octologin's `PendingAuthorization` (32 random bytes,
/// base64url without padding, as gh-toolkit's were) and keep this shape.
#[tokio::test]
async fn golden_pending_signin() {
    let pending = PendingSignIn {
        state: "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE".into(),
        verifier: Zeroizing::new("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into()),
        issued_at: 1_790_000_000,
    };
    let fixture = crate::golden::json("pending_signin.json", &pending);
    // The real begin() produces the same fields and encodings.
    let (service, server, _) = service(vec![]).await;
    let store = Store::default();
    begin(&service, &store).await;
    let generated = store
        .pending
        .lock()
        .unwrap()
        .remove("trusted-session-id")
        .unwrap();
    let base64url = |value: &str| {
        value.len() == 43
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    assert!(base64url(&generated.state) && base64url(&generated.verifier));
    assert!(base64url(&fixture.state) && base64url(&fixture.verifier));
    assert_eq!(generated.issued_at, 100);
    let keys = |value: &PendingSignIn| {
        serde_json::to_value(value)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&generated), keys(&fixture));
    assert_exhausted(&server, 0).await;
}
