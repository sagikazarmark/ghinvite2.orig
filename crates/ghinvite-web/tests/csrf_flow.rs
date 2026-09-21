//! Browser HTTP boundary with the production command adapter and a Restate HTTP stub.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use chrono::Utc;
use common::link_authority::{self, CODE_SERVICE, LINK_SERVICE};
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos, Slug, storage::Storage,
};
use ghinvite_github::{
    mocks::{Expectation, MockTransport},
    transport::Method,
};
use ghinvite_web::{AppState, RestateClient, RestateCommands, WebConfig, build_app};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

mod common;

struct Browser {
    app: axum::Router,
    cookie: String,
    link: InvitationLink,
    request_id: RequestId,
    ingress: MockServer,
}

impl Browser {
    async fn new() -> Self {
        let ingress = MockServer::start().await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        storage
            .insert_installation(&Account {
                installation_id: 77,
                account_id: 42,
                account_login: "octocat".into(),
                account_type: AccountType::User,
                installed_at: Utc::now(),
                uninstalled_at: None,
                selected_repos: SelectedRepos::All,
            })
            .await
            .unwrap();
        for user_id in [42, 99] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: format!("user{user_id}"),
                    avatar_url: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let link = InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string("abcdEFGH01234567".into()).unwrap(),
            installation_id: 77,
            account_id: 42,
            created_by: 42,
            created_at: Utc::now(),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required: true,
            description: "Workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "octocat/api".into(),
            }],
        };
        storage.seed_link(&link).await.unwrap();
        let request_id = RequestId::new();
        storage
            .seed_request(&InvitationRequest {
                id: request_id,
                invitation_link_id: link.id,
                requester_id: 99,
                justification: None,
                state: RequestState::Pending,
                decided_by: None,
                decided_at: None,
                decline_reason: None,
                decision_deadline: None,
                created_at: Utc::now(),
            })
            .await
            .unwrap();
        let mut expectations = vec![
            Expectation::ok_json(
                Method::Post,
                "https://github.com/login/oauth/access_token",
                serde_json::json!({"access_token":"user-token", "token_type":"bearer", "scope":"read:user"}),
            ),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.com/user",
                serde_json::json!({"id":42, "login":"octocat"}),
            ),
        ];
        for _ in 0..8 {
            expectations.push(Expectation::ok_json(Method::Get,
                "https://api.github.com/user/installations/77/repositories?per_page=100",
                serde_json::json!({"total_count":1, "repositories":[{"id":10,"full_name":"octocat/api","private":true}]})));
        }
        let restate = Arc::new(RestateClient::new(ingress.uri()).unwrap());
        let app = build_app(
            AppState::new(
                storage,
                Arc::new(MockTransport::scripted(expectations)),
                Arc::new(RestateCommands::new(restate.clone())),
                restate,
                WebConfig::for_local_dev_with_secret([7; 32]),
            ),
            tower_sessions::MemoryStore::default(),
        );
        let mut browser = Self {
            app,
            cookie: String::new(),
            link,
            request_id,
            ingress,
        };
        let login = browser.get("/login").await;
        browser.cookie = cookie(&login);
        let location = url::Url::parse(login.headers()["location"].to_str().unwrap()).unwrap();
        let state = location
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let response = browser
            .get(&format!("/oauth/callback?code=test&state={state}"))
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        browser.cookie = cookie(&response);
        browser
    }

    async fn get(&self, path: &str) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("cookie", &self.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post(&self, path: &str, body: String) -> Response {
        self.app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("cookie", &self.cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

fn cookie(response: &Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn html(response: Response) -> String {
    assert_eq!(response.headers()["cache-control"], "private, no-store");
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

impl Browser {
    fn link_path(&self, method: &str) -> String {
        format!("/{LINK_SERVICE}/{}/{method}", self.link.id)
    }

    /// The authority's view of the browser's invitation link.
    fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(link_authority::snapshot(&self.link)).unwrap()
    }

    /// Serve the authoritative reads the browser's pages make: link status for
    /// the Console and the requester page for the invitation link.
    async fn mount_reads(&self) {
        Mock::given(path(self.link_path("link_status")))
            .respond_with(ResponseTemplate::new(200).set_body_json(self.snapshot()))
            .mount(&self.ingress)
            .await;
        Mock::given(path(format!(
            "/{CODE_SERVICE}/{}/resolve",
            self.link.slug.as_str()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(self.link.id))
        .mount(&self.ingress)
        .await;
        Mock::given(path(self.link_path("requester_page")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "link_id": self.link.id, "invitation_code": self.link.slug.as_str(),
                "repos": self.link.repos, "permission": "pull", "approval_required": true,
                "can_start_fresh": true, "attempt": null, "request": null
            })))
            .mount(&self.ingress)
            .await;
    }
}

fn decision_receipt(link: InvitationLinkId, request: RequestId, state: &str) -> serde_json::Value {
    serde_json::json!({"outcome": "applied", "request": {
        "request_id": request, "link_id": link, "account_id": 42, "requester_id": 99,
        "state": state, "admitted_at": "2026-09-14T12:00:00Z",
        "decision_deadline": "2026-09-21T12:00:00Z", "revision": 2}})
}

#[tokio::test]
async fn create_service_failure_keeps_the_attempt_and_token_and_allows_retry() {
    let browser = Browser::new().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&browser.ingress)
        .await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let link_id = InvitationLinkId::new();
    let uri = format!(
        "/console/accounts/octocat/links?link_id={link_id}&anchor={}",
        Utc::now().timestamp()
    );
    let body = format!(
        "csrf_token={token}&description=Keep+workshop&internal_note=Keep+note&permission=push&repo_ids=10&max_uses=7&expires_in_days=45"
    );
    let response = browser.post(&uri, body.clone()).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let page = html(response).await;
    // A 503 from ingress leaves the creation's effect in doubt — Restate may
    // have applied it before the response went wrong — so the page offers the
    // retained original attempt rather than inviting a blind new creation.
    assert!(page.contains("Outcome unknown"), "{page}");
    assert!(!page.contains("Please try again"), "{page}");
    assert!(page.contains(&format!(
        "/console/accounts/octocat/attempts/create-{link_id}"
    )));
    assert!(page.contains(&token), "the retry form keeps the token");
    browser.ingress.reset().await;
    let mut link = browser.link.clone();
    link.id = link_id;
    Mock::given(path(format!("/{LINK_SERVICE}/{link_id}/create")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::to_value(link_authority::snapshot(&link)).unwrap()),
        )
        .mount(&browser.ingress)
        .await;
    // Resubmitting the same form replays the same creation identity.
    let response = browser.post(&uri, body).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/console/accounts/octocat/links/{link_id}")
    );
    let created = browser.ingress.received_requests().await.unwrap();
    assert_eq!(created.len(), 1);
    let command: serde_json::Value = serde_json::from_slice(&created[0].body).unwrap();
    assert_eq!(command["description"], "Keep workshop");
    assert_eq!(command["internal_note"], "Keep note");
    assert_eq!(command["max_uses"], 7);
    assert!(!String::from_utf8_lossy(&created[0].body).contains(&token));
}

#[tokio::test]
async fn requester_service_failure_preserves_justification_operation_id_and_token() {
    let browser = Browser::new().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&browser.ingress)
        .await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let id = RequestId::new();
    let body = format!("csrf_token={token}&operation_id={id}&justification=Keep+my+context");
    let code = browser.link.slug.as_str();
    let path_ = format!("/i/{code}");
    let response = browser.post(&path_, body.clone()).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let page = html(response).await;
    for value in [
        "Keep my context",
        // A 503 leaves admission in doubt, and an admitted request spends a
        // use of the invitation link. The page offers the same attempt
        // rather than a blind resubmit; the preserved operation ID below is
        // what makes a deliberate retry idempotent.
        "Outcome unknown",
        &id.to_string(),
        &token,
    ] {
        assert!(page.contains(value), "missing {value}");
    }
    assert!(!page.contains("Please try again"), "{page}");
    browser.ingress.reset().await;
    Mock::given(path(format!("/{CODE_SERVICE}/{code}/resolve")))
        .respond_with(ResponseTemplate::new(200).set_body_json(browser.link.id))
        .mount(&browser.ingress)
        .await;
    Mock::given(path(browser.link_path("prepare_attempt")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "input": {"version": 1, "link_id": browser.link.id, "operation_id": id,
                "requester_id": 42, "justification": "Keep my context"},
            "receipt": {"decided_at": "2026-09-14T12:00:00Z", "result": {"kind": "accepted",
                "request_id": RequestId::new(), "state": "pending", "decision_deadline": null}}
        })))
        .mount(&browser.ingress)
        .await;
    let response = browser.post(&path_, body).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/i/{code}?operation_id={id}")
    );
}

#[tokio::test]
async fn every_browser_form_carries_authority_and_forgery_dispatches_nothing() {
    let browser = Browser::new().await;
    let stranger = Browser::new().await;
    browser.mount_reads().await;
    let other_token = common::csrf_token(&stranger.app, &stranger.cookie).await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let base = "/console/accounts/octocat";
    let detail = format!("{base}/links/{}", browser.link.id);
    let request = format!("/i/{}", browser.link.slug.as_str());
    for path in [
        format!("{base}/links/new"),
        format!("{detail}/edit"),
        detail.clone(),
        format!("{base}/requests"),
        request.clone(),
        "/".into(),
        "/missing".into(),
    ] {
        let page = html(browser.get(&path).await).await;
        let mut forms = 0;
        for form in page.split("<form").skip(1) {
            let form = form.split("</form>").next().unwrap();
            if !form.starts_with(" method=\"post\"") {
                continue;
            }
            forms += 1;
            assert_eq!(form.matches("name=\"csrf_token\"").count(), 1, "{path}");
            assert!(form.contains(&format!("value=\"{token}\"")), "{path}");
            let action = form
                .split("action=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            assert!(!action.contains(&token));
        }
        assert!(forms > 0, "{path}");
    }
    // Rendering read the authority; forgeries must not reach it at all.
    let reads = browser.ingress.received_requests().await.unwrap().len();
    let paths = [
        format!(
            "{base}/links?link_id={}&anchor={}",
            InvitationLinkId::new(),
            Utc::now().timestamp()
        ),
        format!("{detail}/edit"),
        format!("{detail}/revoke"),
        format!("{base}/requests/{}/approve", browser.request_id),
        format!("{base}/requests/{}/decline", browser.request_id),
        request,
        "/logout".into(),
    ];
    for path in &paths {
        for body in [
            String::new(),
            "csrf_token=wrong".into(),
            format!("csrf_token={other_token}"),
            format!("csrf_token={token}&csrf_token={token}"),
        ] {
            let response = browser
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("cookie", &browser.cookie)
                        .header("origin", "https://evil.ghinvite.test")
                        .header("sec-fetch-site", "same-site")
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }
    }
    assert_eq!(
        browser.ingress.received_requests().await.unwrap().len(),
        reads
    );
    // The same forms with the rendered token dispatch their commands.
    let declined = RequestId::new();
    Mock::given(path(browser.link_path("revoke")))
        .respond_with(ResponseTemplate::new(200).set_body_json(browser.snapshot()))
        .mount(&browser.ingress)
        .await;
    for (request, state) in [(browser.request_id, "approved"), (declined, "declined")] {
        Mock::given(path(browser.link_path("decide")))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"request_id": request}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(decision_receipt(
                browser.link.id,
                request,
                state,
            )))
            .mount(&browser.ingress)
            .await;
    }
    let lifecycle = || {
        format!(
            "csrf_token={token}&link_id={}&operation_id={}",
            browser.link.id,
            RequestId::new()
        )
    };
    for (path, body) in [
        (paths[2].clone(), format!("csrf_token={token}")),
        (paths[3].clone(), lifecycle()),
        (format!("{base}/requests/{declined}/decline"), lifecycle()),
    ] {
        assert_eq!(
            browser.post(&path, body).await.status(),
            StatusCode::SEE_OTHER,
            "{path}"
        );
    }
    let requests = browser.ingress.received_requests().await.unwrap();
    assert_eq!(requests.len(), reads + 3);
    for request in requests {
        assert!(!String::from_utf8_lossy(&request.body).contains(&token));
        assert!(!request.url.as_str().contains(&token));
    }
}

#[tokio::test]
async fn validation_and_edit_service_redisplays_keep_reusable_authority() {
    let browser = Browser::new().await;
    browser.mount_reads().await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let path_ = format!("/console/accounts/octocat/links/{}/edit", browser.link.id);
    let invalid = browser
        .post(
            &path_,
            format!("csrf_token={token}&description=&internal_note=Keep+note"),
        )
        .await;
    assert_eq!(invalid.status(), StatusCode::OK);
    let page = html(invalid).await;
    assert!(page.contains(&token) && page.contains("Keep note"));
    assert!(
        browser
            .ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().ends_with("/update_metadata"))
    );
    Mock::given(path(browser.link_path("update_metadata")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&browser.ingress)
        .await;
    let body = format!("csrf_token={token}&description=Keep+description&internal_note=Keep+note");
    let failure = browser.post(&path_, body.clone()).await;
    assert_eq!(failure.status(), StatusCode::BAD_GATEWAY);
    let page = html(failure).await;
    assert!(
        page.contains(&token) && page.contains("Keep description") && page.contains("Keep note")
    );
    browser.ingress.reset().await;
    browser.mount_reads().await;
    Mock::given(path(browser.link_path("update_metadata")))
        .respond_with(ResponseTemplate::new(200).set_body_json(browser.snapshot()))
        .mount(&browser.ingress)
        .await;
    assert_eq!(
        browser.post(&path_, body).await.status(),
        StatusCode::SEE_OTHER
    );
}
