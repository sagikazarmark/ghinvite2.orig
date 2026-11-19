use super::*;
use common::authority_http_fixture::FakeLinkAuthority;
use ghinvite_core::delivery::DeliverySnapshot;
use ghinvite_core::delivery::{CreateCommand, CreateOutcome, CreateReceipt};
use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
use ghinvite_core::request_lifecycle::RequestSnapshot;
use ghinvite_core::request_lifecycle::TerminalDecision;
use ghinvite_core::storage::projection::fixture::Seed;
use ghinvite_core::storage::{
    AppInstallationStorage, ConsoleStorage, DeliveryStorage, RecordStorage,
};
use ghinvite_core::{GithubInvitationId, InvitationState};
use ghinvite_storage_sqlx::SqlxStorage;
mod request_status;

struct DeliveryFixture {
    app: axum::Router,
    storage: Arc<SqlxStorage>,
    authority: FakeLinkAuthority,
    deliveries: Vec<DeliverySnapshot>,
}

async fn fixture() -> DeliveryFixture {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    fixture_with_storage(storage).await
}

async fn fixture_with_storage(storage: Arc<SqlxStorage>) -> DeliveryFixture {
    fixture_with_clocks(storage, false).await
}

async fn fixture_with_clocks(storage: Arc<SqlxStorage>, clocks: bool) -> DeliveryFixture {
    storage
        .put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account()).clone(),
        ))
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
    if clocks {
        link.invitation_lifetime_seconds = 3600;
    }
    link.internal_note = Some("private admin note".into());
    link.targets.repos = [
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
    .map(|(i, name)| ghinvite_core::targets::RepoTarget {
        grant: ghinvite_core::targets::RepoRole::Read,
        repo_id: i as u64 + 1,
        repo_full_name: format!("acme/{name}"),
    })
    .collect();
    storage.seed_link(&link).await.unwrap();
    common::capability::register(storage.as_ref(), &link).await;
    let mut request = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Approved,
    );
    request.justification = Some("private internal justification".into());
    storage.seed_request(&request).await.unwrap();
    let mut deliveries = vec![];
    for (i, repo) in link
        .targets
        .repos
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < 9 || *i == 13)
    {
        let outcome = match i {
            0 if clocks => CreateOutcome::AlreadyPending {
                upstream_id: 100,
                permission: ghinvite_core::targets::RepoRole::Read,
            },
            0..=4 => CreateOutcome::Created {
                upstream_id: 100 + i as u64,
            },
            5 => CreateOutcome::AlreadySatisfied {
                effective_role: ghinvite_core::targets::EffectiveRole::Read,
            },
            6 => CreateOutcome::Blocked {
                reason: ghinvite_core::delivery::BlockedReason::RequesterIdentity,
            },
            7 => CreateOutcome::OutcomeUnknown,
            13 => CreateOutcome::Throttled {
                reason: None,
                until: chrono::Utc::now() + chrono::Duration::minutes(1),
            },
            _ => CreateOutcome::Failed {
                reason: ghinvite_core::delivery::FailureReason::Rejected { status: 422 },
            },
        };
        let mut snapshot: DeliverySnapshot = CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::DeliveryAuditVersion::Legacy,
            command: CreateCommand {
                batch: None,
                budget_attribution: Default::default(),
                access_lifetime_seconds: None,
                budget_share_pct: 50,
                delivery_generation: 1,
                invitation_id: Some(GithubInvitationId::new()),
                link_id: Some(link.id),
                request_id: request.id,
                approval_id: "private approval identity".into(),
                account_id: link.account_id,
                installation_id: link.installation_id,
                requester_id: REQUESTER_ID,
                repo_id: repo.repo_id,
                repo_full_name: repo.repo_full_name.clone(),
                permission: repo.grant.into(),
                approved_at: Utc::now(),
            },
            confirmed_at: outcome.confirmed().then(Utc::now),
            outcome,
            revision: 1,
        }
        .into();
        if clocks && matches!(i, 0 | 4) {
            let approved = snapshot.create.command.approved_at;
            let created = if i == 0 {
                approved - chrono::Duration::days(2)
            } else {
                approved
            };
            assert!(
                snapshot.retain_invitation_timing(ghinvite_core::delivery::InvitationTiming {
                    upstream_id: 100 + i as u64,
                    created_at: created,
                    observed_at: approved + chrono::Duration::seconds(1),
                    owned_lifetime_seconds: Some(3600),
                })
            );
        }
        storage.project_delivery(&snapshot).await.unwrap();
        deliveries.push(snapshot);
    }
    // The authority approved the request; delivery has reached different
    // stages per repository.
    let authority = FakeLinkAuthority::start().await;
    authority.seed_link(&link);
    let approved_at = Utc::now();
    authority.seed_request(RequestSnapshot {
        batch: None,
        domain_evidence: None,
        request_id: request.id,
        link_id: Some(link.id),
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
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
        }),
    });
    authority.set_delivery_progress(
        request.id,
        (1..=14)
            .filter(|repo_id| *repo_id != 13)
            .map(|repo_id| RepositoryProgress {
                delivery_generation: 1,
                repo_id,
                stage: match repo_id {
                    10 => DispatchStage::Approved,
                    11 => DispatchStage::Planned,
                    13 => DispatchStage::Approved,
                    _ => DispatchStage::Submitted,
                },
            })
            .collect(),
    );
    let state = crate::common::sign_in::app_state(
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
        deliveries,
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
    // An owner outage keeps the canonical projected history visible, with an
    // explicit warning that current observations could not be loaded.
    observed.authority.fail("status", 503);
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
    assert!(
        !html.contains("GitHub invitation created"),
        "no current generation is known while request authority is unavailable"
    );
    assert!(html.contains("Latest status updates are unavailable"));
    assert!(html.contains("Delivery status unavailable"));
    assert!(!html.contains("private upstream diagnostic"));
}

#[tokio::test]
async fn already_collaborator_is_read_from_the_canonical_create_outcome() {
    let fixture = fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let before = status_html(&fixture, &cookie).await;
    assert_eq!(before.matches("Already a collaborator").count(), 1);
    let missing = before
        .split("<h3")
        .find(|part| part.contains(">acme/missing</h3>"))
        .unwrap()
        .split("</li>")
        .next()
        .unwrap();
    assert!(missing.contains("Delivery status unavailable"));
    assert!(!missing.contains("Repository access accepted"));
    let mut snapshot = fixture.deliveries[0].clone();
    snapshot.create.command.invitation_id = Some(GithubInvitationId::new());
    snapshot.create.command.repo_id = 13;
    snapshot.create.command.repo_full_name = "acme/missing".into();
    snapshot.create.outcome = CreateOutcome::AlreadySatisfied {
        effective_role: ghinvite_core::targets::EffectiveRole::Read,
    };
    fixture.authority.set_delivery_progress(
        snapshot.create.command.request_id,
        (1..=14)
            .map(|repo_id| RepositoryProgress {
                delivery_generation: 1,
                repo_id,
                stage: DispatchStage::Submitted,
            })
            .collect(),
    );
    fixture.storage.project_delivery(&snapshot).await.unwrap();
    let html = status_html(&fixture, &cookie).await;
    assert!(!html.contains("Your GitHub invitation was accepted"));
    assert_eq!(html.matches("Already a collaborator").count(), 2);
}

async fn settle(fixture: &DeliveryFixture) {
    for (delivery, state) in fixture.deliveries.iter().zip([
        InvitationState::Accepted,
        InvitationState::Declined,
        InvitationState::Cancelled,
        InvitationState::Expired,
    ]) {
        let mut snapshot = fixture
            .storage
            .list_delivery_for_request(delivery.create.command.request_id)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.create.command.delivery_key() == delivery.create.command.delivery_key())
            .unwrap();
        snapshot.revision += 1;
        snapshot.settlement = Some(ghinvite_core::delivery::Settlement {
            state,
            event: ghinvite_core::audit::AuditEvent {
                id: ghinvite_core::AuditEventId::from_ulid(
                    delivery
                        .create
                        .command
                        .invitation_id
                        .expect("legacy fixture identity")
                        .as_ulid(),
                ),
                account_id: snapshot.create.command.account_id,
                occurred_at: Utc::now(),
                event_type: ghinvite_core::storage::settlement::event_type(state).unwrap(),
                actor_kind: ghinvite_core::audit::ActorKind::Github,
                actor_id: None,
                target_kind: ghinvite_core::audit::TargetKind::GithubInvitation,
                target_id: delivery
                    .create
                    .command
                    .invitation_id
                    .expect("legacy fixture identity")
                    .to_string(),
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
                .uri("/join".to_owned())
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
    let expiry_current = current.clone();
    let lapsed_current = current.clone();
    let notice_current = current.clone();
    let request_status_current = current.clone();
    let app = axum::Router::new()
        .route(
            "/fixture-expiry",
            axum::routing::get(move || {
                let current = expiry_current.clone();
                async move {
                    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
                    let fixture = fixture_with_clocks(storage, true).await;
                    let cookie = sign_in(&fixture.app).await;
                    *current.lock().await = Some(fixture);
                    (
                        [(
                            "set-cookie",
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to("/join"),
                    )
                }
            }),
        )
        .route(
            "/fixture-request-status",
            axum::routing::get(move || {
                let current = request_status_current.clone();
                async move {
                    let (fixture, cookie, request_id) = request_status::owned_fixture().await;
                    *current.lock().await = Some(fixture);
                    (
                        [(
                            "set-cookie",
                            format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                        )],
                        axum::response::Redirect::to(&format!("/requests/{request_id}")),
                    )
                }
            }),
        )
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
                        axum::response::Redirect::to("/join"),
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
        .route(
            "/fixture-lapsed/{mode}",
            axum::routing::get(
                move |axum::extract::Path(mode): axum::extract::Path<String>| {
                    let current = lapsed_current.clone();
                    async move {
                        let fixture = fixture().await;
                        let cookie = sign_in(&fixture.app).await;
                        lapsed::browser_page(&fixture, mode == "limited");
                        *current.lock().await = Some(fixture);
                        (
                            [(
                                "set-cookie",
                                format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                            )],
                            axum::response::Redirect::to("/join"),
                        )
                    }
                },
            ),
        )
        .route(
            "/fixture-two-factor/{requirement}",
            axum::routing::get(
                move |axum::extract::Path(requirement): axum::extract::Path<String>| {
                    let current = notice_current.clone();
                    async move {
                        let requirement = match requirement.as_str() {
                            "required" => Some(true),
                            "not-required" => Some(false),
                            _ => None,
                        };
                        let fixture = organization::browser_notice(requirement).await;
                        let cookie = sign_in(&fixture.app).await;
                        *current.lock().await = Some(fixture);
                        (
                            [(
                                "set-cookie",
                                format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                            )],
                            axum::response::Redirect::to("/join"),
                        )
                    }
                },
            ),
        )
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .fallback(move |request: Request<Body>| {
            let current = current.clone();
            async move {
                let guard = current.lock().await;
                let fixture = guard.as_ref().unwrap();
                if request.method() == axum::http::Method::POST && request.uri().path() == "/join" {
                    fixture
                        .authority
                        .clear_requester_page(ACTIVE_SLUG.parse().unwrap());
                }
                let app = fixture.app.clone();
                drop(guard);
                app.oneshot(request).await.unwrap()
            }
        });
    crate::common::browser_fixture::serve("127.0.0.1:4175", app).await;
}

mod organization;

mod lapsed;

mod domains;

#[tokio::test]
async fn missing_or_misbound_current_owner_keeps_projection_explicitly_historical() {
    for case in [
        "missing",
        "request",
        "account",
        "requester",
        "grant",
        "generation",
        "valid",
    ] {
        let fixture = fixture().await;
        let projected = &fixture.deliveries[0];
        let key = projected.create.command.delivery_key();
        let mut current =
            common::authority_http_fixture::accepted_repository_snapshot(projected.clone());
        match case {
            "request" => current.create.command.request_id = RequestId::new(),
            "account" => current.create.command.account_id += 1,
            "requester" => current.create.command.requester_id += 1,
            "grant" => current.create.command.permission = ghinvite_core::Permission::Push,
            "generation" => current.create.command.delivery_generation += 1,
            "missing" | "valid" => (),
            _ => unreachable!(),
        }
        fixture
            .authority
            .set_repository_delivery(key, (case != "missing").then_some(current));
        let cookie = sign_in(&fixture.app).await;
        let page = status_html(&fixture, &cookie).await;
        let row = page
            .split("<h3")
            .find(|part| part.contains(">acme/accept</h3>"))
            .unwrap()
            .split("</li>")
            .next()
            .unwrap();
        if case == "valid" {
            assert!(row.contains("Repository access accepted"), "{case}: {row}");
            assert!(
                !row.contains("Latest status updates are unavailable"),
                "{row}"
            );
            assert!(!row.contains("GitHub invitation created"), "{row}");
        } else {
            assert!(row.contains("GitHub invitation created"), "{case}: {row}");
            assert!(
                row.contains("Latest status updates are unavailable"),
                "{case}: {row}"
            );
            assert!(!row.contains("Repository access accepted"), "{case}: {row}");
        }
        assert!(!page.contains("private approval identity"));
        assert!(fixture.authority.calls().iter().all(|method| {
            ["requester_page", "delivery_progress", "status"].contains(&method.as_str())
        }));
    }
}

#[tokio::test]
async fn requester_uses_retained_current_generation_without_falling_back_to_old_projection() {
    let fixture = fixture().await;
    let request = fixture.deliveries[0].create.command.request_id;
    let old = fixture
        .storage
        .list_delivery_for_request(request)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.create.command.repo_id == 1)
        .unwrap();
    fixture.authority.set_delivery_progress(
        request,
        (1..=14)
            .map(|repo_id| RepositoryProgress {
                repo_id,
                delivery_generation: if repo_id == 1 { 2 } else { 1 },
                stage: DispatchStage::Submitted,
            })
            .collect(),
    );
    let cookie = sign_in(&fixture.app).await;
    let html = status_html(&fixture, &cookie).await;
    // Other targets retain their generation1 results; this target must not
    // borrow its old owned invitation while generation2 is unprojected.
    let row = html
        .split("<h3")
        .find(|part| part.contains(">acme/accept</h3>"))
        .unwrap()
        .split("</li>")
        .next()
        .unwrap();
    assert!(!row.contains("GitHub invitation created"));
    let mut current = old;
    current.create.command.delivery_generation = 2;
    current.create.command.invitation_id = Some(GithubInvitationId::new());
    current.create.command.budget_attribution =
        ghinvite_core::reissue::BudgetAttribution::AdministratorReissue;
    current.create.outcome = CreateOutcome::OutcomeUnknown;
    current.create.confirmed_at = None;
    fixture.storage.project_delivery(&current).await.unwrap();
    let html = status_html(&fixture, &cookie).await;
    let row = html
        .split("<h3")
        .find(|part| part.contains(">acme/accept</h3>"))
        .unwrap()
        .split("</li>")
        .next()
        .unwrap();
    assert!(row.contains("GitHub outcome unknown"));
    assert!(!row.contains("GitHub invitation created"));
}
