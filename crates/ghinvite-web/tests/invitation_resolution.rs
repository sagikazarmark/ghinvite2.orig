use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use common::build_app;
use ghinvite_core::admission::{
    AdmissionOperationId, AdmissionReceipt, AdmissionResult, Admit, AttemptQuery,
};
use ghinvite_core::invitation_link::AccountAdmin;
use ghinvite_core::request_lifecycle::RequestSnapshot;
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionAction, LifecycleOperationId};
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{AppInstallationStorage, RecordStorage};
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos, User,
};
use ghinvite_github::mocks::MockTransport;
use ghinvite_web::{LinkAuthority, WebConfig};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

mod common;
mod requester_delivery;

use common::authority_http_fixture::FakeLinkAuthority;
use common::sign_in::{GithubUser, oauth_expectations, sign_in as oauth_sign_in};

async fn sign_in(app: &axum::Router) -> String {
    let cookie = oauth_sign_in(app).await;
    common::capability::select(app, Some(&cookie)).await
}

#[tokio::test]
async fn ingress_credentials_stay_out_of_html_props_and_browser_errors() {
    use ghinvite_web::RestateClient;
    use ghinvite_web::restate_client::RestateAuth;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    const API_KEY: &str = "browser-secrecy-ingress-test-key";
    let ingress = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {API_KEY}"),
        ))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("Rejected {API_KEY}")))
        .expect(1)
        .mount(&ingress)
        .await;
    let config = WebConfig {
        restate_ingress: ingress.uri(),
        restate_auth: RestateAuth::from_config(None, Some(API_KEY)).unwrap(),
        ..WebConfig::for_local_dev_with_secret([7; 32])
    };
    assert!(!format!("{config:?}").contains(API_KEY));
    let restate = Arc::new(
        RestateClient::with_auth(&config.restate_ingress, config.restate_auth.clone()).unwrap(),
    );
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    common::capability::register(&storage, &active_link(ACTIVE_SLUG)).await;
    let state = crate::common::sign_in::app_state(
        Arc::new(storage),
        Arc::new(MockTransport::scripted(oauth_expectations(
            GithubUser::new("octocat", REQUESTER_ID),
        ))),
        restate,
        config,
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    for (uri, status) in [
        ("/".into(), StatusCode::OK),
        ("/join".to_owned(), StatusCode::BAD_GATEWAY),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert!(!format!("{:?}", response.headers()).contains(API_KEY));
        assert!(!body_text(response).await.contains(API_KEY));
    }
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

/// A requester router over the fake link authority, which holds the active
/// link. Every admission applies but its acknowledgement is lost; a retry of
/// the same attempt recovers the retained receipt.
struct Requester {
    app: axum::Router,
    authority: FakeLinkAuthority,
    link: InvitationLink,
}

const ADMIN: AccountAdmin = AccountAdmin {
    account_id: 9001,
    user_id: CREATOR_ID,
};

async fn requester() -> Requester {
    let authority = FakeLinkAuthority::start().await;
    let link = active_link(ACTIVE_SLUG);
    authority.seed_link(&link);
    authority.lose_acknowledgements("admit");
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    common::capability::register(&storage, &active_link(ACTIVE_SLUG)).await;
    let state = crate::common::sign_in::app_state(
        Arc::new(storage),
        Arc::new(MockTransport::scripted(
            oauth_expectations(GithubUser::new("octocat", REQUESTER_ID))
                .into_iter()
                .chain(oauth_expectations(GithubUser::new(
                    "othercat",
                    REQUESTER_ID + 1,
                )))
                .collect(),
        )),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    Requester {
        app: build_app(state, tower_sessions::MemoryStore::default()),
        authority,
        link,
    }
}

/// Make the authority hold the requester's request on `link` in `state`,
/// admitted earlier by the attempt `operation_id`.
fn seed_admitted(
    authority: &FakeLinkAuthority,
    link: &InvitationLink,
    operation_id: &str,
    justification: Option<&str>,
    state: RequestState,
) -> RequestSnapshot {
    let admitted_at = Utc::now();
    let deadline = admitted_at + chrono::Duration::days(7);
    let request = RequestSnapshot {
        batch: None,
        domain_evidence: None,
        request_id: RequestId::new(),
        link_id: Some(link.id),
        account_id: link.account_id,
        requester_id: REQUESTER_ID,
        justification: justification.map(str::to_owned),
        state,
        admitted_at,
        decision_deadline: Some(deadline),
        revision: 1,
        decision: None,
    };
    authority.seed_request(request.clone());
    authority.seed_admission(
        Admit {
            domain_evidence: None,
            secret_sha256: link.secret.sha256,
            secret_generation: link.secret.secret_generation,
            scope_evidence: vec![],
            link_id: link.id,
            operation_id: AdmissionOperationId::try_from(operation_id.to_owned()).unwrap(),
            requester_id: REQUESTER_ID,
            justification: justification.map(str::to_owned),
        },
        AdmissionReceipt {
            decided_at: admitted_at,
            result: AdmissionResult::Accepted {
                request_id: request.request_id,
                state: RequestState::Pending,
                decision_deadline: Some(deadline),
            },
        },
    );
    request
}

/// Decide `request` as an account admin would.
async fn decide(authority: &FakeLinkAuthority, request: &RequestSnapshot, action: DecisionAction) {
    ghinvite_web::request_authority::RequestAuthority::new(authority.client())
        .decide(DecideRequest {
            link_id: request.link_id.expect("linked fixture"),
            request_id: request.request_id,
            operation_id: LifecycleOperationId::try_from(RequestId::new().to_string()).unwrap(),
            admin: ADMIN,
            action,
        })
        .await
        .unwrap();
}

fn count(authority: &FakeLinkAuthority, method: &str) -> usize {
    authority.calls().iter().filter(|m| *m == method).count()
}

#[tokio::test]
async fn authoritative_form_confirms_identity_and_wrong_account_return_destination() {
    let Requester { app, .. } = requester().await;
    let cookie = sign_in(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = body_text(response).await;
    assert!(html.contains("Signed in as"));
    assert!(html.contains("@octocat"));
    assert!(html.contains("Not you?"));
    assert!(html.contains("Read (pull)"));
    assert!(html.contains("acme/api"));
    assert!(html.contains("Account admins review your request before access is approved."));
    assert!(html.contains("Optional, visible to account admins."));
    assert!(html.contains("Maximum 500 characters"));
    assert!(html.contains("name=\"return_to\" value=\"/join\""));
    let csrf = common::csrf_token(&app, &cookie).await;
    let mut cookie = cookie;
    for (token, expected) in [
        ("wrong", StatusCode::FORBIDDEN),
        (csrf.as_str(), StatusCode::SEE_OTHER),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/logout")
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf_token={token}&return_to=%2Fjoin")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::SEE_OTHER {
            assert_eq!(response.headers()["location"], "/join".to_owned());
            cookie = common::sign_in::session_cookie(&response, Some(cookie));
        }
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let offer = body_text(response).await;
    assert!(offer.contains("Review this invitation"));
    assert!(offer.contains("/login?return_to=%2Fjoin"));
    assert!(!offer.contains("name=\"justification\""));
}

#[tokio::test]
async fn local_unknown_attempt_can_start_fresh_after_ingress_recovers() {
    let Requester { app, authority, .. } = requester().await;
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new().to_string();
    // The attempt never reaches the authority.
    authority.fail_once("prepare_attempt", 503);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&operation_id={id}&justification=original"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    for fresh in [false, true] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/join?operation_id={id}&fresh={fresh}"))
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Start a fresh attempt"));
        assert!(html.contains(&format!("operation_id={id}")));
        assert_eq!(html.contains(&format!("value=\"{id}\"")), !fresh);
        assert_eq!(html.contains("This is a fresh attempt"), fresh);
    }
    assert_eq!(count(&authority, "admit"), 0);
}

#[tokio::test]
async fn pending_authoritative_status_refreshes_until_terminal_without_projection() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    let cookie = sign_in(&app).await;
    for status in [
        RequestState::Pending,
        RequestState::Approved,
        RequestState::Declined,
        RequestState::Expired,
        RequestState::Cancelled,
    ] {
        let operation = RequestId::new().to_string();
        seed_admitted(&authority, &link, &operation, None, status);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/join".to_owned())
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert_eq!(
            html.contains("http-equiv=\"refresh\" content=\"20\""),
            status == RequestState::Pending
        );
        assert!(html.contains("Check again"));
        assert!(html.contains(&format!("Current request status: {status}")));
        assert!(!html.contains("Submit request"));
    }
}

#[tokio::test]
async fn oversized_justification_is_editable_without_replacing_the_operation() {
    let Requester { app, authority, .. } = requester().await;
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new().to_string();
    let text = "é".repeat(8193);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/join?operation_id={id}"))
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&operation_id={id}&justification={}",
                    encoded_return_to(&text)
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let html = body_text(response).await;
    assert!(html.contains(&text));
    assert!(html.contains("aria-invalid=\"true\""));
    assert!(html.contains("justification-help justification-error"));
    assert!(html.contains("Shorten your justification"));
    assert!(html.contains(&format!("value=\"{id}\"")));
    assert!(!html.contains("readonly=\"true\""));
    assert_eq!(count(&authority, "prepare_attempt"), 0);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&operation_id={id}&justification=shortened"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(body_text(response).await.contains("Outcome unknown"));
}

#[tokio::test]
async fn inactive_fresh_visits_are_concealed_and_blocked_fresh_forms_are_suppressed() {
    let Requester {
        app,
        authority,
        mut link,
    } = requester().await;
    link.targets.repos = vec![ghinvite_core::targets::RepoTarget {
        grant: ghinvite_core::targets::RepoRole::Read,
        repo_id: 1,
        repo_full_name: "acme/private".into(),
    }];
    let mut revoked = link.clone();
    revoked.revoked_at = Some(Utc::now());
    revoked.revoked_by = Some(CREATOR_ID);
    let cookie = sign_in(&app).await;
    for (attempt, can_start, expected) in [
        (false, false, StatusCode::NOT_FOUND),
        (false, true, StatusCode::OK),
        (true, false, StatusCode::OK),
    ] {
        // A revoked link, the active link, or the active link where the
        // requester's accepted attempt awaits review.
        authority.seed_link(if can_start || attempt {
            &link
        } else {
            &revoked
        });
        if attempt {
            let operation = RequestId::new().to_string();
            seed_admitted(&authority, &link, &operation, None, RequestState::Pending);
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/join?fresh=true")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let html = body_text(response).await;
        assert_eq!(html.contains("Submit request"), can_start);
        if !can_start && !attempt {
            assert!(!html.contains("acme/private"));
            assert!(html.contains("The link may be incorrect or no longer available."));
        }
        if attempt {
            assert!(html.contains("Request accepted at"));
            assert!(!html.contains("Start a fresh attempt"));
        }
    }
}

#[tokio::test]
async fn lost_admission_acknowledgement_recovers_across_navigation_and_editing_is_explicit() {
    let Requester { app, authority, .. } = requester().await;
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new().to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&operation_id={id}&justification=++original++"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let html = body_text(response).await;
    assert!(
        !html.contains("acknowledgement lost"),
        "no transport detail"
    );
    assert!(html.contains("Outcome unknown"));
    for uri in [
        "/join".to_owned(),
        "/join".to_owned(),
        format!("/join?operation_id={id}"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("Request accepted at"));
    }
    // Once the admitted request is declined, a fresh attempt is possible.
    let [request] = authority.requests().try_into().unwrap();
    decide(
        &authority,
        &request,
        DecisionAction::Decline {
            reason: None,
            requester_message: None,
        },
    )
    .await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/join?fresh=true")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = body_text(response).await;
    assert!(html.contains("This is a fresh attempt"));
    assert!(html.contains(&format!("operation_id={id}")));
    assert!(!html.contains(&format!("value=\"{id}\"")));
    assert!(!html.contains("readonly=\"true\""));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&operation_id={id}&justification=edited"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        body_text(response)
            .await
            .contains("Recover the original attempt")
    );
    assert_eq!(count(&authority, "admit"), 1);
}

#[tokio::test]
async fn unresolved_local_attempt_keeps_exact_input_alongside_older_authority() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let older = RequestId::new().to_string();
    seed_admitted(&authority, &link, &older, None, RequestState::Declined);
    authority.fail("prepare_attempt", 503);
    let newer = RequestId::new().to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&operation_id={newer}&justification=++Exact++input++"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(body_text(response).await.contains("  Exact  input  "));
    for fresh in [false, true] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/join?fresh={fresh}"))
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let html = body_text(response).await;
        assert!(
            html.contains(&format!("operation_id={newer}")),
            "local uncertainty stays recoverable"
        );
        assert!(
            html.contains("Current request status: declined"),
            "older authority stays visible"
        );
        if !fresh {
            assert!(html.contains("  Exact  input  "));
        }
    }
}

#[tokio::test]
async fn prepared_uncertain_attempt_retries_with_original_whitespace() {
    let Requester { app, authority, .. } = requester().await;
    let cookie = sign_in(&app).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    authority.fail("admit", 503);
    let id = RequestId::new().to_string();
    let body = format!("csrf_token={csrf}&operation_id={id}&justification=++Docs++");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/join?operation_id={id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(body_text(response).await.contains("  Docs  "));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "retry reaches uncertain ingress rather than conflicting locally"
    );
    assert_eq!(count(&authority, "admit"), 2);
}

/// Set the admission fixture's authority: revoke or restore the link, and
/// decide the requester's open requests.
async fn fixture_state(
    requester: (FakeLinkAuthority, InvitationLink),
    query: BTreeMap<String, String>,
) -> StatusCode {
    let (authority, link) = requester;
    let action = match query.get("status").map(String::as_str) {
        None => None,
        Some("approved") => Some(DecisionAction::Approve),
        Some("declined") => Some(DecisionAction::Decline {
            reason: None,
            requester_message: None,
        }),
        Some(_) => return StatusCode::BAD_REQUEST,
    };
    if let Some(action) = action {
        for request in authority.requests() {
            if request.state == RequestState::Pending {
                decide(&authority, &request, action.clone()).await;
            }
        }
    }
    match query.get("revoked").map(String::as_str) {
        Some("true") => {
            LinkAuthority::new(authority.client())
                .revoke(ghinvite_core::revocation::RevokeCommand {
                    link_id: link.id,
                    admin: ADMIN,
                    operation_id: link.id.to_string().try_into().unwrap(),
                    cascade: Default::default(),
                })
                .await
                .unwrap()
                .unwrap();
        }
        Some(_) => {
            // The real authority never restores a revoked link; the fixture
            // does so to reopen fresh attempts.
            let mut snapshot = authority.link(link.id).unwrap();
            snapshot.revoked_at = None;
            snapshot.revoked_by = None;
            authority.seed(snapshot);
        }
        None => {}
    }
    StatusCode::NO_CONTENT
}

/// Playwright drives this real router with JavaScript disabled. Authentication
/// is obtained via the existing public OAuth test flow before serving.
#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn admission_browser_server() {
    use axum::response::IntoResponse;
    let current = Arc::new(Mutex::new(None::<Requester>));
    let login_current = current.clone();
    let state_current = current.clone();
    let app = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let current = login_current.clone();
                async move {
                    let requester = requester().await;
                    let cookie = sign_in(&requester.app).await;
                    *current.lock().unwrap() = Some(requester);
                    (
                        [(
                            "set-cookie",
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to("/join"),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/fixture-state",
            axum::routing::post(
                move |axum::extract::Query(query): axum::extract::Query<
                    BTreeMap<String, String>,
                >| {
                    let requester = {
                        let current = state_current.lock().unwrap();
                        let requester = current.as_ref().unwrap();
                        (requester.authority.clone(), requester.link.clone())
                    };
                    fixture_state(requester, query)
                },
            ),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = current.lock().unwrap().as_ref().unwrap().app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    common::browser_fixture::serve("127.0.0.1:4174", app).await;
}

#[tokio::test]
async fn authoritative_native_form_validates_identity_and_preserves_unknown_input_without_sql_link()
{
    let authority = FakeLinkAuthority::start().await;
    authority.seed_link(&active_link(ACTIVE_SLUG));
    authority.fail("prepare_attempt", 503);
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    common::capability::register(&storage, &active_link(ACTIVE_SLUG)).await;
    let state = crate::common::sign_in::app_state(
        Arc::new(storage),
        Arc::new(MockTransport::scripted(oauth_expectations(
            GithubUser::new("octocat", REQUESTER_ID),
        ))),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let backend = ghinvite_web_server::SqliteBackend::new(
        sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap(),
    );
    backend.migrate().await.unwrap();
    let app = build_app(
        state,
        ghinvite_web::session_store::ProtectedStore::new(
            backend,
            ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32]).keyring,
        ),
    );
    let cookie = sign_in(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    let field = |name: &str| {
        html.split(&format!("name=\"{name}\" value=\""))
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    };
    let csrf = field("csrf_token");
    let operation = field("operation_id");
    for id in ["", "bad", "8ZZZZZZZZZZZZZZZZZZZZZZZZZ", &operation] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/join".to_owned())
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "csrf_token={csrf}&operation_id={id}&justification=++original++"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        if id == operation {
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let html = body_text(response).await;
            assert!(html.contains("Outcome unknown"));
            assert!(html.contains(&format!("value=\"{operation}\"")));
            assert!(html.contains("original"));
            assert!(!html.contains("Start a fresh attempt"));
        } else {
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }
    assert_eq!(count(&authority, "prepare_attempt"), 1);
    // Preparation itself was unavailable: recover from the explicitly saved
    // session continuation after navigating away from the failed POST, while
    // the whole authority is unavailable.
    for method in ["requester_page", "admit"] {
        authority.fail(method, 503);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/join?operation_id={operation}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let html = body_text(response).await;
    assert!(html.contains("original"));
    assert!(html.contains(&format!("value=\"{operation}\"")));
    assert!(
        html.contains(&format!("operation_id={operation}")) && !html.contains("fresh=true"),
        "{html}"
    );
    let post = |id: String, text: &str| {
        Request::builder()
            .method("POST")
            .uri("/join".to_owned())
            .header("cookie", &cookie)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "csrf_token={csrf}&operation_id={id}&justification={text}"
            )))
            .unwrap()
    };
    let changed = app
        .clone()
        .oneshot(post(operation.clone(), "edited"))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::CONFLICT);
    let second = RequestId::new().to_string();
    let third = RequestId::new().to_string();
    let (a, b) = tokio::join!(
        app.clone().oneshot(post(second.clone(), "second")),
        app.clone().oneshot(post(third.clone(), "third"))
    );
    assert_eq!(a.unwrap().status(), StatusCode::BAD_GATEWAY);
    assert_eq!(b.unwrap().status(), StatusCode::BAD_GATEWAY);
    // A fresh attempt first reads the link's current domain rules. With the
    // authority unavailable that read fails, so neither new attempt was
    // retained or prepared; only the original recovers.
    assert_eq!(count(&authority, "prepare_attempt"), 1);
    for (id, text, retained) in [
        (operation, "original", true),
        (second, "second", false),
        (third, "third", false),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/join?operation_id={id}"))
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(body_text(response).await.contains(text), retained, "{text}");
    }
}

#[tokio::test]
async fn request_submission_requires_the_rendered_session_token() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    let cookie = sign_in(&app).await;
    let token = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new();
    for body in [
        format!("operation_id={id}"),
        format!("csrf_token=wrong&operation_id={id}"),
        format!("csrf_token={token}&csrf_token={token}&operation_id={id}"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/join".to_owned())
                    .header("cookie", &cookie)
                    .header("origin", "https://evil.ghinvite.test")
                    .header("sec-fetch-site", "same-site")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert!(authority.calls().is_empty());
    // The authority already holds this attempt's receipt, so preparing it
    // settles the admission.
    seed_admitted(
        &authority,
        &link,
        &id.to_string(),
        Some("Native request"),
        RequestState::Pending,
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={token}&operation_id={id}&justification=Native+request"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/join?operation_id={id}")
    );
    let [command] = authority
        .received::<Admit>("prepare_attempt")
        .try_into()
        .unwrap();
    assert_eq!(String::from(command.operation_id), id.to_string());
    assert_eq!(command.requester_id, REQUESTER_ID);
    assert_eq!(command.justification.as_deref(), Some("Native request"));
    let [body] = authority
        .received::<serde_json::Value>("prepare_attempt")
        .try_into()
        .unwrap();
    assert!(!body.to_string().contains(&token));
    assert_eq!(count(&authority, "admit"), 0);
}

/// An existing pending or approved request is the authority's call, not the
/// projection's; its rejection is final and offers no resubmission.
#[tokio::test]
async fn submission_rejected_for_an_existing_request_is_final() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    authority.recover("admit");
    let earlier = RequestId::new().to_string();
    seed_admitted(&authority, &link, &earlier, None, RequestState::Pending);
    let cookie = sign_in(&app).await;
    let token = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}&operation_id={id}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = body_text(response).await;
    assert!(html.contains("you already have a request awaiting review"));
    assert!(html.contains("result is final."));
    assert!(!html.contains("Submit request"));
    assert!(!html.contains("Retry same attempt"));
    // The rejection admitted nothing and spent no use of the link.
    assert_eq!(authority.requests().len(), 1);
    assert_eq!(authority.link(link.id).unwrap().uses, 0);
}

const ACTIVE_SLUG: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const UNKNOWN_SLUG: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const CREATOR_ID: u64 = 701;
const REQUESTER_ID: u64 = 802;

fn encoded_return_to(path: &str) -> String {
    url::form_urlencoded::byte_serialize(path.as_bytes()).collect()
}

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn sample_account() -> Account {
    Account {
        installation_id: 1,
        account_id: 9001,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: dt("2026-05-04T12:00:00Z"),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

fn sample_user(user_id: u64, login: &str) -> User {
    User {
        user_id,
        login: login.into(),
        avatar_url: None,
        github_created_at: None,
        last_seen_at: dt("2026-05-04T12:00:00Z"),
    }
}

fn active_link(slug: &str) -> InvitationLink {
    InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: slug.parse().unwrap(),
        organization_installation_id: None,
        installation_id: 1,
        account_id: 9001,
        created_by: CREATOR_ID,
        created_at: dt("2026-05-04T12:00:00Z"),
        expires_at: None,
        max_uses: None,
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,
        secret: common::capability::state(),
        approval_required: true,
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        targets: ghinvite_core::targets::Targets {
            repos: vec![InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }]
            .into_iter()
            .map(|repo| ghinvite_core::targets::RepoTarget {
                repo_id: repo.repo_id,
                repo_full_name: repo.repo_full_name,
                grant: Permission::Pull.into(),
            })
            .collect(),
            ..Default::default()
        },
    }
}

fn request_with_state(
    id: RequestId,
    link: InvitationLinkId,
    requester_id: u64,
    state: RequestState,
) -> InvitationRequest {
    InvitationRequest {
        batch: None,
        domain_evidence: None,
        id,
        invitation_link_id: Some(link),
        requester_id,
        justification: None,
        state,
        decided_by: None,
        decided_at: None,
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
        decision_deadline: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    }
}

/// A requester router whose projection holds `link`, with no reachable
/// authority: enough for flows that never leave the browser boundary.
async fn build_test_app(link: InvitationLink, mock: MockTransport) -> axum::Router {
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    storage
        .put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account()).clone(),
        ))
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(CREATOR_ID, "creator"))
        .await
        .unwrap();
    storage.seed_link(&link).await.unwrap();
    common::capability::register(&storage, &active_link(ACTIVE_SLUG)).await;
    let state = crate::common::sign_in::app_state(
        Arc::new(storage),
        Arc::new(mock),
        Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    build_app(state, tower_sessions::MemoryStore::default())
}

#[tokio::test]
async fn requester_status_comes_from_the_authority_and_hides_the_decline_reason() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    // No projected request exists; the authority's requester page is the
    // requester's only source of truth.
    let operation = RequestId::new().to_string();
    let request = seed_admitted(&authority, &link, &operation, None, RequestState::Pending);
    decide(
        &authority,
        &request,
        DecisionAction::Decline {
            reason: Some("Private admin context".into()),
            requester_message: None,
        },
    )
    .await;
    let cookie = sign_in(&app).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/join".to_owned())
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("Current request status: declined"));
    assert!(!html.contains("Private admin context"));
    assert!(!html.contains("Submit request"));
    let [query] = authority
        .received::<AttemptQuery>("requester_page")
        .try_into()
        .unwrap();
    assert_eq!(query.requester_id, REQUESTER_ID);
}

#[tokio::test]
async fn landing_unauthenticated_shows_capability_bound_offer_without_requester_data() {
    let Requester { app, .. } = requester().await;
    let cookie = common::capability::select(&app, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/join")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let offer = body_text(resp).await;
    assert!(offer.contains("Review this invitation"));
    assert!(offer.contains("/login?return_to=%2Fjoin"));
    assert!(!offer.contains("name=\"justification\""));
}

#[tokio::test]
async fn legacy_id_paths_are_neutral_404_without_signin_or_echo() {
    let app = build_test_app(
        active_link(ACTIVE_SLUG),
        MockTransport::scripted(oauth_expectations(GithubUser::new("octocat", REQUESTER_ID))),
    )
    .await;
    let cookie = oauth_sign_in(&app).await;
    for cookie in [None, Some(cookie.as_str())] {
        for path in [
            format!("/i/{ACTIVE_SLUG}"),
            "/i/not-a-valid-slug".into(),
            format!("/i/{UNKNOWN_SLUG}/request?foo=1"),
        ] {
            for method in ["GET", "POST"] {
                let mut request = Request::builder().method(method).uri(&path);
                if let Some(cookie) = cookie {
                    request = request.header("cookie", cookie);
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                assert!(response.headers().get("location").is_none());
                let html = body_text(response).await;
                assert!(!html.contains(ACTIVE_SLUG));
                assert!(!html.contains(UNKNOWN_SLUG));
                assert!(!html.contains("not-a-valid-slug"));
                assert!(!html.contains("Submit request"));
            }
        }
    }
}

#[tokio::test]
async fn submit_unauthenticated_redirects_to_login_for_session_join() {
    let app = build_test_app(active_link(ACTIVE_SLUG), MockTransport::scripted(vec![])).await;
    let cookie = common::capability::select(&app, None).await;
    let response = app
        .oneshot(
            Request::post("/join")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("operation_id=invalid"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/login?return_to=%2Fjoin");
}

#[tokio::test]
async fn requester_form_carries_csp_and_only_external_script() {
    let Requester { app, .. } = requester().await;
    let cookie = sign_in(&app).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/join".to_owned())
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-security-policy")
            .map(|v| v.to_str().unwrap()),
        Some(ghinvite_web::middleware::csp::CONTENT_SECURITY_POLICY)
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Request repository access"));
    assert!(text.contains("Submit request"));
    let external = "<script src=\"/static/app.js\"></script>";
    assert!(text.contains(external));
    assert_eq!(
        text.matches("<script").count(),
        text.matches(external).count(),
        "invitation HTML contains an inline <script> block"
    );
}

#[tokio::test]
async fn paused_requester_page_requires_current_capability_and_shows_no_new_request_form() {
    for reason in [
        ghinvite_core::link_lifecycle::PauseReason::Manual,
        ghinvite_core::link_lifecycle::PauseReason::AbuseSuspected {
            window_start: Utc::now(),
        },
    ] {
        let Requester {
            app,
            authority,
            mut link,
        } = requester().await;
        link.pause = Some(ghinvite_core::link_lifecycle::LinkPause {
            epoch: 2,
            at: Utc::now(),
            by: Some(CREATOR_ID),
            reason,
        });
        authority.seed_link(&link);
        let cookie = sign_in(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/join")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("accepting requests right now"));
        assert!(!html.contains("Submit request"));
        assert!(!html.contains("AbuseSuspected"));
        assert!(!html.contains("suspected abuse"));
        // The same session's formerly valid capability becomes neutral after
        // rotation, before any pause state can be disclosed.
        link.secret.secret_generation += 1;
        link.secret.sha256 = [99; 32].into();
        authority.seed_link(&link);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/join")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !body_text(response)
                .await
                .contains("accepting requests right now")
        );
    }
}

#[tokio::test]
async fn cancelled_requester_status_explains_link_revocation_without_admin_context() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    let operation = RequestId::new().to_string();
    let mut request = seed_admitted(&authority, &link, &operation, None, RequestState::Cancelled);
    request.revision = 2;
    request.decision = Some(ghinvite_core::request_lifecycle::TerminalDecision {
        decision_id: format!("request.cancelled/{}", request.request_id),
        decided_by: None,
        effective_at: request.admitted_at,
        evaluated_at: request.admitted_at,
        decline_reason: None,
        requester_message: None,
        cancellation_reason: Some(ghinvite_core::RequestCancellationReason::LinkRevoked),
    });
    authority.seed_request(request);
    let cookie = sign_in(&app).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/join?operation_id={operation}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("Your request was cancelled because this invitation link was revoked."));
    assert!(!html.contains("Decline reason"));
}

#[tokio::test]
async fn explicit_decline_message_is_escaped_on_join_and_owned_request_page_only() {
    let Requester {
        app,
        authority,
        link,
    } = requester().await;
    let request = seed_admitted(
        &authority,
        &link,
        &RequestId::new().to_string(),
        None,
        RequestState::Pending,
    );
    decide(
        &authority,
        &request,
        DecisionAction::Decline {
            reason: Some("PRIVATE_ADMIN_ONLY_CONTEXT".into()),
            requester_message: Some("Please contact <script>the team</script>.".into()),
        },
    )
    .await;
    let cookie = sign_in(&app).await;
    for path in [
        "/join".to_owned(),
        format!("/requests/{}", request.request_id),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&path)
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(html.contains("Message from the reviewer:"), "{html}");
        assert!(
            html.contains("Please contact &#60;script&#62;the team&#60;/script&#62;."),
            "{path}: {html}"
        );
        assert!(!html.contains("PRIVATE_ADMIN_ONLY_CONTEXT"));
        assert!(!html.contains("<script>the team</script>"));
    }
    let mut foreign = authority.request(request.request_id).unwrap();
    foreign.requester_id += 1;
    authority.seed_request(foreign);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/requests/{}", request.request_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!body_text(response).await.contains("Please contact"));
}
