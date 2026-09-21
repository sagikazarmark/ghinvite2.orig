use super::*;
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{DeliveryStorage, InstallationStorage, RecordStorage};

#[tokio::test]
async fn history_and_detail_require_current_account_admin_authority() {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "member").await;
    let link = list_link(1);
    storage.seed_link(&link).await.unwrap();
    for path in [
        format!("/console/accounts/acme/links/{}/requests", link.id),
        format!(
            "/console/accounts/acme/requests/{}",
            ghinvite_core::RequestId::new()
        ),
    ] {
        let response = identity_request(&app, &cookie, "GET", &path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response_html(response).await.contains("Workshop"));
        let response = identity_request(&app, "", "GET", &path).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            response.headers()["location"]
                .to_str()
                .unwrap()
                .contains("/login")
        );
    }
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn request_history_browser_server() {
    use axum::response::IntoResponse;
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    let mut request = storage
        .get_invitation_request(envelope.requests[0].request_id)
        .await
        .unwrap()
        .unwrap();
    request.state = ghinvite_core::RequestState::Declined;
    request.decided_by = Some(42);
    request.decided_at = Some("2026-01-02T12:00:00Z".parse().unwrap());
    request.decline_reason = Some("Browser decision".into());
    for _ in 0..27 {
        request.id = ghinvite_core::RequestId::new();
        storage.seed_request(&request).await.unwrap();
    }
    let destination = format!("/console/accounts/octocat/links/{}", envelope.link.link_id);
    let fixture = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let cookie = cookie.clone();
                let destination = destination.clone();
                async move {
                    (
                        [(
                            axum::http::header::SET_COOKIE,
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to(&destination),
                    )
                        .into_response()
                }
            }),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4178")
        .await
        .unwrap();
    axum::serve(listener, fixture).await.unwrap();
}

#[tokio::test]
async fn request_history_handles_empty_missing_profiles_and_unavailable_projections() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-history-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap(),
    );
    let (app, cookie) = links_app(storage.clone(), "admin").await;
    let mut link = list_link(1);
    link.repos = vec![ghinvite_core::InvitationLinkRepo {
        repo_id: 10,
        repo_full_name: "acme/api".into(),
    }];
    storage.seed_link(&link).await.unwrap();
    let history = format!("/console/accounts/acme/links/{}/requests", link.id);
    let html = response_html(identity_request(&app, &cookie, "GET", &history).await).await;
    assert!(html.contains("No projected requests in this range"));
    let request = ghinvite_core::InvitationRequest {
        id: ghinvite_core::RequestId::new(),
        invitation_link_id: link.id,
        requester_id: 42,
        justification: Some("private justification".into()),
        state: ghinvite_core::RequestState::Pending,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        decision_deadline: Some("2026-01-03T12:34:56Z".parse().unwrap()),
        created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
    };
    storage.seed_request(&request).await.unwrap();
    let detail = format!("/console/accounts/acme/requests/{}", request.id);
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    // A missing optional profile must preserve the immutable identity.
    sqlx::query("UPDATE users SET last_seen_at = 'broken'")
        .execute(&pool)
        .await
        .unwrap();
    let html = response_html(identity_request(&app, &cookie, "GET", &detail).await).await;
    assert!(html.contains("GitHub user ID 42 · Profile unavailable"));
    assert!(html.contains("Decision deadline passed. The projected state may be delayed"));
    sqlx::query("DROP TABLE delivery_outcomes")
        .execute(&pool)
        .await
        .unwrap();
    let html = response_html(identity_request(&app, &cookie, "GET", &detail).await).await;
    assert!(html.contains("Delivery information unavailable"));
    assert!(html.contains("Current delivery status unavailable"));
    assert!(!html.contains("No projected delivery outcome."));
    assert!(html.contains("private justification"));
    sqlx::query("UPDATE invitation_requests SET state = 'corrupt'")
        .execute(&pool)
        .await
        .unwrap();
    let response = identity_request(&app, &cookie, "GET", &history).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let html = response_html(response).await;
    assert!(html.contains("Request history unavailable"));
    assert!(!html.contains("No projected requests"));
    sqlx::query("UPDATE invitation_requests SET state = 'pending'")
        .execute(&pool)
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM invitation_links")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let response = identity_request(&app, &cookie, "GET", &detail).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        !response_html(response)
            .await
            .contains("private justification")
    );
    pool.close().await;
    drop(app);
    drop(storage);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn request_history_pages_and_terminal_details_conceal_foreign_resources() {
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    let link = envelope.link.link_id;
    let mut template = storage
        .get_invitation_request(envelope.requests[0].request_id)
        .await
        .unwrap()
        .unwrap();
    template.state = ghinvite_core::RequestState::Cancelled;
    for _ in 0..27 {
        template.id = ghinvite_core::RequestId::new();
        storage.seed_request(&template).await.unwrap();
    }
    let base = format!("/console/accounts/octocat/links/{link}/requests");
    let html = response_html(identity_request(&app, &cookie, "GET", &base).await).await;
    assert_eq!(html.matches("State:").count(), 25);
    let older = html
        .split("href=\"")
        .filter_map(|s| s.split('"').next())
        .find(|s| s.contains("?before="))
        .unwrap();
    let page = response_html(identity_request(&app, &cookie, "GET", older).await).await;
    assert_eq!(page.matches("State:").count(), 3);
    assert!(page.contains("Latest requests"));
    assert!(!page.contains("Older requests"));
    let malformed =
        response_html(identity_request(&app, &cookie, "GET", &format!("{base}?before=bad")).await)
            .await;
    assert_eq!(malformed.matches("State:").count(), 25);
    let mut foreign = storage
        .get_invitation_link_by_id(link)
        .await
        .unwrap()
        .unwrap();
    foreign.id = ghinvite_core::InvitationLinkId::new();
    foreign.account_id = 999;
    foreign.installation_id = 999;
    storage
        .insert_installation(&ghinvite_core::Account {
            installation_id: 999,
            account_id: 999,
            account_login: "foreign".into(),
            account_type: ghinvite_core::AccountType::Organization,
            installed_at: foreign.created_at,
            uninstalled_at: None,
            selected_repos: ghinvite_core::SelectedRepos::All,
        })
        .await
        .unwrap();
    foreign.slug = ghinvite_core::Slug::from_string("ForeignHistory01".into()).unwrap();
    foreign.description = "Foreign private link".into();
    storage.seed_link(&foreign).await.unwrap();
    template.id = ghinvite_core::RequestId::new();
    template.invitation_link_id = foreign.id;
    template.justification = Some("Foreign private justification".into());
    storage.seed_request(&template).await.unwrap();
    for path in [
        format!("/console/accounts/octocat/links/{}/requests", foreign.id),
        format!("/console/accounts/octocat/requests/{}", template.id),
    ] {
        let response = identity_request(&app, &cookie, "GET", &path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let html = response_html(response).await;
        assert!(!html.contains("Foreign private"));
        assert!(!html.contains("octocat/api"));
    }
}

#[tokio::test]
async fn approved_request_distinguishes_projected_outcomes_from_missing_delivery() {
    use ghinvite_core::delivery::{CreateCommand, CreateOutcome, CreateReceipt};
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    let request = &envelope.requests[0];
    {
        let mut decided = storage
            .get_invitation_request(request.request_id)
            .await
            .unwrap()
            .unwrap();
        decided.state = ghinvite_core::RequestState::Approved;
        decided.decided_by = Some(42);
        decided.decided_at = Some("2026-01-02T12:00:00Z".parse().unwrap());
        decided.decline_reason = None;
        storage.seed_decision(&decided).await.unwrap();
    }
    let path = format!("/console/accounts/octocat/requests/{}", request.request_id);
    let html = response_html(identity_request(&app, &cookie, "GET", &path).await).await;
    assert!(html.contains("No projected delivery outcome yet"));
    let mut receipt = CreateReceipt {
        command: CreateCommand {
            invitation_id: ghinvite_core::GithubInvitationId::new(),
            link_id: request.link_id,
            request_id: request.request_id,
            approval_id: "approval".into(),
            account_id: 42,
            installation_id: 77,
            requester_id: 99,
            repo_id: 10,
            repo_full_name: "octocat/api".into(),
            permission: ghinvite_core::Permission::Pull,
            approved_at: "2026-01-02T12:00:00Z".parse().unwrap(),
        },
        outcome: CreateOutcome::Blocked {
            reason: "private infrastructure diagnostic".into(),
        },
        revision: 1,
        confirmed_at: None,
    };
    for (outcome, label) in [
        (
            receipt.outcome.clone(),
            "Blocked — waiting for availability or identity verification",
        ),
        (
            CreateOutcome::OutcomeUnknown,
            "GitHub outcome unknown — awaiting reconciliation",
        ),
        (
            CreateOutcome::Created { upstream_id: 12345 },
            "GitHub invitation created — awaiting acceptance",
        ),
        (CreateOutcome::AlreadyCollaborator, "Already a collaborator"),
        (
            CreateOutcome::Failed { status: 422 },
            "GitHub rejected delivery",
        ),
    ] {
        receipt.outcome = outcome;
        receipt.revision += 1;
        storage.project_delivery(&receipt).await.unwrap();
        let html = response_html(identity_request(&app, &cookie, "GET", &path).await).await;
        assert!(html.contains(label), "missing {label}");
        if matches!(receipt.outcome, CreateOutcome::Created { .. }) {
            assert!(html.contains("Current delivery status unavailable"));
            assert!(html.contains("retained history"));
        }
        assert!(!html.contains("private infrastructure diagnostic"));
    }
}

#[tokio::test]
async fn terminal_request_is_reachable_from_link_history_with_immutable_scope_and_decision() {
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    let id = envelope.requests[0].request_id;
    let link = envelope.link.link_id;
    {
        let mut decided = storage.get_invitation_request(id).await.unwrap().unwrap();
        decided.state = ghinvite_core::RequestState::Declined;
        decided.decided_by = Some(42);
        decided.decided_at = Some("2026-01-02T12:00:00Z".parse().unwrap());
        decided.decline_reason = Some("Admin-only decision context".into());
        storage.seed_decision(&decided).await.unwrap();
    }
    let base = "/console/accounts/octocat";
    let html = response_html(
        identity_request(&app, &cookie, "GET", &format!("{base}/links/{link}")).await,
    )
    .await;
    assert!(html.contains(&format!("{base}/links/{link}/requests")));
    let response = identity_request(
        &app,
        &cookie,
        "GET",
        &format!("{base}/links/{link}/requests"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(html.contains("Request history"));
    assert!(html.contains("@requester"));
    assert!(html.contains("declined"));
    assert!(html.contains(&format!("{base}/requests/{id}")));
    let response = identity_request(&app, &cookie, "GET", &format!("{base}/requests/{id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let html = response_html(response).await;
    for expected in [
        "Invitation request",
        "declined",
        "octocat/api",
        "pull",
        "Admin-only decision context",
        "2026-01-02 12:00:00 UTC",
        "GitHub user ID 42",
        "Projected history",
        "Decision deadline",
    ] {
        assert!(html.contains(expected), "missing {expected}: {html}");
    }
}
