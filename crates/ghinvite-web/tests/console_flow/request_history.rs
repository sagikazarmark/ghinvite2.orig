use super::*;
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{DeliveryStorage, RecordStorage};

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
    let (app, cookie, storage, envelope) = super::requester_profile::browser_fixture().await;
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
    crate::common::browser_fixture::serve("127.0.0.1:4178", fixture).await;
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
    link.targets.repos = vec![ghinvite_core::targets::RepoTarget {
        grant: ghinvite_core::targets::RepoRole::Read,
        repo_id: 10,
        repo_full_name: "acme/api".into(),
    }];
    storage.seed_link(&link).await.unwrap();
    let history = format!("/console/accounts/acme/links/{}/requests", link.id);
    let html = response_html(identity_request(&app, &cookie, "GET", &history).await).await;
    assert!(html.contains("No projected requests in this range"));
    let request = ghinvite_core::InvitationRequest {
        batch: None,
        domain_evidence: None,
        id: ghinvite_core::RequestId::new(),
        invitation_link_id: Some(link.id),
        requester_id: 42,
        justification: Some("private justification".into()),
        state: ghinvite_core::RequestState::Pending,
        decided_by: None,
        decided_at: None,
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
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
    template.cancellation_reason = Some(ghinvite_core::RequestCancellationReason::LinkRevoked);
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
        .seed_console_installation(&ghinvite_core::Account {
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
    foreign.description = "Foreign private link".into();
    storage.seed_link(&foreign).await.unwrap();
    template.id = ghinvite_core::RequestId::new();
    template.invitation_link_id = Some(foreign.id);
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
    for (outcome, label) in [
        (
            CreateOutcome::Throttled {
                reason: None,
                until: chrono::Utc::now() + chrono::Duration::minutes(1),
            },
            "Throttled — waiting for GitHub’s rate limit",
        ),
        (
            CreateOutcome::Throttled {
                until: chrono::Utc::now() + chrono::Duration::minutes(1),
                reason: Some(ghinvite_core::budget::RetryReason::LinkShare),
            },
            "Waiting — this link has used its share of today's invitations",
        ),
        (
            CreateOutcome::Blocked {
                reason: ghinvite_core::delivery::BlockedReason::InstallationUnavailable,
            },
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
        (
            CreateOutcome::AlreadySatisfied {
                effective_role: ghinvite_core::targets::EffectiveRole::Read,
            },
            "Already a collaborator",
        ),
        (
            CreateOutcome::Failed {
                reason: ghinvite_core::delivery::FailureReason::Rejected { status: 422 },
            },
            "GitHub rejected delivery",
        ),
    ] {
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
        assert!(html.contains("Current delivery status unavailable"));
        let receipt = CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::DeliveryAuditVersion::Legacy,
            command: CreateCommand {
                batch: None,
                budget_attribution: Default::default(),
                access_lifetime_seconds: None,
                budget_share_pct: 50,
                delivery_generation: 1,
                invitation_id: Some(ghinvite_core::GithubInvitationId::new()),
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
            confirmed_at: outcome
                .confirmed()
                .then(|| "2026-01-02T12:01:00Z".parse().unwrap()),
            outcome,
            revision: 1,
        };
        storage
            .project_delivery(&receipt.clone().into())
            .await
            .unwrap();
        let html = response_html(identity_request(&app, &cookie, "GET", &path).await).await;
        assert!(
            html.contains(label) || html.contains(&label.replace('\'', "&#39;")),
            "missing {label}: {html}"
        );
        if matches!(receipt.outcome, CreateOutcome::Created { .. }) {
            assert!(html.contains("Current delivery status unavailable"));
        }
        assert!(!html.contains("private infrastructure diagnostic"));
    }
}

#[tokio::test]
async fn missing_or_misbound_owner_cannot_make_console_history_current() {
    use ghinvite_core::delivery::{
        CreateCommand, CreateOutcome, CreateReceipt, DispatchStage, RepositoryProgress,
    };
    for case in [
        "missing",
        "request",
        "account",
        "requester",
        "grant",
        "generation",
        "valid",
    ] {
        let (app, cookie, storage, envelope, authority) =
            deadline_queue_app_with_authority(None).await;
        let original = &envelope.requests[0];
        let at: DateTime<Utc> = "2026-01-02T12:00:00Z".parse().unwrap();
        let mut decided = storage
            .get_invitation_request(original.request_id)
            .await
            .unwrap()
            .unwrap();
        decided.state = ghinvite_core::RequestState::Approved;
        decided.decided_at = Some(at);
        decided.decided_by = Some(42);
        storage.seed_decision(&decided).await.unwrap();
        let mut owner_request = original.clone();
        owner_request.state = ghinvite_core::RequestState::Approved;
        authority.seed_request(owner_request);
        authority.set_delivery_progress(
            original.request_id,
            vec![RepositoryProgress {
                delivery_generation: 1,
                repo_id: 10,
                stage: DispatchStage::Submitted,
            }],
        );
        let snapshot: ghinvite_core::delivery::DeliverySnapshot = CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV2,
            command: CreateCommand {
                budget_attribution: Default::default(),
                access_lifetime_seconds: None,
                budget_share_pct: 50,
                invitation_id: None,
                delivery_generation: 1,
                link_id: original.link_id,
                batch: None,
                request_id: original.request_id,
                approval_id: "private approval binding".into(),
                account_id: 42,
                installation_id: 77,
                requester_id: 99,
                repo_id: 10,
                repo_full_name: "octocat/api".into(),
                permission: ghinvite_core::Permission::Pull,
                approved_at: at,
            },
            outcome: CreateOutcome::Created { upstream_id: 12345 },
            revision: 1,
            confirmed_at: Some(at),
        }
        .into();
        storage.project_delivery(&snapshot).await.unwrap();
        let key = snapshot.create.command.delivery_key();
        let mut current = common::authority_http_fixture::accepted_repository_snapshot(snapshot);
        match case {
            "request" => current.create.command.request_id = ghinvite_core::RequestId::new(),
            "account" => current.create.command.account_id += 1,
            "requester" => current.create.command.requester_id += 1,
            "grant" => current.create.command.permission = ghinvite_core::Permission::Push,
            "generation" => current.create.command.delivery_generation += 1,
            "missing" | "valid" => (),
            _ => unreachable!(),
        }
        authority.set_repository_delivery(key, (case != "missing").then_some(current));
        let path = format!("/console/accounts/octocat/requests/{}", original.request_id);
        let page = response_html(identity_request(&app, &cookie, "GET", &path).await).await;
        if case == "valid" {
            assert!(
                page.contains("Repository access accepted"),
                "{case}: {page}"
            );
            assert!(
                !page.contains("Current delivery status unavailable"),
                "{page}"
            );
        } else {
            assert!(page.contains("GitHub invitation created"), "{case}: {page}");
            assert!(
                page.contains("Current delivery status unavailable"),
                "{case}: {page}"
            );
            assert!(
                !page.contains("Repository access accepted"),
                "{case}: {page}"
            );
        }
        assert!(!page.contains("private approval binding"));
        assert!(
            authority
                .calls()
                .iter()
                .all(|method| ["delivery_progress", "status"].contains(&method.as_str()))
        );
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

#[tokio::test]
async fn cancelled_request_history_preserves_reason_and_exposes_no_decision_form() {
    let (app, cookie, storage, envelope) = deadline_queue_app(None).await;
    let id = envelope.requests[0].request_id;
    let mut request = storage.get_invitation_request(id).await.unwrap().unwrap();
    request.state = ghinvite_core::RequestState::Cancelled;
    request.decided_by = None;
    request.decided_at = Some("2026-01-02T12:00:00Z".parse().unwrap());
    request.cancellation_reason = Some(ghinvite_core::RequestCancellationReason::LinkRevoked);
    storage.seed_decision(&request).await.unwrap();
    let response = identity_request(
        &app,
        &cookie,
        "GET",
        &format!("/console/accounts/octocat/requests/{id}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let html = response_html(response).await;
    assert!(html.contains("This request was cancelled because its invitation link was revoked."));
    assert!(!html.contains("Decline reason"));
    assert!(!html.contains(&format!("requests/{id}/approve")));
    assert!(!html.contains(&format!("requests/{id}/decline")));
}
