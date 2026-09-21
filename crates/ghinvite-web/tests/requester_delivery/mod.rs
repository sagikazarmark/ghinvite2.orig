use super::*;
use ghinvite_core::delivery::{CreateCommand, CreateOutcome, CreateReceipt};
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{DeliveryStorage, InstallationStorage, RecordStorage};
use ghinvite_core::{GithubInvitation, GithubInvitationId, InvitationState};
use ghinvite_storage_sqlx::SqlxStorage;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path_regex};

struct DeliveryFixture {
    app: axum::Router,
    storage: Arc<SqlxStorage>,
    ingress: MockServer,
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
    for (i, repo) in link.repos.iter().take(9).enumerate() {
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
            _ => CreateOutcome::Failed { status: 422 },
        };
        storage
            .project_delivery(&CreateReceipt {
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
            })
            .await
            .unwrap();
        invitations.push(invitation);
    }
    let ingress = MockServer::start().await;
    Mock::given(path_regex("/resolve$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(link.id))
        .mount(&ingress)
        .await;
    Mock::given(path_regex("/requester_page$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "link_id": link.id, "invitation_code": ACTIVE_SLUG, "repos": link.repos,
            "permission": "pull", "approval_required": true, "can_start_fresh": false, "attempt": null,
            "request": { "request_id": request.id, "link_id": link.id, "account_id": link.account_id,
                "requester_id": REQUESTER_ID, "justification": "private internal justification",
                "state": "approved", "admitted_at": "2026-09-14T12:00:00Z",
                "decision_deadline": "2026-09-21T12:00:00Z", "revision": 1 }
        }))).mount(&ingress).await;
    Mock::given(path_regex("/delivery_progress$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"repo_id": 10, "stage": "approved"}, {"repo_id": 11, "stage": "planned"},
            {"repo_id": 12, "stage": "submitted"}
        ])))
        .mount(&ingress)
        .await;
    let state = AppState::new(
        storage.clone(),
        Arc::new(MockTransport::scripted(oauth_expectations(
            "octocat",
            REQUESTER_ID,
        ))),
        Arc::new(UnusedCommands),
        Arc::new(ghinvite_web::RestateClient::new(ingress.uri()).unwrap()),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    DeliveryFixture {
        app: build_app(state, tower_sessions::MemoryStore::default()),
        storage,
        ingress,
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
    let cookie = sign_in(observed.app.clone()).await;
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
    Mock::given(path_regex("/delivery_progress$"))
        .respond_with(ResponseTemplate::new(503).set_body_string("private upstream diagnostic"))
        .with_priority(1)
        .mount(&fixture.ingress)
        .await;
    let cookie = sign_in(fixture.app.clone()).await;
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
    let cookie = sign_in(fixture.app.clone()).await;
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
        fixture
            .storage
            .debug_set_github_invitation(invitation.id, state, invitation.github_invitation_id)
            .await
            .unwrap();
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
    let cookie = sign_in(fixture.app.clone()).await;
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
    assert!(
        fixture
            .ingress
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| ["resolve", "requester_page", "delivery_progress"]
                .contains(&r.url.path().rsplit('/').next().unwrap()))
    );
}

#[tokio::test]
async fn mixed_delivery_gives_outcome_specific_next_steps_without_claiming_missing_delivery_failed()
{
    let fixture = fixture().await;
    let cookie = sign_in(fixture.app.clone()).await;
    let html = status_html(&fixture, &cookie).await;
    for copy in [
        "GitHub invitation created — awaiting acceptance",
        "Already a collaborator",
        "Blocked — waiting for availability or identity verification",
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
                    let cookie = sign_in(fixture.app.clone()).await;
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
