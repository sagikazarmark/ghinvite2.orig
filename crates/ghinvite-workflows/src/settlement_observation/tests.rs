use super::*;
use crate::settlement_policy::{ReconcileDecision, evaluate, tests::snapshot};
use ghinvite_core::{AccountType, InvitationState};
use ghinvite_github::{
    AppSlot, CallError, Method,
    mocks::{Expectation, MockTransport},
    profiles,
};
use serde_json::json;

const BASE: &str = ghinvite_github::mocks::API_BASE;
fn get(path: &str, body: serde_json::Value) -> Expectation {
    Expectation::ok_json(Method::Get, format!("{BASE}{path}"), body)
}
fn list(body: serde_json::Value) -> Expectation {
    get("/repos/acme/api/invitations?per_page=100", body)
}
fn invitation(id: u64, user: u64) -> serde_json::Value {
    json!({"id":id,"invitee":{"id":user,"login":"alice"},"permissions":"write","created_at":"2026-05-04T12:00:00Z","expired":false})
}
fn role_reads(role: &str) -> Vec<Expectation> {
    vec![
        get("/user/8", json!({"id":8,"login":"alice"})),
        get("/users/alice", json!({"id":8,"login":"alice"})),
        get(
            "/repos/acme/api/collaborators/alice/permission",
            json!({"user":{"id":8,"login":"alice"},"permission":"write","role_name":role}),
        ),
    ]
}
fn available() -> Vec<Expectation> {
    vec![
        get(
            "/app/installations/9",
            json!({"id":9,"account":{"id":100,"login":"acme"},"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":null}),
        ),
        get(
            "/repos/acme/api",
            json!({"id":10,"full_name":"acme/api","private":true}),
        ),
    ]
}
async fn run(
    snapshot: &DeliverySnapshot,
    script: Vec<Expectation>,
    policy: AcceptancePolicy,
) -> Result<Option<Observation>, String> {
    let mock = MockTransport::scripted(script);
    let app = crate::test_support::github_apps(&mock)
        .app(AppSlot::Repo)
        .unwrap();
    let app_rest = app.app_rest().unwrap();
    let app_rest = &app_rest.rest();
    let account = Account {
        installation_id: 9,
        account_id: 100,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: snapshot.create.command.approved_at,
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    };
    let result = app
        .write_once(9, &profiles::REPO_READ, |handle| async move {
            observe(
                &handle,
                app_rest,
                &account,
                snapshot,
                snapshot.create.command.approved_at,
                policy,
            )
            .await
            .map_err(CallError::<std::convert::Infallible>::Api)
        })
        .await
        .map_err(|error| format!("{error:?}"));
    mock.assert_exhausted();
    result
}

#[tokio::test]
async fn effective_read_below_write_declines_and_records_role() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    let script = std::iter::once(list(json!([])))
        .chain(role_reads("read"))
        .chain(available())
        .collect();
    let observation = run(&snapshot, script, AcceptancePolicy::default())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        evaluate(&snapshot, &observation, AcceptancePolicy::default()),
        ReconcileDecision::Settle {
            state: InvitationState::Declined,
            ..
        }
    ));
    assert!(matches!(
        observation.evidence,
        Evidence::Absent {
            role_name: Some(EffectiveRole::Read),
            availability_verified: true,
            ..
        }
    ));
}

#[tokio::test]
async fn missing_repository_after_empty_listing_is_not_absent_evidence() {
    let mut verification = available();
    verification[1].response.status = 404;
    let script = std::iter::once(list(json!([])))
        .chain(role_reads("write"))
        .chain(verification)
        .collect();
    assert!(
        run(
            &snapshot(CreateOutcome::Created { upstream_id: 99 }),
            script,
            AcceptancePolicy::default()
        )
        .await
        .unwrap_err()
        .contains("NotFound")
    );
}

#[tokio::test]
async fn uncertain_absence_never_checks_role_or_settles() {
    assert!(
        run(
            &snapshot(CreateOutcome::OutcomeUnknown),
            vec![list(json!([]))],
            AcceptancePolicy::default()
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn multiple_invitee_matches_do_not_guess_an_upstream_identity() {
    assert!(
        run(
            &snapshot(CreateOutcome::OutcomeUnknown),
            vec![list(json!([invitation(99, 8), invitation(100, 8)]))],
            AcceptancePolicy::default(),
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn uncertain_presence_on_page_two_resolves_by_numeric_invitee() {
    let snapshot = snapshot(CreateOutcome::OutcomeUnknown);
    let mut first = list(json!(
        (0..100)
            .map(|id| invitation(1000 + id, 5000 + id))
            .collect::<Vec<_>>()
    ));
    first.response.headers.insert(
        "link".into(),
        format!("<{BASE}/repos/acme/api/invitations?page=2>; rel=next"),
    );
    let second = get(
        "/repos/acme/api/invitations?page=2",
        json!([invitation(99, 8)]),
    );
    let script = [first, second].into_iter().chain(available()).collect();
    let observation = run(&snapshot, script, AcceptancePolicy::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        evaluate(&snapshot, &observation, AcceptancePolicy::default()),
        ReconcileDecision::ResolveCreated { upstream_id: 99 }
    );
}

#[tokio::test]
async fn failed_later_page_never_exposes_partial_absence() {
    let mut first = list(json!([]));
    first.response.headers.insert(
        "link".into(),
        format!("<{BASE}/repos/acme/api/invitations?page=2>; rel=next"),
    );
    let mut second = get("/repos/acme/api/invitations?page=2", json!({}));
    second.response.status = 503;
    assert!(
        run(
            &snapshot(CreateOutcome::Created { upstream_id: 99 }),
            vec![first, second],
            AcceptancePolicy::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn direct_flag_uses_all_pages_and_numeric_identity() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    let mut first = get(
        "/repos/acme/api/collaborators?affiliation=direct&per_page=100",
        json!([]),
    );
    first.response.headers.insert(
        "link".into(),
        format!("<{BASE}/repos/acme/api/collaborators?affiliation=direct&page=2>; rel=next"),
    );
    let second = get(
        "/repos/acme/api/collaborators?affiliation=direct&page=2",
        json!([{"id":8,"login":"alice-renamed","role_name":"write"}]),
    );
    let script = std::iter::once(list(json!([])))
        .chain(role_reads("write"))
        .chain([first, second])
        .chain(available())
        .collect();
    let policy = AcceptancePolicy {
        require_direct_collaborator: true,
    };
    let observation = run(&snapshot, script, policy).await.unwrap().unwrap();
    assert!(matches!(
        evaluate(&snapshot, &observation, policy),
        ReconcileDecision::Settle {
            state: InvitationState::Accepted,
            ..
        }
    ));
}

#[tokio::test]
async fn recycled_login_cannot_supply_membership_evidence() {
    let script = vec![
        list(json!([])),
        get("/user/8", json!({"id":8,"login":"alice"})),
        get("/users/alice", json!({"id":50,"login":"alice"})),
    ];
    assert!(
        run(
            &snapshot(CreateOutcome::Created { upstream_id: 99 }),
            script,
            AcceptancePolicy::default()
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn present_foreign_invitation_is_observed_using_reads_only() {
    let script = std::iter::once(list(json!([invitation(99, 8)])))
        .chain(available())
        .collect();
    let snapshot = snapshot(CreateOutcome::AlreadyPending {
        upstream_id: 99,
        permission: ghinvite_core::targets::RepoRole::Write,
    });
    let observation = run(&snapshot, script, AcceptancePolicy::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        evaluate(&snapshot, &observation, AcceptancePolicy::default()),
        ReconcileDecision::NoChange
    );
    assert!(!crate::settlement_policy::can_withdraw(&snapshot));
}

#[tokio::test]
async fn credentials_of_another_installation_or_app_are_refused() {
    let snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
    let account = Account {
        installation_id: 9,
        account_id: 100,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: snapshot.create.command.approved_at,
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    };
    let mock = MockTransport::scripted(vec![]);
    let apps = crate::test_support::github_apps(&mock);
    let repo = apps.app(AppSlot::Repo).unwrap();
    for (installation, jwt_slot) in [(10, AppSlot::Repo), (9, AppSlot::Org)] {
        let app_rest = apps.app(jwt_slot).unwrap().app_rest().unwrap();
        let app_rest = &app_rest.rest();
        let (account, snapshot) = (&account, &snapshot);
        let result = repo
            .write_once(installation, &profiles::REPO_READ, |handle| async move {
                observe_listed(
                    &handle,
                    app_rest,
                    account,
                    snapshot,
                    snapshot.create.command.approved_at,
                    AcceptancePolicy::default(),
                    &[],
                )
                .await
                .map_err(CallError::<std::convert::Infallible>::Api)
            })
            .await;
        assert!(
            matches!(
                result,
                Err(ghinvite_github::TokenCallError::Call(CallError::Api(
                    ApiError::Decode {
                        what: "observation_credential_binding"
                    }
                )))
            ),
            "{result:?}"
        );
    }
    mock.assert_exhausted();
}
