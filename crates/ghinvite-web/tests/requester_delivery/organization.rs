use super::*;
use ghinvite_core::{
    account::{App, Installation},
    org_delivery::*,
    request_lifecycle::OrganizationProgress,
    storage::{AppInstallationStorage, org_delivery::OrgDeliveryStorage},
};

async fn org_fixture() -> (DeliveryFixture, OrganizationDeliverySnapshot) {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
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
    let org = Installation {
        app: App::Org,
        account: sample_account(),
        installed_by_user_id: Some(CREATOR_ID),
        installed_by_login: Some("creator".into()),
        suspended_at: None,
        permissions: Default::default(),
        revision: 1,
    };
    storage.put_app_installation(&org).await.unwrap();
    let mut link = active_link(ACTIVE_SLUG);
    link.targets = serde_json::from_value(serde_json::json!({"organization":{"org_id":9001,"org_login":"acme","grant":"member"},"teams":[{"team_id":7,"team_slug":"learners","grant":"member"}],"repos":[]})).unwrap();
    storage.seed_link(&link).await.unwrap();
    common::capability::register(storage.as_ref(), &link).await;
    let request = request_with_state(
        RequestId::new(),
        link.id,
        REQUESTER_ID,
        RequestState::Approved,
    );
    storage.seed_request(&request).await.unwrap();
    let authority = FakeLinkAuthority::start().await;
    authority.seed_link(&link);
    authority.seed_request(RequestSnapshot {
        batch: None,
        domain_evidence: None,
        request_id: request.id,
        link_id: Some(link.id),
        account_id: 9001,
        requester_id: REQUESTER_ID,
        justification: None,
        state: RequestState::Approved,
        admitted_at: request.created_at,
        decision_deadline: None,
        revision: 2,
        decision: Some(TerminalDecision {
            decision_id: "approval".into(),
            decided_by: Some(CREATOR_ID),
            effective_at: request.created_at,
            evaluated_at: request.created_at,
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
        }),
    });
    authority.set_organization_progress(
        request.id,
        OrganizationProgress {
            delivery_generation: 1,
            org_id: 9001,
            team_ids: vec![7],
            stage: DispatchStage::Submitted,
        },
    );
    let snapshot = OrganizationDeliverySnapshot::planned(OrganizationCreateCommand {
        batch: None,
        budget_attribution: Default::default(),
        access_lifetime_seconds: None,
        invitation_id: Some(GithubInvitationId::new()),
        link_id: Some(link.id),
        request_id: request.id,
        approval_id: "approval".into(),
        account_id: 9001,
        installation_id: 1,
        requester_id: REQUESTER_ID,
        organization: link.targets.organization.unwrap(),
        teams: link.targets.teams,
        delivery_generation: 1,
        approved_at: request.created_at,
        budget_share_pct: 50,
    })
    .unwrap();
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        Arc::new(MockTransport::scripted(oauth_expectations(
            GithubUser::new("octocat", REQUESTER_ID),
        ))),
        authority.client(),
        WebConfig::for_local_dev_with_secret([7; 32]),
    );
    (
        DeliveryFixture {
            app: build_app(state, tower_sessions::MemoryStore::default()),
            storage,
            authority,
            deliveries: vec![],
        },
        snapshot,
    )
}

#[tokio::test]
async fn requester_two_factor_notice_is_advisory_and_only_for_a_known_requirement() {
    let (fixture, snapshot) = org_fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let link = fixture
        .authority
        .link(snapshot.create.command.link_id)
        .unwrap();
    let mut page = ghinvite_core::admission::RequesterPage {
        two_factor_required: Some(true),
        email_domains: vec![],
        paused: false,
        rerequest_after: None,
        link_id: link.link_id,
        targets: link.creation.targets,
        approval_required: true,
        can_start_fresh: true,
        attempt: None,
        request: None,
    };
    let original = fixture.authority.requests();
    const NOTICE: &str =
        "This organization requires two-factor authentication; you'll need it to accept.";
    for requirement in [Some(true), Some(false), None] {
        page.two_factor_required = requirement;
        fixture.authority.set_requester_page(page.clone());
        let html = status_html(&fixture, &cookie).await;
        let text = html.replace("&#39;", "'").replace("&#x27;", "'");
        assert_eq!(text.contains(NOTICE), requirement == Some(true));
        assert!(
            html.contains("Submit request"),
            "warning leaves the ordinary request form available"
        );
        assert!(!html.contains("You have two-factor authentication enabled"));
        assert!(!html.contains("You must enable two-factor authentication before requesting"));
    }
    assert_eq!(fixture.authority.requests(), original);
    assert!(
        !fixture
            .authority
            .calls()
            .iter()
            .any(|method| method == "admit")
    );
}

pub(super) async fn browser_notice(requirement: Option<bool>) -> DeliveryFixture {
    let (fixture, snapshot) = org_fixture().await;
    let link = fixture
        .authority
        .link(snapshot.create.command.link_id)
        .unwrap();
    fixture
        .authority
        .set_requester_page(ghinvite_core::admission::RequesterPage {
            two_factor_required: requirement,
            email_domains: vec![],
            paused: false,
            rerequest_after: None,
            link_id: link.link_id,
            targets: link.creation.targets,
            approval_required: true,
            can_start_fresh: true,
            attempt: None,
            request: None,
        });
    fixture
}

#[tokio::test]
async fn requester_org_progress_distinguishes_lag_foreign_pending_team_pending_and_unknown() {
    let (fixture, mut snapshot) = org_fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("Delivery submitted. No GitHub outcome recorded yet."));
    assert!(!html.contains("Active organization membership was verified"));
    snapshot.create.outcome = OrganizationCreateOutcome::AlreadyPending {
        upstream_id: 9,
        role: OrganizationInvitationRole::Admin,
    };
    snapshot.create.revision = 2;
    snapshot.create.confirmed_at = Some(Utc::now());
    snapshot.revision = 2;
    let team = snapshot.teams.get_mut(&7).unwrap();
    team.create.outcome = TeamCreateOutcome::AlreadySatisfied {
        role: TeamRole::Maintainer,
        state: MembershipState::Pending,
    };
    team.create.confirmed_at = Some(Utc::now());
    team.create.revision = 2;
    team.revision = 2;
    fixture
        .storage
        .project_organization_delivery(&snapshot)
        .await
        .unwrap();
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("without claiming ownership"));
    assert!(html.contains("active access has not been verified"));
    assert!(!html.contains("Active team maintainer access was verified"));
    let mut second = snapshot.clone();
    second.create.command.delivery_generation = 2;
    second.create.command.invitation_id = Some(GithubInvitationId::new());
    second.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
    second.create.confirmed_at = None;
    fixture
        .storage
        .project_organization_delivery(&second)
        .await
        .unwrap();
    fixture
        .authority
        .fail("organization_delivery_progress", 503);
    let html = status_html(&fixture, &cookie).await;
    assert!(
        !html.contains("Delivery generation: 2"),
        "a projected maximum is not current-generation authority"
    );
    assert!(!html.contains("Outcome unknown"));
    assert!(html.contains("updates are unavailable"));
    assert!(!html.contains("without claiming ownership"));
    assert!(!html.contains("private approval"));
}

async fn refresh(
    fixture: &DeliveryFixture,
    cookie: &str,
    request: RequestId,
    csrf: &str,
    json: bool,
) -> axum::response::Response {
    fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/join/organization/refresh")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .header(
                    "accept",
                    if json {
                        "application/json"
                    } else {
                        "text/html"
                    },
                )
                .body(Body::from(format!(
                    "request_id={request}&csrf_token={csrf}"
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn organization_refresh_native_and_enhanced_require_current_capability_requester_and_generation()
 {
    let (fixture, snapshot) = org_fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let csrf = common::csrf_token(&fixture.app, &cookie).await;
    let request = snapshot.create.command.request_id;
    fixture
        .authority
        .seed_organization_delivery(snapshot.clone());
    let response = refresh(&fixture, &cookie, request, &csrf, false).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "calls {:?}",
        fixture.authority.calls()
    );
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert!(body_text(response).await.contains("Refresh requested"));
    fixture
        .authority
        .set_organization_refresh(OrganizationRefresh::Throttled {
            retry_at: "2099-01-01T00:00:00Z".parse().unwrap(),
        });
    let response = refresh(&fixture, &cookie, request, &csrf, true).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(body["kind"], "throttled");
    assert_eq!(body["retry_at"], "2099-01-01T00:00:00Z");
    let calls = fixture
        .authority
        .calls()
        .iter()
        .filter(|m| m.as_str() == "refresh")
        .count();
    assert_eq!(calls, 2);
    assert_eq!(
        refresh(&fixture, &cookie, RequestId::new(), &csrf, false)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        refresh(&fixture, &cookie, request, "wrong", true)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // A terminal Lapsed request retains history but cannot schedule fresh observation.
    let mut terminal = fixture.authority.request(request).unwrap();
    terminal.state = RequestState::Lapsed;
    let link = fixture.authority.link(terminal.link_id).unwrap();
    let mut page = ghinvite_core::admission::RequesterPage {
        two_factor_required: None,
        email_domains: vec![],
        paused: false,
        rerequest_after: None,
        link_id: terminal.link_id.expect("linked fixture"),
        targets: link.creation.targets,
        approval_required: false,
        can_start_fresh: true,
        attempt: None,
        request: Some(terminal),
    };
    fixture.authority.set_requester_page(page.clone());
    assert_eq!(
        refresh(&fixture, &cookie, request, &csrf, true)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .authority
            .calls()
            .iter()
            .filter(|method| method.as_str() == "refresh")
            .count(),
        calls
    );
    page.request.as_mut().unwrap().state = RequestState::Approved;
    fixture.authority.set_requester_page(page);
    // Current generation is authoritative even while SQL and the old owner remain.
    fixture.authority.set_organization_progress(
        request,
        OrganizationProgress {
            org_id: 9001,
            team_ids: vec![7],
            delivery_generation: 2,
            stage: DispatchStage::Submitted,
        },
    );
    let response = refresh(&fixture, &cookie, request, &csrf, true).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("not_tracked"));
    assert_eq!(
        fixture
            .authority
            .calls()
            .iter()
            .filter(|m| m.as_str() == "refresh")
            .count(),
        calls
    );
    // Retired capability cannot trigger reads or refresh through the old session.
    let mut link = fixture
        .authority
        .link(snapshot.create.command.link_id)
        .unwrap()
        .as_link();
    link.secret.secret_generation += 1;
    link.secret.sha256 = ghinvite_core::invitation_link::SecretHash::from_bytes([9; 32]);
    fixture.authority.seed_link(&link);
    assert_eq!(
        refresh(&fixture, &cookie, request, &csrf, true)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .authority
            .calls()
            .iter()
            .filter(|m| m.as_str() == "refresh")
            .count(),
        calls
    );
}
