//! Concrete HTTP composition proof; fake owner supplies immutable rules and receipts.
use super::*;
use tower_sessions::SessionStore;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
const TOKEN: &str = "ghu_DOMAIN_TOKEN_MARKER_not_for_session_1234567890";
/// One sign-in's requests, each answered once and in this order.
fn script(id: u64, emails: serde_json::Value, fail: bool) -> Vec<Mock> {
    vec![
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token":TOKEN,"token_type":"bearer"})),
            ),
        Mock::given(method("GET")).and(path("/user")).respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id":id,"login":"domainuser"})),
        ),
        Mock::given(method("GET"))
            .and(path("/user/emails"))
            .and(query_param("per_page", "100"))
            .respond_with(if fail {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(emails)
            }),
        Mock::given(method("DELETE"))
            .and(path("/applications/Iv1.signin/token"))
            .respond_with(ResponseTemplate::new(204)),
    ]
}
async fn github(script: Vec<Mock>) -> MockServer {
    let server = MockServer::start().await;
    for mock in script {
        server.register(mock.up_to_n_times(1).expect(1)).await;
    }
    // Anything unscripted fails `verify`.
    server
        .register(
            Mock::given(wiremock::matchers::any())
                .respond_with(ResponseTemplate::new(500))
                .with_priority(u8::MAX)
                .expect(0),
        )
        .await;
    server
}
/// Every scripted request was made, and nothing else.
async fn assert_exhausted(github: &MockServer) {
    github.verify().await;
}
async fn fixture(
    script: Vec<Mock>,
) -> (
    axum::Router,
    FakeLinkAuthority,
    tower_sessions::MemoryStore,
    MockServer,
) {
    let authority = FakeLinkAuthority::start().await;
    let mut link = active_link(ACTIVE_SLUG);
    link.email_domains =
        vec![ghinvite_core::email_domain::DomainRule::parse("EXAMPLE.com.").unwrap()];
    authority.seed_link(&link);
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    common::capability::register(storage.as_ref(), &link).await;
    let github = github(script).await;
    let config = WebConfig {
        signin_apps: Some(common::apps::signin_app(&github)),
        ..WebConfig::for_local_dev_with_secret([7; 32])
    };
    let sessions = tower_sessions::MemoryStore::default();
    let app = build_app(
        crate::common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(vec![])),
            authority.client(),
            config,
        ),
        sessions.clone(),
    );
    (app, authority, sessions, github)
}
async fn get(app: &axum::Router, cookie: &str, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::get(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn login(app: &axum::Router, cookie: &str) -> (String, String) {
    let response = get(app, cookie, "/login?return_to=%2Fjoin%3Ffresh%3Dtrue").await;
    assert_eq!(response.status(), 303);
    let url = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    assert!(!url.query_pairs().any(|(k, _)| k == "scope"));
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    (
        common::sign_in::session_cookie(&response, Some(cookie.into())),
        state,
    )
}
async fn record(
    store: &tower_sessions::MemoryStore,
    cookie: &str,
) -> tower_sessions::session::Record {
    let id = cookie.split_once('=').unwrap().1.parse().unwrap();
    store.load(&id).await.unwrap().unwrap()
}
fn email(value: &str, verified: bool) -> serde_json::Value {
    serde_json::json!({"email":value,"verified":verified,"primary":true,"visibility":null})
}
#[tokio::test]
async fn typed_domain_callback_rotates_identity_and_submits_only_matched_domains() {
    let (app, authority, sessions, github) = fixture(script(
        REQUESTER_ID,
        serde_json::json!([
            email("PRIVATE_LOCAL@example.com", true),
            email("PRIVATE_UNMATCHED@other.example", true)
        ]),
        false,
    ))
    .await;
    let cookie = common::capability::select(&app, None).await;
    let offer = body_text(get(&app, &cookie, "/join").await).await;
    assert!(offer.contains("Verified email domain required"));
    assert!(offer.contains("example.com"));
    assert!(offer.contains("acme/api"));
    assert!(!offer.contains("name=\"justification\""));
    let (cookie, state) = login(&app, &cookie).await;
    let response = get(
        &app,
        &cookie,
        &format!("/oauth/callback?state={state}&code=CODE_MARKER"),
    )
    .await;
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/join?fresh=true");
    let newcookie = common::sign_in::session_cookie(&response, None);
    assert_ne!(cookie, newcookie);
    let stored = record(&sessions, &newcookie).await;
    let session: &serde_json::Value = &stored.data["ghinvite"];
    assert!(session.get("access_token").is_none());
    assert_eq!(
        session["domain_evidence"]["matched_domains"],
        serde_json::json!(["example.com"])
    );
    let json = serde_json::to_string(&stored.data).unwrap();
    for marker in [TOKEN, "PRIVATE_", "other.example", "CODE_MARKER"] {
        assert!(!json.contains(marker));
    }
    for (field, value) in [
        (
            "checked_at",
            serde_json::json!((Utc::now() + chrono::Duration::minutes(1)).to_rfc3339()),
        ),
        ("link_id", serde_json::json!(InvitationLinkId::new())),
    ] {
        let mut invalid = stored.clone();
        invalid.data.get_mut("ghinvite").unwrap()["domain_evidence"][field] = value;
        sessions.save(&invalid).await.unwrap();
        let html = body_text(get(&app, &newcookie, "/join?fresh=true").await).await;
        assert!(html.contains("Confirm your email domain"));
        assert!(!html.contains("name=\"justification\""));
        let csrf = session["csrf_token"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::post("/join")
                    .header("cookie", &newcookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "csrf_token={csrf}&operation_id={}",
                        RequestId::new()
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(authority.received::<Admit>("admit").is_empty());
    }
    sessions.save(&stored).await.unwrap();
    let old_operation = AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap();
    let capability = common::capability::state();
    authority.seed_admission(
        Admit {
            link_id: ACTIVE_SLUG.parse().unwrap(),
            secret_sha256: capability.sha256,
            secret_generation: capability.secret_generation,
            requester_id: REQUESTER_ID,
            operation_id: old_operation.clone(),
            justification: None,
            scope_evidence: vec![],
            domain_evidence: None,
        },
        AdmissionReceipt {
            decided_at: Utc::now(),
            result: AdmissionResult::Rejected {
                reason: ghinvite_core::admission::Rejection::DomainEvidenceMissing,
            },
        },
    );
    let old = body_text(
        get(
            &app,
            &newcookie,
            &format!("/join?operation_id={}", String::from(old_operation)),
        )
        .await,
    )
    .await;
    assert!(old.contains("Confirm your email domain"));
    // Generic navigation can prepare a new operation after reauthentication,
    // while the explicit completed operation remains its original refusal.
    let response = get(&app, &newcookie, "/join").await;
    assert_eq!(response.status(), 200);
    let html = body_text(response).await;
    assert!(html.contains("name=\"justification\""));
    let field = |name: &str| {
        html.split(&format!("name=\"{name}\" value=\""))
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    };
    let body = format!(
        "csrf_token={}&operation_id={}&justification=domain-test",
        field("csrf_token"),
        field("operation_id")
    );
    let response = app
        .clone()
        .oneshot(
            Request::post("/join")
                .header("cookie", &newcookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    let commands = authority.received::<Admit>("admit");
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0]
            .domain_evidence
            .as_ref()
            .unwrap()
            .matched_domains[0]
            .as_str(),
        "example.com"
    );
    assert_exhausted(&github).await;
}
#[tokio::test]
async fn reauthentication_failure_persists_evidence_removal_and_keeps_pending_capability() {
    let mut requests = script(
        REQUESTER_ID,
        serde_json::json!([email("first@example.com", true)]),
        false,
    );
    requests.extend(script(REQUESTER_ID + 1, serde_json::json!([]), true));
    let (app, authority, sessions, github) = fixture(requests).await;
    let cookie = common::capability::select(&app, None).await;
    let (cookie, state) = login(&app, &cookie).await;
    let response = get(
        &app,
        &cookie,
        &format!("/oauth/callback?state={state}&code=first"),
    )
    .await;
    let cookie = common::sign_in::session_cookie(&response, None);
    let before = record(&sessions, &cookie).await;
    let pending = before.data["ghinvite"]["pending_link"].clone();
    assert!(before.data["ghinvite"].get("domain_evidence").is_some());
    // Retain an exact local attempt whose preparation never reached the owner.
    // Fresh recovery must still check current proof after a failed reauth.
    authority.fail_once("prepare_attempt", 503);
    let operation = RequestId::new().to_string();
    let response = app
        .clone()
        .oneshot(
            Request::post("/join")
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={}&operation_id={operation}",
                    before.data["ghinvite"]["csrf_token"].as_str().unwrap()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let cookie = common::sign_in::session_cookie(&response, Some(cookie));
    let (cookie, state) = login(&app, &cookie).await;
    let response = get(
        &app,
        &cookie,
        &format!("/oauth/callback?state={state}&code=second"),
    )
    .await;
    assert_eq!(response.status(), 500);
    let after = record(&sessions, &cookie).await;
    assert!(after.data["ghinvite"].get("domain_evidence").is_none());
    assert_eq!(after.data["ghinvite"]["pending_link"], pending);
    assert_eq!(after.data["ghinvite"]["user_id"], REQUESTER_ID);
    let html = body_text(get(&app, &cookie, "/join?fresh=true").await).await;
    assert!(html.contains("Confirm your email domain"));
    assert!(!html.contains("name=\"justification\""));
    let explicit_fresh = body_text(
        get(
            &app,
            &cookie,
            &format!("/join?operation_id={operation}&fresh=true"),
        )
        .await,
    )
    .await;
    assert!(explicit_fresh.contains("Confirm your email domain"));
    assert!(!explicit_fresh.contains("name=\"justification\""));
    // Exact old input remains recoverable; its original proof stays attached.
    let old = body_text(get(&app, &cookie, &format!("/join?operation_id={operation}")).await).await;
    assert!(old.contains("name=\"justification\""));
    assert_exhausted(&github).await;
}
#[tokio::test]
async fn unverified_or_noreply_emails_offer_mismatch_without_an_admission_form() {
    let (app, authority, _, github) = fixture(script(
        REQUESTER_ID,
        serde_json::json!([
            email("unverified@example.com", false),
            email("id@users.noreply.github.com", true)
        ]),
        false,
    ))
    .await;
    let cookie = common::capability::select(&app, None).await;
    let (cookie, state) = login(&app, &cookie).await;
    let response = get(
        &app,
        &cookie,
        &format!("/oauth/callback?state={state}&code=first"),
    )
    .await;
    let cookie = common::sign_in::session_cookie(&response, None);
    let html = body_text(get(&app, &cookie, "/join?fresh=true").await).await;
    assert!(html.contains("Domain mismatch"));
    assert!(html.contains("example.com"));
    assert!(!html.contains("name=\"justification\""));
    assert!(authority.received::<Admit>("admit").is_empty());
    assert_exhausted(&github).await;
}

#[tokio::test]
async fn callback_key_generation_retry_preserves_original_pending_operation() {
    use ghinvite_core::storage::pending_signin::{PendingSignInKey, PendingSignInStorage};
    use sha2::{Digest, Sha256};
    let authority = FakeLinkAuthority::start().await;
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let sessions = tower_sessions::MemoryStore::default();
    let mut requests = script(REQUESTER_ID, serde_json::json!([]), false);
    // No domain restriction without a pending link: no email read.
    let _emails = requests.remove(2);
    let github = github(requests).await;
    let old = WebConfig::for_local_dev_with_secret([7; 32]).keyring;
    let new = Arc::new(
        kekring::Keyring::builder("p1")
            .generation(1, [7; 32])
            .unwrap()
            .generation(2, [8; 32])
            .unwrap()
            .current(2)
            .unwrap()
            .build()
            .unwrap(),
    );
    let app = |keyring| {
        build_app(
            crate::common::sign_in::app_state(
                storage.clone(),
                Arc::new(MockTransport::scripted(vec![])),
                authority.client(),
                WebConfig {
                    signin_apps: Some(common::apps::signin_app(&github)),
                    keyring,
                    ..WebConfig::for_local_dev_with_secret([7; 32])
                },
            ),
            sessions.clone(),
        )
    };
    let current = app(new);
    let lagging = app(old);
    let (cookie, state) = login(&current, "").await;
    let id = cookie.split_once('=').unwrap().1;
    let mut hash = Sha256::new();
    hash.update(b"p1:pending-signin:v1\0");
    hash.update(id);
    let key = PendingSignInKey::from_bytes(hash.finalize().into());
    let before = storage.read_pending_signin(key).await.unwrap().unwrap();
    let path = format!("/oauth/callback?state={state}&code=CODE_MARKER");
    let response = get(&lagging, &cookie, &path).await;
    assert_eq!(response.status(), 503);
    assert!(!response.headers().contains_key("set-cookie"));
    let after = storage.read_pending_signin(key).await.unwrap().unwrap();
    assert_eq!(after.ciphertext, before.ciphertext);
    assert_eq!(after.expires_at, before.expires_at);
    assert!(github.received_requests().await.unwrap().is_empty());
    let response = get(&current, &cookie, &path).await;
    assert_eq!(response.status(), 303);
    assert_ne!(common::sign_in::session_cookie(&response, None), cookie);
    assert!(storage.read_pending_signin(key).await.unwrap().is_none());
    assert_exhausted(&github).await;
}
