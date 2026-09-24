//! Browser HTTP boundary with the production command adapter and the fake link
//! authority.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use chrono::{Duration, Utc};
use common::authority_http_fixture::FakeLinkAuthority;
use common::sign_in::{OCTOCAT, oauth_expectations, sign_in};
use ghinvite_core::admission::Admit;
use ghinvite_core::request_lifecycle::DecideRequest;
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::projection::{CreateLink, RequestSnapshot};
use ghinvite_core::storage::{InstallationStorage, RecordStorage};
use ghinvite_core::{
    Account, AccountType, InvitationLink, InvitationLinkId, InvitationLinkRepo, InvitationRequest,
    Permission, RequestId, RequestState, SelectedRepos,
};
use ghinvite_github::{
    mocks::{Expectation, MockTransport},
    transport::Method,
};
use ghinvite_web::{AppState, WebConfig, build_app};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

mod common;

struct Browser {
    app: axum::Router,
    cookie: String,
    link: InvitationLink,
    request_id: RequestId,
    authority: FakeLinkAuthority,
}

impl Browser {
    async fn new() -> Self {
        let authority = FakeLinkAuthority::start().await;
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
        authority.seed_link(&link);
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
        authority.seed_request(pending(&link, request_id));
        let mut expectations = oauth_expectations(OCTOCAT.with_access_token("user-token"));
        for _ in 0..8 {
            expectations.push(Expectation::ok_json(Method::Get,
                "https://api.github.com/user/installations/77/repositories?per_page=100",
                serde_json::json!({"total_count":1, "repositories":[{"id":10,"full_name":"octocat/api","private":true}]})));
        }
        let app = build_app(
            AppState::new(
                storage,
                Arc::new(MockTransport::scripted(expectations)),
                authority.client(),
                WebConfig::for_local_dev_with_secret([7; 32]),
            ),
            tower_sessions::MemoryStore::default(),
        );
        let cookie = sign_in(&app).await;
        Self {
            app,
            cookie,
            link,
            request_id,
            authority,
        }
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

/// The authority's record of an invitation request on `link` awaiting review.
fn pending(link: &InvitationLink, request_id: RequestId) -> RequestSnapshot {
    RequestSnapshot {
        request_id,
        link_id: link.id,
        account_id: link.account_id,
        requester_id: 99,
        justification: None,
        state: RequestState::Pending,
        admitted_at: Utc::now(),
        decision_deadline: Some(Utc::now() + Duration::days(7)),
        revision: 1,
        decision: None,
    }
}

/// Every call body that reached the authority.
fn bodies(authority: &FakeLinkAuthority) -> Vec<String> {
    let mut methods = authority.calls();
    methods.sort();
    methods.dedup();
    methods
        .iter()
        .flat_map(|method| authority.received::<serde_json::Value>(method))
        .map(|body| body.to_string())
        .collect()
}

#[tokio::test]
async fn create_service_failure_keeps_the_attempt_and_token_and_allows_retry() {
    let browser = Browser::new().await;
    // The creation applies but its acknowledgement is lost.
    browser.authority.lose_acknowledgements("create");
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
    let before = browser.authority.calls().len();
    // Resubmitting the same form replays the same creation identity.
    let response = browser.post(&uri, body).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/console/accounts/octocat/links/{link_id}")
    );
    assert_eq!(browser.authority.calls()[before..], ["create"]);
    assert_eq!(
        browser.authority.applied(),
        ["create"],
        "the retry replayed it"
    );
    let created = browser.authority.created();
    let command: &CreateLink = created.last().unwrap();
    assert_eq!(command.link_id, link_id);
    assert_eq!(command.description, "Keep workshop");
    assert_eq!(command.internal_note.as_deref(), Some("Keep note"));
    assert_eq!(command.max_uses, Some(7));
    assert!(
        bodies(&browser.authority)
            .iter()
            .all(|b| !b.contains(&token))
    );
}

#[tokio::test]
async fn requester_service_failure_preserves_justification_operation_id_and_token() {
    let browser = Browser::new().await;
    // Admission applies but its acknowledgement is lost.
    browser.authority.lose_acknowledgements("admit");
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let id = RequestId::new();
    let body = format!("csrf_token={token}&operation_id={id}&justification=Keep+my+context");
    let code = &browser.link.id.to_string();
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
    // Retrying the same attempt recovers the retained receipt.
    let response = browser.post(&path_, body).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        format!("/i/{code}?operation_id={id}")
    );
    let prepared = browser.authority.received::<Admit>("prepare_attempt");
    assert_eq!(prepared.len(), 2);
    assert_eq!(prepared[0], prepared[1], "the retry sends the same attempt");
    assert_eq!(browser.authority.applied(), ["prepare_attempt", "admit"]);
}

#[tokio::test]
async fn every_browser_form_carries_authority_and_forgery_dispatches_nothing() {
    let browser = Browser::new().await;
    let stranger = Browser::new().await;
    let other_token = common::csrf_token(&stranger.app, &stranger.cookie).await;
    let token = common::csrf_token(&browser.app, &browser.cookie).await;
    let base = "/console/accounts/octocat";
    let detail = format!("{base}/links/{}", browser.link.id);
    let request = format!("/i/{}", browser.link.id);
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
    let reads = browser.authority.calls().len();
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
    assert_eq!(browser.authority.calls().len(), reads);
    // The same forms with the rendered token dispatch their commands.
    let declined = RequestId::new();
    browser
        .authority
        .seed_request(pending(&browser.link, declined));
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
    assert_eq!(
        browser.authority.calls()[reads..],
        ["revoke", "decide", "decide"]
    );
    let decided: Vec<_> = browser
        .authority
        .received::<DecideRequest>("decide")
        .into_iter()
        .map(|command| command.request_id)
        .collect();
    assert_eq!(decided, [browser.request_id, declined]);
    assert_eq!(
        browser.authority.request(declined).unwrap().state,
        RequestState::Declined
    );
    assert!(
        bodies(&browser.authority)
            .iter()
            .all(|b| !b.contains(&token))
    );
}

#[tokio::test]
async fn validation_and_edit_service_redisplays_keep_reusable_authority() {
    let browser = Browser::new().await;
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
        !browser
            .authority
            .calls()
            .contains(&"update_metadata".to_owned())
    );
    browser.authority.fail_once("update_metadata", 503);
    let body = format!("csrf_token={token}&description=Keep+description&internal_note=Keep+note");
    let failure = browser.post(&path_, body.clone()).await;
    assert_eq!(failure.status(), StatusCode::BAD_GATEWAY);
    let page = html(failure).await;
    assert!(
        page.contains(&token) && page.contains("Keep description") && page.contains("Keep note")
    );
    assert_eq!(
        browser.post(&path_, body).await.status(),
        StatusCode::SEE_OTHER
    );
}
