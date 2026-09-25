use super::*;
use common::authority_http_fixture::FakeLinkAuthority;
use ghinvite_core::delivery::{CreateCommand, CreateOutcome, CreateReceipt};
use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
use ghinvite_core::request_lifecycle::RequestSnapshot;
use ghinvite_core::request_lifecycle::TerminalDecision;
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{ConsoleStorage, DeliveryStorage, InstallationStorage, RecordStorage};
use ghinvite_core::{GithubInvitation, GithubInvitationId, InvitationState};
use ghinvite_storage_sqlx::SqlxStorage;

struct DeliveryFixture {
    app: axum::Router,
    storage: Arc<SqlxStorage>,
    authority: FakeLinkAuthority,
    invitations: Vec<GithubInvitation>,
}

async fn fixture() -> DeliveryFixture {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    fixture_with_storage(storage).await
}

async fn fixture_with_storage(storage: Arc<SqlxStorage>) -> DeliveryFixture {
    storage
        .insert_installation(&sample_account())
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(CREATOR_ID, "creator"))
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(REQUESTER_ID, "octocat"))
        .await
        .unwrap();
    let mut link = active_link(ACTIVE_SLUG);
    link.internal_note = Some("private admin note".into());
    link.repos = [
        "accept",
        "decline",
        "cancel",
        "expire",
        "waiting",
        "collaborator",
        "blocked",
        "unknown",
        "failed",
        "approved",
        "planned",
        "submitted",
        "missing",
        "throttled",
    ]
    .iter()
    .enumerate()
    .map(|(i, name)| InvitationLinkRepo {
        repo_id: i as u64 + 1,
        repo_full_name: format!("acme/{name}"),
    })
    .collect();
    storage.seed_link(&link).await.unwrap();
    let mut request = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Approved,
    );
    request.justification = Some("private internal justification".into());
    storage.seed_request(&request).await.unwrap();
    let mut invitations = vec![];
    for (i, repo) in link
        .repos
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < 9 || *i == 13)
    {
        let invitation = GithubInvitation {
            id: GithubInvitationId::new(),
            invitation_request_id: request.id,
            repo_id: repo.repo_id,
            github_invitation_id: Some(100 + i as u64),
            state: InvitationState::Sent,
            error_message: Some("private GitHub diagnostic".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        if i < 5 {
            storage.insert_github_invitation(&invitation).await.unwrap();
        }
        let outcome = match i {
            0..=4 => CreateOutcome::Created {
                upstream_id: 100 + i as u64,
            },
            5 => CreateOutcome::AlreadyCollaborator,
            6 => CreateOutcome::Blocked {
                reason: "private credential diagnostic".into(),
            },
            7 => CreateOutcome::OutcomeUnknown,
            13 => CreateOutcome::Throttled,
            _ => CreateOutcome::Failed { status: 422 },
        };
        storage
            .project_delivery(
                &CreateReceipt {
                    command: CreateCommand {
                        invitation_id: invitation.id,
                        link_id: link.id,
                        request_id: request.id,
                        approval_id: "private approval identity".into(),
                        account_id: link.account_id,
                        installation_id: link.installation_id,
                        requester_id: REQUESTER_ID,
                        repo_id: repo.repo_id,
                        repo_full_name: repo.repo_full_name.clone(),
                        permission: link.permission,
                        approved_at: Utc::now(),
                    },
                    confirmed_at: outcome.confirmed().then(Utc::now),
                    outcome,
                    revision: 1,
                }
                .into(),
            )
            .await
            .unwrap();
        invitations.push(invitation);
    }
    // The authority approved the request; delivery has reached different
    // stages per repository.
    let authority = FakeLinkAuthority::start().await;
    authority.seed_link(&link);
    let approved_at = Utc::now();
    authority.seed_request(RequestSnapshot {
        request_id: request.id,
        link_id: link.id,
        account_id: link.account_id,
        requester_id: REQUESTER_ID,
        justification: request.justification.clone(),
        state: RequestState::Approved,
        admitted_at: approved_at,
        decision_deadline: Some(approved_at + chrono::Duration::days(7)),
        revision: 2,
        decision: Some(TerminalDecision {
            decision_id: format!("request.approved/{}", request.id),
            decided_by: Some(CREATOR_ID),
            effective_at: approved_at,
            evaluated_at: approved_at,
            decline_reason: None,
        }),
    });
    authority.set_delivery_progress(
        request.id,
        [
            (10, DispatchStage::Approved),
            (11, DispatchStage::Planned),
            (12, DispatchStage::Submitted),
        ]
        .map(|(repo_id, stage)| RepositoryProgress { repo_id, stage })
        .into(),
    );
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(oauth_expectations(
            GithubUser::new("octocat", REQUESTER_ID),
        ))),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    DeliveryFixture {
        app: build_app(state, tower_sessions::MemoryStore::default()),
        storage,
        authority,
        invitations,
    }
}

#[tokio::test]
async fn failed_observations_preserve_known_outcomes_and_mark_updates_unavailable() {
    let path = std::env::temp_dir().join(format!("requester-delivery-{}.sqlite", RequestId::new()));
    let storage = Arc::new(SqlxStorage::at_path(&path).await.unwrap());
    let pool =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    let observed = fixture_with_storage(storage).await;
    let cookie = sign_in(&observed.app).await;
    // A database read failure is distinct from an empty/missing projection.
    sqlx::query("DROP TABLE github_invitations")
        .execute(&pool)
        .await
        .unwrap();
    let html = status_html(&observed, &cookie).await;
    assert!(html.contains("GitHub invitation created"));
    assert!(html.contains("Latest status updates are unavailable"));
    assert!(!html.contains("no such table"));
    drop(observed);
    pool.close().await;
    std::fs::remove_file(path).unwrap();

    let fixture = fixture().await;
    fixture.authority.fail("delivery_progress", 503);
    let cookie = sign_in(&fixture.app).await;
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("GitHub invitation created"));
    assert!(html.contains("Latest status updates are unavailable"));
    assert!(html.contains("Delivery status unavailable"));
    assert!(!html.contains("private upstream diagnostic"));
}

#[tokio::test]
async fn receipt_free_already_collaborator_is_not_reported_as_invitation_acceptance() {
    let fixture = fixture().await;
    let mut invitation = fixture.invitations[0].clone();
    invitation.id = GithubInvitationId::new();
    invitation.repo_id = 13; // Repository with no retained create receipt.
    invitation.github_invitation_id = None;
    invitation.state = InvitationState::Accepted;
    fixture
        .storage
        .insert_github_invitation(&invitation)
        .await
        .unwrap();
    let cookie = sign_in(&fixture.app).await;
    let html = status_html(&fixture, &cookie).await;
    assert!(!html.contains("Your GitHub invitation was accepted"));
    assert_eq!(html.matches("Already a collaborator").count(), 2);
}

async fn settle(fixture: &DeliveryFixture) {
    for (invitation, state) in fixture.invitations.iter().zip([
        InvitationState::Accepted,
        InvitationState::Declined,
        InvitationState::Cancelled,
        InvitationState::Expired,
    ]) {
        let mut snapshot = fixture
            .storage
            .list_delivery_for_request(invitation.invitation_request_id)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.create.command.invitation_id == invitation.id)
            .unwrap();
        snapshot.revision += 1;
        snapshot.settlement = Some(ghinvite_core::delivery::Settlement {
            state,
            event: ghinvite_core::audit::AuditEvent {
                id: ghinvite_core::AuditEventId::from_ulid(invitation.id.as_ulid()),
                account_id: snapshot.create.command.account_id,
                occurred_at: Utc::now(),
                event_type: ghinvite_core::storage::settlement::event_type(state).unwrap(),
                actor_kind: ghinvite_core::audit::ActorKind::Github,
                actor_id: None,
                target_kind: ghinvite_core::audit::TargetKind::GithubInvitation,
                target_id: invitation.id.to_string(),
                metadata: serde_json::Value::Null,
                request_id: None,
            },
        });
        fixture.storage.project_delivery(&snapshot).await.unwrap();
    }
}

async fn status_html(fixture: &DeliveryFixture, cookie: &str) -> String {
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_text(response).await
}

#[tokio::test]
async fn later_lifecycle_supersedes_retained_create_receipts_on_requester_routes() {
    let fixture = fixture().await;
    let cookie = sign_in(&fixture.app).await;
    assert!(
        status_html(&fixture, &cookie)
            .await
            .contains("GitHub invitation created")
    );
    settle(&fixture).await;
    let html = status_html(&fixture, &cookie).await;
    for label in [
        "Repository access accepted",
        "GitHub invitation declined",
        "GitHub invitation cancelled",
        "GitHub invitation expired",
    ] {
        assert!(html.contains(label), "missing {label}");
    }
    assert!(!html.contains("private "));
    assert!(!html.contains("AI coding workshop"));
    assert!(!html.contains("Submit request"));
    assert!(fixture.authority.calls().iter().all(|method| {
        ["requester_page", "delivery_progress", "status"].contains(&method.as_str())
    }));
}

#[tokio::test]
async fn mixed_delivery_gives_outcome_specific_next_steps_without_claiming_missing_delivery_failed()
{
    let fixture = fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let html = status_html(&fixture, &cookie).await;
    for copy in [
        "GitHub invitation created — awaiting acceptance",
        "Already a collaborator",
        "Blocked — waiting for availability or identity verification",
        "Throttled — waiting for GitHub’s rate limit",
        "GitHub outcome unknown",
        "GitHub rejected delivery",
        "Approved — awaiting dispatch",
        "Planned — awaiting dispatch",
        "Submitted — awaiting GitHub confirmation",
        "Delivery status unavailable",
        "Accept on GitHub",
        "https://github.com/acme/waiting/invitations",
        "https://github.com/notifications",
        "signed in as @octocat",
        "Wait, then check again",
        "contact an account admin",
        "Missing status does not mean delivery failed",
    ] {
        assert!(html.contains(copy), "missing {copy}");
    }
    settle(&fixture).await;
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("Open repository"));
    assert!(html.contains("If you still need access, contact an account admin"));
    assert!(!html.contains("private "));
}

/// Serves the production requester router; only Restate and OAuth are stubbed.
#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn delivery_browser_server() {
    let current = Arc::new(tokio::sync::Mutex::new(None::<DeliveryFixture>));
    let login_current = current.clone();
    let settle_current = current.clone();
    let app = axum::Router::new()
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let current = login_current.clone();
                async move {
                    let fixture = fixture().await;
                    let cookie = sign_in(&fixture.app).await;
                    *current.lock().await = Some(fixture);
                    (
                        [(
                            "set-cookie",
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to(&format!("/i/{ACTIVE_SLUG}")),
                    )
                }
            }),
        )
        .route(
            "/fixture-settle",
            axum::routing::post(move || {
                let current = settle_current.clone();
                async move {
                    settle(current.lock().await.as_ref().unwrap()).await;
                    StatusCode::NO_CONTENT
                }
            }),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let current = current.clone();
            async move {
                let app = current.lock().await.as_ref().unwrap().app.clone();
                app.oneshot(request).await.unwrap()
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:4175")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
