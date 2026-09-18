use super::*;
use std::sync::atomic::{AtomicU16, Ordering};

#[tokio::test]
async fn authorization_read_failure_preserves_submission_without_account_disclosure() {
    let mut expectations = oauth_expectations_without_membership();
    expectations.push(Expectation::status(
        Method::Get,
        "https://api.github.com/user/memberships/orgs/acme",
        503,
    ));
    expectations.extend([
        oauth_expectations().pop().unwrap(),
        installation_repos_expectation(),
    ]);
    let (app, cookie, calls) =
        build_signed_in_admin_app_with_recording_commands(expectations).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let body = format!(
        "csrf_token={csrf}&description=Preserve+me&permission=push&repo_ids=10&repo_ids=11"
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_server_error());
    let html = response_html(response).await;
    assert!(html.contains("Access verification is temporarily unavailable"));
    assert!(html.contains("Preserve me"));
    assert!(html.contains("name=\"repo_ids\" value=\"10\""));
    assert!(html.contains("name=\"repo_ids\" value=\"11\""));
    assert!(!html.contains("acme/api"));
    assert!(html.contains("Retry access verification"));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn overview_keeps_unavailable_sections_distinct_from_empty_work() {
    for (table, unavailable, healthy) in [
        (
            "invitation_requests",
            "Pending invitation requests are unavailable",
            "0 active invitation links",
        ),
        (
            "invitation_link_repos",
            "Invitation links are unavailable",
            "0 pending invitation requests",
        ),
    ] {
        let path = std::env::temp_dir().join(format!(
            "ghinvite-overview-{}.sqlite",
            ghinvite_core::InvitationLinkId::new()
        ));
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
                .await
                .unwrap(),
        );
        let (app, cookie) = links_app(storage.clone(), "admin").await;
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&path),
        )
        .await
        .unwrap();
        sqlx::query(&format!("DROP TABLE {table}"))
            .execute(&pool)
            .await
            .unwrap();
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let html = response_html(response).await;
        assert!(html.contains(unavailable), "{html}");
        assert!(html.contains(healthy));
        assert!(html.contains("href=\"/console/accounts/acme\">Try again</a>"));
        assert!(!html.contains(table));
        if table == "invitation_link_repos" {
            assert!(!html.contains("No invitation links yet"));
            assert!(!html.contains("0 active invitation links"));
        } else {
            assert!(!html.contains("0 pending invitation requests"));
        }
        pool.close().await;
        drop(app);
        drop(storage);
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn settings_distinguishes_installed_historical_and_unavailable_state() {
    for (uninstalled, upstream, label, status, destination) in [
        (
            false,
            200,
            "Installed",
            StatusCode::OK,
            "https://github.com/settings/installations/77",
        ),
        (true, 200, "Uninstalled", StatusCode::OK, "/install"),
        (
            false,
            503,
            "Installation availability could not be verified",
            StatusCode::BAD_GATEWAY,
            "/console/accounts/octocat/settings",
        ),
    ] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let account = identity_account(42, "octocat", AccountType::User);
        storage.insert_installation(&account).await.unwrap();
        if uninstalled {
            storage
                .mark_installation_uninstalled(account.installation_id, Utc::now())
                .await
                .unwrap();
        }
        let mut expectations = oauth_sign_in_expectations();
        if !uninstalled {
            let mut repos = installation_repos_expectation();
            repos.response.status = upstream;
            expectations.push(repos);
        }
        let state = AppState::new(
            storage,
            Arc::new(MockTransport::scripted(expectations)),
            Arc::new(RecordingCommands::default()),
            WebConfig::for_local_dev_with_secret([7; 32]),
        )
        .with_admission(Arc::new(RestateClient::new("http://127.0.0.1:1").unwrap()));
        let (app, cookie) = sign_in(build_app(state, tower_sessions::MemoryStore::default())).await;
        let response =
            identity_request(&app, &cookie, "GET", "/console/accounts/octocat/settings").await;
        assert_eq!(response.status(), status);
        let html = response_html(response).await;
        assert!(html.contains(label), "{html}");
        assert!(html.contains(&format!("href=\"{destination}\"")));
        assert!(!html.contains("reflects the active installation state"));
        if uninstalled || upstream != 200 {
            assert!(html.contains("Last recorded installation scope"));
        }
    }
}

#[tokio::test]
async fn repository_failures_preserve_native_form_and_retry_before_creation() {
    for (upstream, message, status) in [
        (
            504,
            "GitHub did not respond in time",
            StatusCode::GATEWAY_TIMEOUT,
        ),
        (
            429,
            "GitHub is limiting requests",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            401,
            "GitHub authorization needs attention",
            StatusCode::BAD_GATEWAY,
        ),
        (
            403,
            "GitHub denied repository access",
            StatusCode::BAD_GATEWAY,
        ),
        (
            500,
            "Repositories could not be loaded",
            StatusCode::BAD_GATEWAY,
        ),
    ] {
        let mut expectations = oauth_expectations();
        let mut failure = installation_repos_expectation();
        failure.response.status = upstream;
        failure.response.body = b"private-upstream-diagnostic".to_vec();
        let membership = oauth_expectations().pop().unwrap();
        expectations.extend([
            failure.clone(),
            membership.clone(),
            failure,
            membership,
            installation_repos_expectation(),
        ]);
        let (app, cookie, calls) =
            build_signed_in_admin_app_with_recording_commands(expectations).await;
        let response =
            identity_request(&app, &cookie, "GET", "/console/accounts/acme/links/new").await;
        assert_eq!(response.status(), status);
        let html = response_html(response).await;
        assert!(html.contains(message));
        assert!(!html.contains("No repositories are available"));
        let csrf = common::csrf_token(&app, &cookie).await;
        let values = "description=Workshop&internal_note=Keep+this&permission=push&approval_required=true&max_uses=7&expires_in_days=9&repo_ids=10&repo_ids=11";
        for (retry, expected_status) in [("", status), ("&reload_repos=true", StatusCode::OK)] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/console/accounts/acme/links")
                        .header("cookie", &cookie)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(Body::from(format!("csrf_token={csrf}&{values}{retry}")))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected_status);
            let html = response_html(response).await;
            for value in [
                "Workshop",
                "Keep this",
                "value=\"7\"",
                "value=\"9\"",
                "value=\"push\" selected",
                "name=\"repo_ids\" value=\"10\"",
                "name=\"repo_ids\" value=\"11\"",
            ] {
                assert!(html.contains(value), "missing {value}: {html}");
            }
            assert!(!html.contains("private-upstream-diagnostic"));
            if retry.is_empty() {
                assert!(html.contains("Retry loading repositories"));
                assert!(!html.contains("No repositories are available"));
                assert!(!html.contains("src=\"/assets/ghinvite-island.js\""));
            } else {
                assert!(html.contains("value=\"10\" checked"));
                assert!(html.contains("value=\"11\" checked"));
            }
            assert!(calls.lock().unwrap().is_empty());
        }
    }
}

struct RepositoryFailureTransport {
    oauth: MockTransport,
    status: Arc<AtomicU16>,
}

#[async_trait::async_trait]
impl ghinvite_github::HttpTransport for RepositoryFailureTransport {
    async fn send(
        &self,
        request: ghinvite_github::transport::Request,
    ) -> ghinvite_github::Result<Response> {
        if request.url.contains("/repositories?") {
            let status = self.status.load(Ordering::SeqCst);
            if status == 0 {
                return Err(ghinvite_github::Error::Transport(
                    "timeout: private credential".into(),
                ));
            }
            let mut response = installation_repos_expectation().response;
            response.status = status;
            return Ok(response);
        }
        self.oauth.send(request).await
    }
}

async fn repository_recovery_app(status: Arc<AtomicU16>) -> axum::Router {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .insert_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    build_app(
        AppState::new(
            storage,
            Arc::new(RepositoryFailureTransport {
                oauth: MockTransport::scripted(oauth_sign_in_expectations()),
                status,
            }),
            Arc::new(RecordingCommands::default()),
            WebConfig::for_local_dev_with_secret([7; 32]),
        ),
        tower_sessions::MemoryStore::default(),
    )
}

#[tokio::test]
async fn repository_transport_timeout_is_not_an_empty_installation() {
    let (app, cookie) = sign_in(repository_recovery_app(Arc::new(AtomicU16::new(0))).await).await;
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/links/new").await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    let html = response_html(response).await;
    assert!(html.contains("GitHub did not respond in time"));
    assert!(!html.contains("private credential"));
    assert!(!html.contains("No repositories are available"));
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn read_recovery_browser_server() {
    use axum::response::IntoResponse;
    let status = Arc::new(AtomicU16::new(200));
    let app = repository_recovery_app(status.clone()).await;
    let login_app = app.clone();
    let fixture = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let app = login_app.clone();
                async move {
                    let (_, cookie) = sign_in(app).await;
                    (
                        [(
                            axum::http::header::SET_COOKIE,
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to("/console/accounts/octocat/links/new"),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/fixture-status/{status}",
            axum::routing::post(
                move |axum::extract::Path(value): axum::extract::Path<u16>| {
                    let status = status.clone();
                    async move {
                        status.store(value, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                },
            ),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4175")
        .await
        .unwrap();
    axum::serve(listener, fixture).await.unwrap();
}
