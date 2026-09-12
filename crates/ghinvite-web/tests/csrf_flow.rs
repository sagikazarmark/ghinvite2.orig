//! Browser HTTP boundary with the production command adapter and a Restate HTTP stub.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use chrono::Utc;
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
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

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
        storage.insert_invitation_link(&link).await.unwrap();
        let request_id = RequestId::new();
        storage
            .insert_invitation_request_and_increment_uses(&InvitationRequest {
                id: request_id,
                invitation_link_id: link.id,
                requester_id: 99,
                justification: None,
                state: RequestState::Pending,
                decided_by: None,
                decided_at: None,
                decline_reason: None,
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
        let app = build_app(
            AppState::new(
                storage,
                Arc::new(MockTransport::scripted(expectations)),
                Arc::new(RestateCommands::new(Arc::new(
                    RestateClient::new(ingress.uri()).unwrap(),
                ))),
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

#[tokio::test]
async fn create_service_failure_redisplays_input_and_token_and_allows_retry() {
    let browser = Browser::new().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&browser.ingress)
        .await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let body = format!(
        "csrf_token={token}&description=Keep+workshop&internal_note=Keep+note&permission=push&repo_ids=10&max_uses=7&expires_in_days=45"
    );
    let response = browser
        .post("/console/accounts/octocat/links", body.clone())
        .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let page = html(response).await;
    for value in [
        "Keep workshop",
        "Keep note",
        "value=\"7\"",
        "value=\"45\"",
        "value=\"10\" checked",
        &token,
    ] {
        assert!(page.contains(value), "missing preserved value {value}");
    }
    assert!(page.contains("Failed to create invitation link"));
    browser.ingress.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"link_id":InvitationLinkId::new(), "slug":"abcdEFGH01234567"}),
        ))
        .mount(&browser.ingress)
        .await;
    let response = browser.post("/console/accounts/octocat/links", body).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn requester_service_failure_preserves_justification_request_id_and_token() {
    let browser = Browser::new().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&browser.ingress)
        .await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let id = RequestId::new();
    let body = format!("csrf_token={token}&request_id={id}&justification=Keep+my+context");
    let path = format!("/i/{}", browser.link.slug.as_str());
    let response = browser.post(&path, body.clone()).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let page = html(response).await;
    for value in [
        "Keep my context",
        "Failed to submit request",
        &id.to_string(),
        &token,
    ] {
        assert!(page.contains(value));
    }
    browser.ingress.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&browser.ingress)
        .await;
    assert_eq!(
        browser.post(&path, body).await.status(),
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn every_browser_form_carries_authority_and_forgery_dispatches_nothing() {
    let browser = Browser::new().await;
    let stranger = Browser::new().await;
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
    let paths = [
        format!("{base}/links"),
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
    assert!(
        browser
            .ingress
            .received_requests()
            .await
            .unwrap()
            .is_empty()
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "link_id":InvitationLinkId::new(), "slug":"abcdEFGH01234567"
        })))
        .mount(&browser.ingress)
        .await;
    for path in paths.iter().skip(2).take(3) {
        assert_eq!(
            browser
                .post(path, format!("csrf_token={token}"))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );
    }
    let requests = browser.ingress.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert!(!String::from_utf8_lossy(&request.body).contains(&token));
        assert!(!request.url.as_str().contains(&token));
    }
}

#[tokio::test]
async fn validation_and_edit_service_redisplays_keep_reusable_authority() {
    let browser = Browser::new().await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let path = format!("/console/accounts/octocat/links/{}/edit", browser.link.id);
    let invalid = browser
        .post(
            &path,
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
            .is_empty()
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&browser.ingress)
        .await;
    let body = format!("csrf_token={token}&description=Keep+description&internal_note=Keep+note");
    let failure = browser.post(&path, body.clone()).await;
    assert_eq!(failure.status(), StatusCode::BAD_GATEWAY);
    let page = html(failure).await;
    assert!(
        page.contains(&token) && page.contains("Keep description") && page.contains("Keep note")
    );
    browser.ingress.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&browser.ingress)
        .await;
    assert_eq!(
        browser.post(&path, body).await.status(),
        StatusCode::SEE_OTHER
    );
}
