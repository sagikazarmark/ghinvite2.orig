use super::*;
use ghinvite_core::account::{App, Installation};
use ghinvite_core::storage::AppInstallationStorage;
use ghinvite_ui::link_form::LinkFormIslandProps;
use serde_json::json;
async fn fixture_config(
    repo: bool,
    enabled: bool,
    team_org: u64,
) -> (
    axum::Router,
    String,
    FakeLinkAuthority,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    fixture_access_config(repo, enabled, team_org, false).await
}
async fn fixture_access_config(
    repo: bool,
    enabled: bool,
    team_org: u64,
    access: bool,
) -> (
    axum::Router,
    String,
    FakeLinkAuthority,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    if repo {
        storage
            .seed_console_installation(&acme_installation())
            .await
            .unwrap();
    }
    let mut org = Installation::repo(acme_installation());
    org.app = App::Org;
    org.account.installation_id = 88;
    storage.put_app_installation(&org).await.unwrap();
    let authority = FakeLinkAuthority::start().await;
    let mut expectations = oauth_expectations(ACME_ADMIN);
    expectations.extend([
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/octocat",json!({"user":{"id":42,"login":"octocat"},"organization":{"id":9001,"login":"acme"},"role":"admin","state":"active"})),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme",json!({"id":9001,"login":"acme","created_at":"2020-01-01T00:00:00Z"})),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams?per_page=100",json!([{"id":7,"name":"Learners","slug":"learners"}])),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams/learners",json!({"id":7,"name":"Learners","slug":"learners","organization":{"id":team_org,"login":"acme"}})),
        installation_repos_expectation(),repo_identity(10,"acme/api",9001),repo_identity(11,"acme/web",9001),
    ]);
    let mut config = console_config();
    config.organization_delivery_enabled = enabled;
    config.access_removal_enabled = access;
    config.github_org_install_url =
        Some("https://github.com/apps/fixture-org/installations/new".into());
    let app = build_app(
        crate::common::sign_in::app_state(
            storage.clone(),
            Arc::new(MockTransport::scripted(expectations)),
            authority.client(),
            config,
        ),
        tower_sessions::MemoryStore::default(),
    );
    let cookie = sign_in(&app).await;
    (app, cookie, authority, storage)
}
async fn fixture_full(
    repo: bool,
    enabled: bool,
    team_org: u64,
) -> (
    axum::Router,
    String,
    FakeLinkAuthority,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    fixture_config(repo, enabled, team_org).await
}
async fn fixture(
    repo: bool,
    enabled: bool,
    team_org: u64,
) -> (axum::Router, String, FakeLinkAuthority) {
    let (app, cookie, authority, _) = fixture_full(repo, enabled, team_org).await;
    (app, cookie, authority)
}
fn props(html: &str) -> LinkFormIslandProps {
    serde_json::from_str(
        html.split("<script type=\"application/json\" id=\"link-form-props\">")
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn org_only_duplicate_creates_new_identity_and_secret_with_source_unchanged() {
    let (app, cookie, authority) = fixture(false, true, 9001).await;
    let (_, html) = get_links(&app, &cookie, "/new").await;
    let first = props(&html);
    assert!(first.repos.is_empty());
    assert!(html.contains("Join acme as a Member"));
    let body = "description=Org+workshop&include_organization=true&team_ids=7&approval_required=true&max_uses=7&expires_in_days=9&invitation_lifetime_hours=24&invitation_lifetime_seconds=3601&budget_share_pct=17";
    let created = post_form(&app, &cookie, &first.action, body).await;
    assert_eq!(created.status(), StatusCode::OK);
    assert!(response_html(created).await.contains("/i#"));
    let source = authority.created()[0].clone();
    assert_eq!(source.installation_id, 88);
    assert_eq!(source.organization_installation_id, None);
    assert!(source.targets.repos.is_empty());
    let source_snapshot = authority.link(source.link_id).unwrap();
    let (status, html) = get_links(&app, &cookie, &format!("/{}/duplicate", source.link_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!html.contains("/i#"));
    let duplicate = props(&html);
    assert_ne!(duplicate.action, first.action);
    assert!(duplicate.values.include_organization);
    assert_eq!(duplicate.values.selected_team_ids, vec![7]);
    assert_eq!(duplicate.values.budget_share_pct, "17");
    assert_eq!(duplicate.values.invitation_lifetime_seconds, "3601");
    assert_eq!(
        duplicate.values.expiration_timestamp,
        source.expires_at.unwrap().to_rfc3339()
    );
    let exact_body = format!(
        "{body}&expiration_timestamp={}",
        url::form_urlencoded::byte_serialize(duplicate.values.expiration_timestamp.as_bytes())
            .collect::<String>()
    );
    let second = post_form(&app, &cookie, &duplicate.action, &exact_body).await;
    assert_eq!(second.status(), StatusCode::OK);
    let created = authority.created();
    assert_eq!(created.len(), 2);
    assert_ne!(created[0].link_id, created[1].link_id);
    assert_ne!(created[0].secret_sha256, created[1].secret_sha256);
    assert_eq!(created[0].targets, created[1].targets);
    assert_eq!(created[0].expires_at, created[1].expires_at);
    assert_eq!(
        created[0].invitation_lifetime_seconds,
        created[1].invitation_lifetime_seconds
    );
    assert_eq!(authority.link(source.link_id).unwrap(), source_snapshot);
}
#[tokio::test]
async fn mixed_form_uses_verified_app_bindings_and_preserves_heterogeneous_grants() {
    let (app, cookie, authority) = fixture(true, true, 9001).await;
    let (_, html) = get_links(&app, &cookie, "/new").await;
    let form = props(&html);
    let response=post_form(&app,&cookie,&form.action,"description=Mixed&include_organization=true&team_ids=7&approval_required=true&repo_ids=10&repo_ids=11&repo_grants=10:pull&repo_grants=11:maintain&installation_id=999&organization_installation_id=999&organization_role=admin&team_role=maintainer").await;
    assert_eq!(response.status(), StatusCode::OK);
    let command = authority.created().pop().unwrap();
    assert_eq!(command.installation_id, 77);
    assert_eq!(command.organization_installation_id, Some(88));
    assert_eq!(
        command.targets.repos[0].grant,
        ghinvite_core::targets::RepoRole::Read
    );
    assert_eq!(
        command.targets.repos[1].grant,
        ghinvite_core::targets::RepoRole::Maintain
    );
}
#[tokio::test]
async fn disabled_or_wrong_team_identity_never_creates_organization_link() {
    for (enabled, org) in [(false, 9001), (true, 9999)] {
        let (app, cookie, authority) = fixture(false, enabled, org).await;
        let (_, html) = get_links(&app, &cookie, "/new").await;
        let form = props(&html);
        if !enabled {
            assert!(html.contains("Organization invitations are not enabled"));
            assert!(!html.contains("Join acme as a Member"));
        }
        let response = post_form(
            &app,
            &cookie,
            &form.action,
            "description=Org&include_organization=true&team_ids=7&approval_required=true",
        )
        .await;
        if enabled {
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        } else {
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert!(authority.created().is_empty());
    }
}
#[tokio::test]
async fn organization_install_cta_uses_separate_configured_destination() {
    let (app, cookie, _) = fixture(false, false, 9001).await;
    let response = identity_request(&app, &cookie, "GET", "/install/org").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "https://github.com/apps/fixture-org/installations/new"
    );
}

#[tokio::test]
async fn organization_history_uses_scoped_receipts_and_keeps_each_team_outcome_separate() {
    use ghinvite_core::{
        GithubInvitationId, RequestId,
        org_delivery::*,
        storage::{org_delivery::OrgDeliveryStorage, projection::fixture::Seed},
    };
    let (app, cookie, authority, storage) = fixture_full(false, true, 9001).await;
    let (_, html) = get_links(&app, &cookie, "/new").await;
    let form = props(&html);
    let created = post_form(
        &app,
        &cookie,
        &form.action,
        "description=Org+history&include_organization=true&team_ids=7&approval_required=true",
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let link = authority
        .link(authority.created()[0].link_id)
        .unwrap()
        .as_link();
    storage.seed_link(&link).await.unwrap();
    let request = ghinvite_core::InvitationRequest {
        batch: None,
        domain_evidence: None,
        id: RequestId::new(),
        invitation_link_id: Some(link.id),
        requester_id: 42,
        justification: None,
        state: ghinvite_core::RequestState::Approved,
        decided_by: Some(42),
        decided_at: Some(chrono::Utc::now()),
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
        decision_deadline: None,
        created_at: chrono::Utc::now(),
    };
    storage.seed_request(&request).await.unwrap();
    let command = OrganizationCreateCommand {
        batch: None,
        budget_attribution: Default::default(),
        access_lifetime_seconds: None,
        invitation_id: Some(GithubInvitationId::new()),
        link_id: Some(link.id),
        request_id: request.id,
        approval_id: "private approval identity".into(),
        account_id: 9001,
        installation_id: 88,
        requester_id: 42,
        organization: link.targets.organization.clone().unwrap(),
        teams: link.targets.teams.clone(),
        delivery_generation: 1,
        approved_at: request.created_at,
        budget_share_pct: 50,
    };
    let mut snapshot = OrganizationDeliverySnapshot::planned(command).unwrap();
    snapshot.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
        role: OrganizationRole::Admin,
    };
    snapshot.create.confirmed_at = Some(request.created_at);
    snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::Blocked {
        reason: OrganizationBlockedReason::IdpSyncedTeam,
    };
    storage
        .project_organization_delivery(&snapshot)
        .await
        .unwrap();
    let path = format!("/console/accounts/acme/requests/{}", request.id);
    let response = identity_request(&app, &cookie, "GET", &path).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let html = response_html(response).await;
    assert!(html.contains("Recorded organization delivery history: acme"));
    assert!(html.contains("Recorded delivery generation: 1"));
    assert!(html.contains("Organization owner access was verified and preserved"));
    assert!(html.contains("this team is managed by an identity provider"));
    assert!(!html.contains("Repository details unavailable"));
    assert!(!html.contains("private approval identity"));

    // Advancing the owner cannot turn an earlier positive receipt into the
    // active generation's outcome. Both generations remain explicitly visible.
    let mut second = snapshot.clone();
    second.create.command.delivery_generation = 2;
    second.create.command.invitation_id = Some(GithubInvitationId::new());
    second.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
    second.create.confirmed_at = None;
    storage
        .project_organization_delivery(&second)
        .await
        .unwrap();
    authority.seed_request(ghinvite_core::request_lifecycle::RequestSnapshot {
        batch: None,
        domain_evidence: None,
        request_id: request.id,
        link_id: Some(link.id),
        account_id: link.account_id,
        requester_id: request.requester_id,
        justification: None,
        state: request.state,
        admitted_at: request.created_at,
        decision_deadline: None,
        revision: 1,
        decision: Some(ghinvite_core::request_lifecycle::TerminalDecision {
            decision_id: "private approval identity".into(),
            decided_by: request.decided_by,
            effective_at: request.created_at,
            evaluated_at: request.created_at,
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
        }),
    });
    authority.set_organization_progress(
        request.id,
        ghinvite_core::request_lifecycle::OrganizationProgress {
            delivery_generation: 2,
            org_id: 9001,
            team_ids: vec![7],
            stage: ghinvite_core::delivery::DispatchStage::Submitted,
        },
    );
    let html = response_html(identity_request(&app, &cookie, "GET", &path).await).await;
    let (active, history) = html
        .split_once("Recorded organization delivery history: acme")
        .unwrap();
    assert!(active.contains("Delivery generation: 2"));
    assert!(active.contains("Outcome unknown"));
    assert!(!active.contains("owner access was verified"));
    assert!(history.contains("Recorded delivery generation: 1"));
    assert!(history.contains("owner access was verified"));
    assert!(!history.contains("Recorded delivery generation: 2"));

    // An owner outage retains historical receipts without guessing which
    // generation is current from SQL order or its maximum generation.
    authority.fail("organization_delivery_progress", 503);
    let html = response_html(identity_request(&app, &cookie, "GET", &path).await).await;
    let (active, history) = html
        .split_once("Recorded organization delivery history: acme")
        .unwrap();
    assert!(active.contains("updates are unavailable"));
    assert!(!active.contains("Delivery generation:"));
    assert!(!active.contains("owner access was verified"));
    assert!(!active.contains("Outcome unknown"));
    assert!(history.contains("Recorded delivery generation: 1"));
    assert!(history.contains("Recorded delivery generation: 2"));
    assert!(!html.contains("private approval identity"));
    let response = identity_request(&app, "", "GET", &path).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(!response_html(response).await.contains("learners"));
    let wrong = path.replace("/acme/", "/other-account/");
    let response = identity_request(&app, &cookie, "GET", &wrong).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!response_html(response).await.contains("learners"));
}

#[tokio::test]
async fn lapsed_history_preserves_approval_actor_time_and_delayed_delivery_warning() {
    use ghinvite_core::storage::projection::fixture::Seed;
    let (app, cookie, authority, storage) = fixture_full(false, true, 9001).await;
    let (_, html) = get_links(&app, &cookie, "/new").await;
    let form = props(&html);
    let created = post_form(
        &app,
        &cookie,
        &form.action,
        "description=Lapsed+history&include_organization=true&team_ids=7&approval_required=true",
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let link = authority
        .link(authority.created()[0].link_id)
        .unwrap()
        .as_link();
    storage.seed_link(&link).await.unwrap();
    let approved_at = "2026-01-01T12:34:56Z".parse().unwrap();
    let request = ghinvite_core::InvitationRequest {
        batch: None,
        domain_evidence: None,
        id: ghinvite_core::RequestId::new(),
        invitation_link_id: Some(link.id),
        requester_id: 42,
        justification: None,
        state: ghinvite_core::RequestState::Lapsed,
        decided_by: Some(42),
        decided_at: Some(approved_at),
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
        decision_deadline: None,
        created_at: approved_at,
    };
    use ghinvite_core::storage::projection::{
        ApprovedRequestProjection, AuditIntent, ProjectionStorage, RequestProjectionEnvelope,
        fixture::request_snapshot,
    };
    let mut approved = request_snapshot(&link, &request, 2);
    approved.state = ghinvite_core::RequestState::Approved;
    let mut lapsed = approved.clone();
    lapsed.state = ghinvite_core::RequestState::Lapsed;
    lapsed.revision = 3;
    storage
        .apply_request(&RequestProjectionEnvelope {
            approval: Some(ApprovedRequestProjection {
                reissue: None,
                request: approved,
                events: vec![],
            }),
            request: lapsed,
            events: vec![AuditIntent {
                event_id: format!("request.lapsed/{}", request.id),
                kind: ghinvite_core::audit::EventType::RequestLapsed,
                actor_id: None,
                target_id: request.id.to_string(),
                effective_at: approved_at + chrono::Duration::days(7),
                evaluated_at: approved_at + chrono::Duration::days(7),
            }],
        })
        .await
        .unwrap();
    let path = format!("/console/accounts/acme/requests/{}", request.id);
    let response = identity_request(&app, &cookie, "GET", &path).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let html = response_html(response).await;
    assert!(html.contains("State: lapsed"));
    assert!(html.contains("Original approval time: 2026-01-01 12:34:56 UTC"));
    assert!(html.contains("Decision actor: @octocat · GitHub user ID 42"));
    assert!(html.contains("original approval remains recorded"));
    assert!(html.contains("learners"));
    assert!(html.contains("updates are unavailable"));
    assert!(!html.contains("Active organization membership was verified"));
}

#[tokio::test]
async fn restricted_creation_and_duplicate_preserve_rules() {
    let (app, cookie, authority, _) = fixture_config(true, true, 9001).await;
    let (_, html) = get_links(&app, &cookie, "/new").await;
    let first = props(&html);
    let body = "description=Domain+workshop&repo_ids=10&repo_grants=10%3Apush&approval_required=true&max_uses=7&email_domains=EXAMPLE.COM.%2Cexample.com%2C*.b%C3%BCcher.example";
    let response = post_form(&app, &cookie, &first.action, body).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(html.contains("/i#"));
    let source = authority.created()[0].clone();
    assert_eq!(
        source
            .email_domains
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["example.com", "*.xn--bcher-kva.example"]
    );
    let before = authority.link(source.link_id).unwrap();
    let (_, html) = get_links(&app, &cookie, &format!("/{}/duplicate", source.link_id)).await;
    let duplicate = props(&html);
    assert_eq!(
        duplicate.values.email_domains,
        "example.com, *.xn--bcher-kva.example"
    );
    let second = post_form(&app, &cookie, &duplicate.action, body).await;
    assert_eq!(second.status(), StatusCode::OK);
    let all = authority.created();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].email_domains, all[1].email_domains);
    assert_ne!(all[0].link_id, all[1].link_id);
    assert_ne!(all[0].secret_sha256, all[1].secret_sha256);
    assert_eq!(authority.link(source.link_id).unwrap(), before);
}

#[tokio::test]
async fn eligibility_panel_is_advisory_and_rejects_stale_or_misbound_facts() {
    use ghinvite_core::org_eligibility::{BasePermission, Facts, Seats};
    let (app, cookie, authority) = fixture(true, true, 9001).await;
    let facts = Facts {
        account_id: 9001,
        installation_id: 88,
        observed_at: Utc::now(),
        seats: Some(Seats {
            capacity: 10,
            used: 10,
        }),
        two_factor_required: Some(true),
        base_permission: Some(BasePermission::Read),
        managed_enterprise: None,
    };
    authority.seed_organization_facts(9001, facts.clone());
    let (status, html) = get_links(&app, &cookie, "/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("No free organization seat was reported"));
    assert!(html.contains("two-factor authentication to accept"));
    assert!(html.contains("Enterprise-managed user policy: unknown"));
    assert!(
        !html.contains("disabled=\""),
        "advisory warnings never disable creation"
    );
    assert!(matches!(
        props(&html).eligibility,
        ghinvite_ui::organization_eligibility::Eligibility::Known(_)
    ));
    let created = post_form(&app, &cookie, &props(&html).action,
        "description=Advisory+only&include_organization=true&approval_required=true&max_uses=7&expires_in_days=9").await;
    assert_eq!(created.status(), StatusCode::OK);
    let source = authority.created()[0].clone();
    let (status, detail) = get_links(&app, &cookie, &format!("/{}", source.link_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail.contains("No free organization seat was reported"));
    for invalid in [
        Facts {
            observed_at: Utc::now() - chrono::Duration::hours(1),
            ..facts.clone()
        },
        Facts {
            observed_at: Utc::now() + chrono::Duration::hours(1),
            ..facts.clone()
        },
        Facts {
            account_id: 9002,
            ..facts.clone()
        },
        Facts {
            installation_id: 99,
            ..facts.clone()
        },
    ] {
        authority.seed_organization_facts(9001, invalid);
        let (status, html) = get_links(&app, &cookie, "/new").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Organization conditions are unknown"));
        assert!(!html.contains("No free organization seat was reported"));
        assert!(matches!(
            props(&html).eligibility,
            ghinvite_ui::organization_eligibility::Eligibility::Unknown
        ));
    }
    authority.fail_once("facts", 503);
    let (status, html) = get_links(&app, &cookie, "/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Organization conditions are unknown"));
    let calls = authority.received::<serde_json::Value>("facts").len();
    let (status, html) = get_links(&app, "", "/new").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(!html.contains("Organization eligibility"));
    assert_eq!(
        authority.received::<serde_json::Value>("facts").len(),
        calls
    );
    let concealed = app
        .clone()
        .oneshot(
            Request::get("/console/accounts/not-authorized/links/new")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(concealed.status(), StatusCode::NOT_FOUND);
    assert!(
        !response_html(concealed)
            .await
            .contains("Organization eligibility")
    );
    assert_eq!(
        authority.received::<serde_json::Value>("facts").len(),
        calls
    );
}

#[tokio::test]
async fn access_lifetime_requires_capability_and_survives_exact_duplicate() {
    for enabled in [false, true] {
        let (app, cookie, authority, _) = fixture_access_config(false, true, 9001, enabled).await;
        let (_, html) = get_links(&app, &cookie, "/new").await;
        let first = props(&html);
        assert_eq!(first.access_removal_enabled, enabled);
        let body = "description=Finite+access&include_organization=true&team_ids=7&approval_required=true&access_lifetime_hours=3601s";
        let response = post_form(&app, &cookie, &first.action, body).await;
        if !enabled {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let html = response_html(response).await;
            assert_eq!(props(&html).values.access_lifetime_hours, "3601s");
            assert!(html.contains("Access expiry is unavailable"));
            assert!(authority.created().is_empty());
            continue;
        }
        assert_eq!(response.status(), StatusCode::OK);
        let original = authority.created()[0].clone();
        assert_eq!(original.access_lifetime_seconds, Some(3601));
        let (status, html) =
            get_links(&app, &cookie, &format!("/{}/duplicate", original.link_id)).await;
        assert_eq!(status, StatusCode::OK);
        let duplicate = props(&html);
        assert_eq!(duplicate.values.access_lifetime_hours, "3601s");
        let response = post_form(&app, &cookie, &duplicate.action, body).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(authority.created()[1].access_lifetime_seconds, Some(3601));
        assert_eq!(authority.created()[0], original);
    }
}
