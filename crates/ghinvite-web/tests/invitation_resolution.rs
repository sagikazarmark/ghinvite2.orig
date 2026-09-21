use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use ghinvite_core::storage::Storage;
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos, Slug, User,
};
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::transport::{Method, Response};
use ghinvite_web::commands::{
    GhinviteCommands, OnboardInstallation, RecordInstallationUninstalled,
    RecordRepositorySelectionChange, RouteGithubInvitationWebhook,
};
use ghinvite_web::{AppState, WebConfig, build_app};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

mod common;
mod requester_delivery;

use common::link_authority::{CODE_SERVICE, LINK_SERVICE};

#[tokio::test]
async fn ingress_credentials_stay_out_of_html_props_and_browser_errors() {
    use ghinvite_web::restate_client::RestateAuth;
    use ghinvite_web::{RestateClient, RestateCommands};
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
    let state = AppState::new(
        Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        ),
        Arc::new(MockTransport::scripted(oauth_expectations(
            "octocat",
            REQUESTER_ID,
        ))),
        Arc::new(RestateCommands::new(restate.clone())),
        restate,
        config,
    );
    let app = build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(app.clone()).await;
    for (uri, status) in [
        ("/".into(), StatusCode::OK),
        (format!("/i/{ACTIVE_SLUG}"), StatusCode::BAD_GATEWAY),
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

/// Transport seam: committed admission with a lost acknowledgement. Domain
/// replay/revoke races are separately exercised against the real Restate object.
#[derive(Clone)]
struct LostAdmissionResponse {
    link_id: InvitationLinkId,
    attempts: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
    latest: Arc<Mutex<Option<String>>>,
    control: Arc<Mutex<(bool, String)>>,
}
impl wiremock::Respond for LostAdmissionResponse {
    fn respond(&self, request: &wiremock::Request) -> wiremock::ResponseTemplate {
        use serde_json::{Value, json};
        use wiremock::ResponseTemplate;
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let method = request.url.path().rsplit('/').next().unwrap();
        let mut attempts = self.attempts.lock().unwrap();
        match method {
            "resolve" => ResponseTemplate::new(200).set_body_json(self.link_id),
            "prepare_attempt" => {
                let id = body["operation_id"].as_str().unwrap().to_owned();
                if let Some(old) = attempts.get(&id) {
                    if old["input"] != body {
                        return ResponseTemplate::new(409);
                    }
                } else {
                    attempts.insert(id.clone(), json!({"input": body, "receipt": null}));
                    *self.latest.lock().unwrap() = Some(id.clone());
                }
                ResponseTemplate::new(200).set_body_json(attempts[&id].clone())
            }
            "admit" => {
                let id = body["operation_id"].as_str().unwrap();
                let attempt = attempts.get_mut(id).unwrap();
                if attempt["receipt"].is_null() {
                    attempt["receipt"] = json!({"decided_at": "2026-09-14T12:00:00Z", "result": {
                        "kind": "accepted", "request_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "state": "pending",
                        "decision_deadline": "2026-09-21T12:00:00Z" }});
                    ResponseTemplate::new(503).set_body_string("private transport detail")
                } else {
                    ResponseTemplate::new(200).set_body_json(attempt["receipt"].clone())
                }
            }
            "requester_page" => {
                let (revoked, status) = self.control.lock().unwrap().clone();
                let id = body["operation_id"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.latest.lock().unwrap().clone());
                let mut attempt = id.as_ref().and_then(|id| attempts.get(id));
                if attempt.is_some_and(|a| a["input"]["requester_id"] != body["requester_id"]) {
                    if !body["operation_id"].is_null() {
                        return ResponseTemplate::new(404);
                    }
                    attempt = None;
                }
                let accepted = attempt.is_some_and(|a| !a["receipt"].is_null());
                let can_start =
                    !(revoked || accepted && matches!(status.as_str(), "pending" | "approved"));
                let current = if accepted {
                    json!({"request_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "link_id": self.link_id, "account_id": 1, "requester_id": body["requester_id"],
                    "justification": null, "state": status, "admitted_at": "2026-09-14T12:00:00Z",
                    "decision_deadline": "2026-09-21T12:00:00Z", "revision": 1})
                } else {
                    Value::Null
                };
                ResponseTemplate::new(200).set_body_json(
                    json!({"link_id": self.link_id, "invitation_code": ACTIVE_SLUG,
                    "repos": [{"repo_id": 1, "repo_full_name": "acme/api"}], "permission": "pull",
                    "approval_required": true, "can_start_fresh": can_start, "attempt": attempt, "request": current}),
                )
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

async fn lost_response_app() -> (axum::Router, wiremock::MockServer) {
    lost_response_app_with_control(Arc::new(Mutex::new((false, "declined".into())))).await
}

async fn lost_response_app_with_control(
    control: Arc<Mutex<(bool, String)>>,
) -> (axum::Router, wiremock::MockServer) {
    let ingress = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(LostAdmissionResponse {
            link_id: InvitationLinkId::new(),
            attempts: Default::default(),
            latest: Default::default(),
            control,
        })
        .mount(&ingress)
        .await;
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    let state = AppState::new(
        Arc::new(storage),
        Arc::new(MockTransport::scripted(
            oauth_expectations("octocat", REQUESTER_ID)
                .into_iter()
                .chain(oauth_expectations("othercat", REQUESTER_ID + 1))
                .collect(),
        )),
        Arc::new(UnusedCommands),
        Arc::new(ghinvite_web::RestateClient::new(ingress.uri()).unwrap()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    (
        build_app(state, tower_sessions::MemoryStore::default()),
        ingress,
    )
}

#[tokio::test]
async fn authoritative_form_confirms_identity_and_wrong_account_return_destination() {
    let (app, _ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    assert!(html.contains("Permission: Read (pull)"));
    assert!(html.contains("acme/api"));
    assert!(html.contains("Account admins review your request before access is approved."));
    assert!(html.contains("Optional, visible to account admins."));
    assert!(html.contains("Maximum 16384 UTF-8 bytes"));
    assert!(html.contains(&format!("name=\"return_to\" value=\"/i/{ACTIVE_SLUG}\"")));
    let csrf = common::csrf_token(&app, &cookie).await;
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
                    .body(Body::from(format!(
                        "csrf_token={token}&return_to=%2Fi%2F{ACTIVE_SLUG}"
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::SEE_OTHER {
            assert_eq!(response.headers()["location"], format!("/i/{ACTIVE_SLUG}"));
        }
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.headers()["location"],
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}"))
        )
    );
}

#[tokio::test]
async fn local_unknown_attempt_can_start_fresh_after_ingress_recovers() {
    use wiremock::{Mock, ResponseTemplate, matchers::path_regex};
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new().to_string();
    Mock::given(path_regex("/prepare_attempt$"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&ingress)
        .await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    Mock::given(path_regex("/requester_page$"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({"operation_id": id}),
        ))
        .respond_with(ResponseTemplate::new(404))
        .with_priority(1)
        .mount(&ingress)
        .await;
    for fresh in [false, true] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/i/{ACTIVE_SLUG}?operation_id={id}&fresh={fresh}"))
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
    assert!(
        ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().ends_with("/admit"))
    );
}

#[tokio::test]
async fn pending_authoritative_status_refreshes_until_terminal_without_projection() {
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    for status in ["pending", "approved", "declined", "expired", "cancelled"] {
        wiremock::Mock::given(wiremock::matchers::path_regex("/requester_page$"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "link_id": RequestId::new(), "invitation_code": ACTIVE_SLUG, "repos": [], "permission": "pull",
                "approval_required": true, "can_start_fresh": false, "attempt": null,
                "request": {"link_id": RequestId::new(), "request_id": RequestId::new(), "account_id": 1,
                    "requester_id": REQUESTER_ID, "justification": null, "state": status,
                    "admitted_at": "2026-09-14T12:00:00Z", "decision_deadline": "2026-09-21T12:00:00Z", "revision": 1}
            }))).with_priority(1).up_to_n_times(1).mount(&ingress).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/i/{ACTIVE_SLUG}"))
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
            status == "pending"
        );
        assert!(html.contains("Check again"));
        assert!(html.contains(&format!("Current request status: {status}")));
        assert!(!html.contains("Submit request"));
    }
}

#[tokio::test]
async fn oversized_justification_is_editable_without_replacing_the_operation() {
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new().to_string();
    let text = "é".repeat(8193);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}?operation_id={id}"))
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
    assert!(
        ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().ends_with("prepare_attempt"))
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    for (attempt, can_start, expected) in [
        (false, false, StatusCode::NOT_FOUND),
        (false, true, StatusCode::OK),
        (true, false, StatusCode::OK),
    ] {
        wiremock::Mock::given(wiremock::matchers::path_regex("/requester_page$"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "link_id": RequestId::new(), "invitation_code": ACTIVE_SLUG,
                "repos": [{"repo_id": 1, "repo_full_name": "acme/private"}], "permission": "pull",
                "approval_required": true, "can_start_fresh": can_start,
                "attempt": if attempt { serde_json::json!({"input": {
                    "link_id": RequestId::new(), "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                    "requester_id": REQUESTER_ID, "justification": null },
                    "receipt": {"decided_at": "2026-09-14T12:00:00Z", "result": {"kind": "accepted",
                    "request_id": RequestId::new(), "state": "pending", "decision_deadline": "2026-09-21T12:00:00Z"}}
                }) } else { serde_json::Value::Null }, "request": null
            }))).with_priority(1).up_to_n_times(1).mount(&ingress).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/i/{ACTIVE_SLUG}?fresh=true"))
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
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new().to_string();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    assert!(!html.contains("private transport detail"));
    assert!(html.contains("Outcome unknown"));
    for uri in [
        format!("/i/{ACTIVE_SLUG}"),
        format!("/i/{ACTIVE_SLUG}?operation_id={id}"),
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
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}?fresh=true"))
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
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    assert_eq!(
        ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/admit"))
            .count(),
        1
    );
}

/// Playwright drives this real router with JavaScript disabled. Authentication
/// is obtained via the existing public OAuth test flow before serving.
#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn admission_browser_server() {
    use axum::response::IntoResponse;
    let current = Arc::new(Mutex::new(None::<(axum::Router, wiremock::MockServer)>));
    let control = Arc::new(Mutex::new((false, "pending".into())));
    let login_current = current.clone();
    let login_control = control.clone();
    let app = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let current = login_current.clone();
                let control = login_control.clone();
                async move {
                    *control.lock().unwrap() = (false, "pending".into());
                    let (app, ingress) = lost_response_app_with_control(control).await;
                    let cookie = sign_in(app.clone()).await;
                    *current.lock().unwrap() = Some((app, ingress));
                    (
                        [(
                            "set-cookie",
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to(&format!("/i/{ACTIVE_SLUG}")),
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
                    let control = control.clone();
                    async move {
                        let mut state = control.lock().unwrap();
                        if let Some(revoked) = query.get("revoked") {
                            state.0 = revoked == "true";
                        }
                        if let Some(status) = query.get("status") {
                            state.1 = status.clone();
                        }
                        StatusCode::NO_CONTENT
                    }
                },
            ),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = current.lock().unwrap().as_ref().unwrap().0.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4174")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[tokio::test]
async fn authoritative_native_form_validates_identity_and_preserves_unknown_input_without_sql_link()
{
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let ingress = MockServer::start().await;
    let link_id = InvitationLinkId::new();
    Mock::given(method("POST"))
        .and(path(format!("/{CODE_SERVICE}/{ACTIVE_SLUG}/resolve")))
        .respond_with(ResponseTemplate::new(200).set_body_json(link_id))
        .mount(&ingress)
        .await;
    Mock::given(path(format!("/{LINK_SERVICE}/{link_id}/requester_page")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "link_id": link_id, "invitation_code": ACTIVE_SLUG, "repos": [], "permission": "pull",
            "approval_required": true, "can_start_fresh": true, "attempt": null, "request": null
        })))
        .mount(&ingress)
        .await;
    Mock::given(path(format!("/{LINK_SERVICE}/{link_id}/prepare_attempt")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    let state = AppState::new(
        Arc::new(storage),
        Arc::new(MockTransport::scripted(oauth_expectations(
            "octocat",
            REQUESTER_ID,
        ))),
        Arc::new(UnusedCommands),
        Arc::new(ghinvite_web::RestateClient::new(ingress.uri()).unwrap()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let backend = ghinvite_web::session_store::SqliteBackend::new(
        sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap(),
    );
    backend.migrate().await.unwrap();
    let app = build_app(
        state,
        ghinvite_web::session_store::ProtectedStore::new(backend, [7; 32]),
    );
    let cookie = sign_in(app.clone()).await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
                    .uri(format!("/i/{ACTIVE_SLUG}"))
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
    let calls = ingress.received_requests().await.unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|r| r.url.path().ends_with("prepare_attempt"))
            .count(),
        1
    );
    // Preparation itself was unavailable: recover from the explicitly saved
    // session continuation after navigating away from the failed POST.
    ingress.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&ingress)
        .await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}?operation_id={operation}"))
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
            .uri(format!("/i/{ACTIVE_SLUG}"))
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
    for (id, text) in [
        (operation, "original"),
        (second, "second"),
        (third, "third"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/i/{ACTIVE_SLUG}?operation_id={id}"))
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(body_text(response).await.contains(text));
    }
}

#[tokio::test]
async fn request_submission_requires_the_rendered_session_token() {
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
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
                    .uri(format!("/i/{ACTIVE_SLUG}"))
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
    assert!(ingress.received_requests().await.unwrap().is_empty());
    // The authority already holds this attempt's receipt, so preparing it
    // settles the admission.
    wiremock::Mock::given(wiremock::matchers::path_regex("/prepare_attempt$"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "input": {"link_id": RequestId::new(), "operation_id": id,
                    "requester_id": REQUESTER_ID, "justification": "Native request"},
                "receipt": {"decided_at": "2026-09-14T12:00:00Z", "result": {"kind": "accepted",
                    "request_id": RequestId::new(), "state": "pending", "decision_deadline": null}}
            })),
        )
        .with_priority(1)
        .mount(&ingress)
        .await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
        format!("/i/{ACTIVE_SLUG}?operation_id={id}")
    );
    let prepared: Vec<_> = ingress
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/prepare_attempt"))
        .collect();
    assert_eq!(prepared.len(), 1);
    let command: serde_json::Value = serde_json::from_slice(&prepared[0].body).unwrap();
    assert_eq!(command["operation_id"], id.to_string());
    assert_eq!(command["requester_id"], REQUESTER_ID);
    assert_eq!(command["justification"], "Native request");
    assert!(!String::from_utf8_lossy(&prepared[0].body).contains(&token));
}

/// An existing pending or approved request is the authority's call, not the
/// projection's; its rejection is final and offers no resubmission.
#[tokio::test]
async fn submission_rejected_for_an_existing_request_is_final() {
    let (app, ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;
    let token = common::csrf_token(&app, &cookie).await;
    let id = RequestId::new();
    wiremock::Mock::given(wiremock::matchers::path_regex("/prepare_attempt$"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "input": {"link_id": RequestId::new(), "operation_id": id,
                    "requester_id": REQUESTER_ID, "justification": null},
                "receipt": {"decided_at": "2026-09-14T12:00:00Z",
                    "result": {"kind": "rejected", "reason": "existing_request"}}
            })),
        )
        .with_priority(1)
        .mount(&ingress)
        .await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={token}&operation_id={id}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = body_text(response).await;
    assert!(html.contains("you already have a pending or approved request"));
    assert!(html.contains("result is final."));
    assert!(!html.contains("Submit request"));
    assert!(!html.contains("Retry same attempt"));
    assert!(
        ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().ends_with("/admit"))
    );
}

const ACTIVE_SLUG: &str = "abcdEFGH01234567";
const UNKNOWN_SLUG: &str = "ZZZZZZZZZZZZZZZZ";
const CREATOR_ID: u64 = 701;
const REQUESTER_ID: u64 = 802;

fn encoded_return_to(path: &str) -> String {
    url::form_urlencoded::byte_serialize(path.as_bytes()).collect()
}

/// The requester routes reach Restate through the admission client, never
/// through the installation command facade.
#[derive(Default)]
struct UnusedCommands;

#[async_trait::async_trait]
impl GhinviteCommands for UnusedCommands {
    async fn onboard_installation(
        &self,
        _command: OnboardInstallation,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected onboard_installation command")
    }

    async fn record_repository_selection_change(
        &self,
        _command: RecordRepositorySelectionChange,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected record_repository_selection_change command")
    }

    async fn record_installation_uninstalled(
        &self,
        _command: RecordInstallationUninstalled,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected record_installation_uninstalled command")
    }

    async fn route_github_invitation_webhook(
        &self,
        _command: RouteGithubInvitationWebhook,
    ) -> ghinvite_web::Result<()> {
        panic!("unexpected route_github_invitation_webhook command")
    }
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
        last_seen_at: dt("2026-05-04T12:00:00Z"),
    }
}

fn active_link(slug: &str) -> InvitationLink {
    InvitationLink {
        id: InvitationLinkId::new(),
        slug: Slug::from_string(slug.to_string()).unwrap(),
        installation_id: 1,
        account_id: 9001,
        created_by: CREATOR_ID,
        created_at: dt("2026-05-04T12:00:00Z"),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: true,
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "acme/api".into(),
        }],
    }
}

fn request_with_state(
    id: RequestId,
    link: InvitationLinkId,
    requester_id: u64,
    state: RequestState,
) -> InvitationRequest {
    InvitationRequest {
        id,
        invitation_link_id: link,
        requester_id,
        justification: None,
        state,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
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
        .insert_installation(&sample_account())
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(CREATOR_ID, "creator"))
        .await
        .unwrap();
    storage.seed_link(&link).await.unwrap();
    let state = AppState::new(
        Arc::new(storage),
        Arc::new(mock),
        Arc::new(UnusedCommands),
        Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    build_app(state, tower_sessions::MemoryStore::default())
}

#[tokio::test]
async fn requester_status_comes_from_the_authority_and_hides_the_decline_reason() {
    let (app, ingress) = lost_response_app().await;
    let request = RequestId::new();
    // No projected request exists; the authority's requester page is the
    // requester's only source of truth.
    wiremock::Mock::given(wiremock::matchers::path_regex("/requester_page$"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "link_id": InvitationLinkId::new(), "invitation_code": ACTIVE_SLUG,
            "repos": [{"repo_id": 1, "repo_full_name": "acme/api"}], "permission": "pull",
            "approval_required": true, "can_start_fresh": false, "attempt": null,
            "request": {"request_id": request, "link_id": InvitationLinkId::new(),
                "account_id": 9001, "requester_id": REQUESTER_ID, "justification": null,
                "state": "declined", "admitted_at": "2026-01-01T00:00:00Z",
                "decision_deadline": "2026-01-08T00:00:00Z", "revision": 2,
                "decision": {"decision_id": "decision", "decided_by": 7,
                    "effective_at": "2026-01-02T00:00:00Z", "evaluated_at": "2026-01-02T00:00:00Z",
                    "decline_reason": "Private admin context"}}
        })))
        .with_priority(1)
        .mount(&ingress)
        .await;
    let cookie = sign_in(app.clone()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
    let queries: Vec<serde_json::Value> = ingress
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/requester_page"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0]["requester_id"], REQUESTER_ID);
}

fn oauth_expectations(login: &str, user_id: u64) -> Vec<Expectation> {
    vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user"}"#
                    .to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": user_id, "login": login}),
        ),
    ]
}

fn session_cookie(resp: &axum::response::Response, fallback: Option<String>) -> String {
    resp.headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .or(fallback)
        .expect("session cookie available")
}

fn state_from_location(location: &str) -> &str {
    location
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
}

async fn sign_in(app: axum::Router) -> String {
    let resp1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie1 = session_cookie(&resp1, None);
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state = state_from_location(location);

    let resp2 = app
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state}"))
                .header("cookie", &cookie1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    session_cookie(&resp2, Some(cookie1))
}

#[tokio::test]
async fn landing_unauthenticated_redirects_to_login_for_active_slug() {
    let app = build_test_app(active_link(ACTIVE_SLUG), MockTransport::scripted(vec![])).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}"))
        )
    );
}

#[tokio::test]
async fn landing_unauthenticated_redirects_to_login_for_bad_or_inactive_slugs() {
    let mut revoked = active_link(ACTIVE_SLUG);
    revoked.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    revoked.revoked_by = Some(CREATOR_ID);

    let mut expired = active_link(ACTIVE_SLUG);
    expired.expires_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());

    let mut exhausted = active_link(ACTIVE_SLUG);
    exhausted.max_uses = Some(1);
    exhausted.uses_count = 1;

    for (link, uri, expected_return_to) in [
        (
            active_link(ACTIVE_SLUG),
            "/i/not-a-valid-slug".to_string(),
            "/i/not-a-valid-slug".to_string(),
        ),
        (
            active_link(ACTIVE_SLUG),
            format!("/i/{UNKNOWN_SLUG}"),
            format!("/i/{UNKNOWN_SLUG}"),
        ),
        (
            revoked,
            format!("/i/{ACTIVE_SLUG}"),
            format!("/i/{ACTIVE_SLUG}"),
        ),
        (
            expired,
            format!("/i/{ACTIVE_SLUG}"),
            format!("/i/{ACTIVE_SLUG}"),
        ),
        (
            exhausted,
            format!("/i/{ACTIVE_SLUG}"),
            format!("/i/{ACTIVE_SLUG}"),
        ),
    ] {
        let app = build_test_app(link, MockTransport::scripted(vec![])).await;
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert_eq!(
            location,
            format!(
                "/login?return_to={}",
                encoded_return_to(&expected_return_to)
            )
        );
    }
}

#[tokio::test]
async fn submit_unauthenticated_redirects_to_login_for_canonical_page() {
    let app = build_test_app(active_link(ACTIVE_SLUG), MockTransport::scripted(vec![])).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=ship",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}"))
        )
    );
}

#[tokio::test]
async fn unsupported_nested_invitation_routes_authenticate_before_404() {
    let app = build_test_app(
        active_link(ACTIVE_SLUG),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}/request"))
        )
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request?foo=1&bar=2"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        format!(
            "/login?return_to={}",
            encoded_return_to(&format!("/i/{ACTIVE_SLUG}/request?foo=1&bar=2"))
        )
    );

    let cookie = sign_in(app.clone()).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
    assert!(!text.contains("Request repository access"));
    assert!(!text.contains("Submit request"));
}

#[tokio::test]
async fn requester_form_carries_csp_and_only_external_script() {
    let (app, _ingress) = lost_response_app().await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
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
