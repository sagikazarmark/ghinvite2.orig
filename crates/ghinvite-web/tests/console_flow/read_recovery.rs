use super::*;

use std::sync::atomic::{AtomicU16, Ordering};

struct MembershipFailureTransport {
    oauth: MockTransport,
    status: u16,
}

impl ghinvite_github::HttpTransport for MembershipFailureTransport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<Response, ghinvite_github::TransportError> {
        if request.url.contains("/memberships/") {
            if self.status == 0 {
                return Err(ghinvite_github::TransportError::SentNoResponse { timed_out: false });
            }
            return Ok(Response {
                status: self.status,
                headers: Default::default(),
                body: b"private upstream diagnostic".to_vec(),
            });
        }
        self.oauth.send(request).await
    }
}

#[tokio::test]
async fn github_authorization_failures_use_dependency_statuses_without_disclosing_diagnostics() {
    for (upstream, expected) in [
        (0, StatusCode::SERVICE_UNAVAILABLE),
        (500, StatusCode::SERVICE_UNAVAILABLE),
        (401, StatusCode::SERVICE_UNAVAILABLE),
        (429, StatusCode::SERVICE_UNAVAILABLE),
        (504, StatusCode::SERVICE_UNAVAILABLE),
        (404, StatusCode::NOT_FOUND),
    ] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        storage
            .seed_console_installation(&identity_account(9001, "acme", AccountType::Organization))
            .await
            .unwrap();
        seed_org_authority(
            &storage,
            &identity_account(9001, "acme", AccountType::Organization),
        )
        .await;
        let state = crate::common::sign_in::app_state(
            storage,
            Arc::new(MembershipFailureTransport {
                oauth: MockTransport::scripted(oauth_expectations(OCTOCAT)),
                status: upstream,
            }),
            std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
            console_config(),
        );
        let app = build_app(state, tower_sessions::MemoryStore::default());
        let cookie = sign_in(&app).await;
        let response = identity_request(&app, &cookie, "GET", "/console/accounts/acme").await;
        assert_eq!(response.status(), expected, "upstream {upstream}");
        let html = response_html(response).await;
        assert!(!html.contains("private"));
        assert!(!html.contains("Console overview"));
    }
}

#[tokio::test]
async fn historical_settings_require_current_authority() {
    for (account_id, kind, authorized) in [
        (42, AccountType::User, true),
        (999, AccountType::User, false),
        (9001, AccountType::Organization, true),
        (9001, AccountType::Organization, false),
    ] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let account = identity_account(account_id, "historical", kind);
        storage.seed_console_installation(&account).await.unwrap();
        storage
            .uninstall_console_installation(account.installation_id, Utc::now())
            .await
            .unwrap();
        let mut expectations = oauth_expectations(OCTOCAT);
        if kind == AccountType::Organization {
            seed_org_authority(&storage, &account).await;
            expectations.push(Expectation::ok_json(Method::Get,
                "https://api.github.com/orgs/historical/memberships/octocat",
                serde_json::json!({"role": if authorized { "admin" } else { "member" }, "state":"active", "organization":{"id":9001,"login":"historical"},"user":{"id":42,"login":"octocat"}})));
        }
        let state = crate::common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(expectations)),
            std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
            console_config(),
        );
        let app = build_app(state, tower_sessions::MemoryStore::default());
        let cookie = sign_in(&app).await;
        let response = identity_request(
            &app,
            &cookie,
            "GET",
            "/console/accounts/historical/settings",
        )
        .await;
        assert_eq!(
            response.status(),
            if authorized {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        );
        let html = response_html(response).await;
        assert_eq!(html.contains("Uninstalled."), authorized);
        if authorized {
            assert!(html.contains("href=\"/install\""));
        }
    }
}

#[tokio::test]
async fn authorization_read_failure_preserves_submission_without_account_disclosure() {
    for (upstream, expected) in [
        (503, StatusCode::SERVICE_UNAVAILABLE),
        (429, StatusCode::SERVICE_UNAVAILABLE),
        (504, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let mut expectations = oauth_expectations(OCTOCAT);
        expectations.push(Expectation::status(
            Method::Get,
            "https://api.github.com/orgs/acme/memberships/octocat",
            upstream,
        ));
        expectations.extend([
            org_membership("acme", 9001, "acme"),
            installation_repos_expectation(),
        ]);
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let account = acme_installation();
        storage.seed_console_installation(&account).await.unwrap();
        seed_org_authority(&storage, &account).await;
        let authority = FakeLinkAuthority::start().await;
        let app = build_app(
            crate::common::sign_in::app_state(
                storage,
                Arc::new(MockTransport::scripted(expectations)),
                authority.client(),
                console_config(),
            ),
            tower_sessions::MemoryStore::default(),
        );
        let cookie = sign_in(&app).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let body = format!(
            "csrf_token={csrf}&description=Preserve+me&permission=push&repo_ids=10&repo_grants=10:push&repo_ids=11&repo_grants=11:push"
        );
        let identity = format!(
            "link_id={}&anchor={}",
            ghinvite_core::InvitationLinkId::new(),
            Utc::now().timestamp()
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/console/accounts/acme/links?{identity}"))
                    .header("cookie", &cookie)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let html = response_html(response).await;
        assert!(html.contains("Access verification is temporarily unavailable"));
        assert!(html.contains("Preserve me"));
        assert!(html.contains("name=\"repo_ids\" value=\"10\""));
        assert!(html.contains("name=\"repo_ids\" value=\"11\""));
        assert!(!html.contains("acme/api"));
        assert!(html.contains("Retry access verification"));
        // The retry resubmits to the same creation identity.
        assert!(
            html.contains(&identity.replace('&', "&amp;"))
                || html.contains(&identity.replace('&', "&#38;")),
            "{html}"
        );
        assert!(authority.calls().is_empty());
    }
}

#[tokio::test]
async fn creation_storage_failure_remains_internal_error_instead_of_access_recovery() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-auth-failure-{}.sqlite",
        ghinvite_core::InvitationLinkId::new()
    ));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    sqlx::query("ALTER TABLE installations RENAME TO unavailable_installations")
        .execute(&pool)
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "csrf_token={csrf}&description=Workshop&permission=pull&repo_ids=10&repo_grants=10:pull"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let html = response_html(response).await;
    // A storage outage is not an access-verification problem, so the form must
    // not come back offering to retry verification.
    assert!(
        !html.contains("Access verification is temporarily unavailable"),
        "{html}"
    );
    assert!(!html.contains("Retry access verification"), "{html}");
    // And the page says nothing about the database beyond that it failed.
    assert!(html.contains("Something went wrong"), "{html}");
    assert!(!html.contains("installations"), "{html}");
    pool.close().await;
    drop(app);
    drop(storage);
    std::fs::remove_file(path).unwrap();
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
            "invitation_link_targets",
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
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
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
        if table == "invitation_link_targets" {
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
            StatusCode::SERVICE_UNAVAILABLE,
            "/console/accounts/octocat/settings",
        ),
    ] {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let account = identity_account(42, "octocat", AccountType::User);
        storage.seed_console_installation(&account).await.unwrap();
        if uninstalled {
            storage
                .uninstall_console_installation(account.installation_id, Utc::now())
                .await
                .unwrap();
        }
        let mut expectations = oauth_expectations(OCTOCAT);
        if !uninstalled {
            let mut repos = repos_for(42, "octocat");
            repos.response.status = upstream;
            expectations.push(repos);
        }
        let state = crate::common::sign_in::app_state(
            storage,
            Arc::new(MockTransport::scripted(expectations)),
            Arc::new(RestateClient::new("http://127.0.0.1:1").unwrap()),
            console_config(),
        );
        let app = build_app(state, tower_sessions::MemoryStore::default());
        let cookie = sign_in(&app).await;
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
            "Repository access could not be checked",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            429,
            "Repository access could not be checked",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            401,
            "Repository access could not be checked",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            403,
            "Repository access could not be checked",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            500,
            "Repository access could not be checked",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let mut expectations = oauth_expectations(ACME_ADMIN);
        let mut failure = installation_repos_expectation();
        failure.response.status = upstream;
        failure.response.body = b"private-upstream-diagnostic".to_vec();
        for _ in 0..if upstream == 401 { 4 } else { 2 } {
            expectations.push(failure.clone());
        }
        expectations.push(installation_repos_expectation());
        let (app, cookie, authority) = build_signed_in_admin_app_with_authority(expectations).await;
        let response =
            identity_request(&app, &cookie, "GET", "/console/accounts/acme/links/new").await;
        assert_eq!(response.status(), status);
        let html = response_html(response).await;
        assert!(html.contains(message));
        assert!(!html.contains("No repositories are available"));
        let csrf = common::csrf_token(&app, &cookie).await;
        let values = "description=Workshop&internal_note=Keep+this&permission=push&approval_required=true&max_uses=7&expires_in_days=9&repo_ids=10&repo_grants=10:push&repo_ids=11&repo_grants=11:push";
        let uri = format!(
            "/console/accounts/acme/links?link_id={}&anchor={}",
            ghinvite_core::InvitationLinkId::new(),
            Utc::now().timestamp()
        );
        for (retry, expected_status) in [("", status), ("&reload_repos=true", StatusCode::OK)] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&uri)
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
            assert!(authority.calls().is_empty());
        }
    }
}

struct RepositoryFailureTransport {
    oauth: MockTransport,
    status: Arc<AtomicU16>,
}

impl ghinvite_github::HttpTransport for RepositoryFailureTransport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<Response, ghinvite_github::TransportError> {
        if request.url.contains("/repositories?") {
            let status = self.status.load(Ordering::SeqCst);
            if status == 0 {
                return Err(ghinvite_github::TransportError::SentNoResponse { timed_out: false });
            }
            let mut response = repos_for(42, "octocat").response;
            response.status = status;
            return Ok(response);
        }
        self.oauth.send(request).await
    }
}

async fn repository_recovery_app(status: Arc<AtomicU16>) -> axum::Router {
    repository_recovery_app_with_cache(status, Arc::new(BrowserEvidenceCache::default())).await
}
/// Stores only evidence actually obtained from GitHub. The control ages records
/// past five minutes, exercising service freshness rather than granting access.
#[derive(Default)]
struct BrowserEvidenceCache(
    std::sync::Mutex<BTreeMap<String, ghinvite_web::authority_cache::CacheEntry>>,
);
impl BrowserEvidenceCache {
    fn expire(&self) -> usize {
        let mut rows = self.0.lock().unwrap();
        let count = rows.len();
        for row in rows.values_mut() {
            row.observed_at = Utc::now().timestamp() - 301;
        }
        count
    }
}
#[async_trait::async_trait]
impl ghinvite_web::authority_cache::AuthorityCache for BrowserEvidenceCache {
    async fn get(
        &self,
        key: &str,
    ) -> Result<
        Option<ghinvite_web::authority_cache::CacheEntry>,
        ghinvite_web::authority_cache::CacheUnavailable,
    > {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn put(
        &self,
        key: &str,
        value: ghinvite_web::authority_cache::CacheEntry,
    ) -> Result<(), ghinvite_web::authority_cache::CacheUnavailable> {
        let mut rows = self.0.lock().unwrap();
        assert!(rows.len() < 32);
        rows.insert(key.into(), value);
        Ok(())
    }
}
async fn repository_recovery_app_with_cache(
    status: Arc<AtomicU16>,
    cache: Arc<BrowserEvidenceCache>,
) -> axum::Router {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .seed_console_installation(&identity_account(42, "octocat", AccountType::User))
        .await
        .unwrap();
    ghinvite_web::build_app(
        crate::common::sign_in::with_signin_bridge(crate::common::sign_in::app_state(
            storage,
            Arc::new(RepositoryFailureTransport {
                oauth: MockTransport::scripted(
                    oauth_expectations(OCTOCAT)
                        .into_iter()
                        .chain([
                            repo_identity(10, "octocat/api", 42),
                            repo_identity(11, "octocat/web", 42),
                        ])
                        .collect(),
                ),
                status,
            }),
            std::sync::Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
            console_config(),
        ))
        .with_authority_cache(cache)
        .with_edge_limits(Arc::new(
            ghinvite_web::rate_limit::MemoryRateLimiter::default(),
        )),
        tower_sessions::MemoryStore::default(),
    )
    .layer(axum::Extension(
        ghinvite_web::rate_limit::TrustedClientIp::new(std::net::Ipv4Addr::LOCALHOST.into()),
    ))
}

#[tokio::test]
async fn repository_transport_failure_is_not_reported_as_timeout_or_empty_installation() {
    let app = repository_recovery_app(Arc::new(AtomicU16::new(0))).await;
    let cookie = sign_in(&app).await;
    let response =
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/links/new").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let html = response_html(response).await;
    assert!(html.contains("Repository access could not be checked"));
    assert!(!html.contains("GitHub did not respond in time"));
    assert!(!html.contains("private credential"));
    assert!(!html.contains("No repositories are available"));
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn read_recovery_browser_server() {
    use axum::response::IntoResponse;
    let status = Arc::new(AtomicU16::new(200));
    let cache = Arc::new(BrowserEvidenceCache::default());
    let app = repository_recovery_app_with_cache(status.clone(), cache.clone()).await;
    let login_app = app.clone();
    let fixture = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let app = login_app.clone();
                async move {
                    let cookie = sign_in(&app).await;
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
                    let cache = cache.clone();
                    async move {
                        status.store(value, Ordering::SeqCst);
                        let expired = cache.expire();
                        axum::Json(serde_json::json!({"expired_evidence":expired}))
                    }
                },
            ),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    crate::common::browser_fixture::serve("127.0.0.1:4175", fixture).await;
}
