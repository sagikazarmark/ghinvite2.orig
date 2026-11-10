use super::*;
use ghinvite_core::targets::{MemberGrant, OrganizationTarget, TeamTarget};
fn org_targets(team: Option<u64>) -> Targets {
    Targets {
        organization: Some(OrganizationTarget {
            org_id: 100,
            org_login: "acme".into(),
            grant: MemberGrant::Member,
        }),
        teams: team
            .into_iter()
            .map(|id| TeamTarget {
                team_id: id,
                team_slug: format!("team-{id}"),
                grant: MemberGrant::Member,
            })
            .collect(),
        repos: vec![],
    }
}
fn org_identity(id: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/orgs/acme",
        json!({"id":id,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
    )
}
fn owner(role: &str, state: &str, id: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/orgs/acme/memberships/alice",
        json!({"role":role,"state":state,"user":{"id":id,"login":"alice"},"organization":{"id":100,"login":"acme"}}),
    )
}
async fn org_fixture(expectations: Vec<Expectation>, repo: bool) -> (AppState, MockTransport) {
    let mut installation = Installation::repo(account());
    installation.app = App::Org;
    installation.account.installation_id = 2;
    let (mut state, storage, mock, _) = fixture_with(expectations, false, installation).await;
    state.config.organization_delivery_enabled = true;
    if repo {
        storage
            .put_app_installation(&Installation::repo(account()))
            .await
            .unwrap();
    }
    (state, mock)
}
#[tokio::test]
async fn organization_only_selection_walks_all_pages_with_exact_read_profile() {
    let teams: Vec<_> = (1..=100)
        .map(|id| json!({"id":id,"name":format!("Team {id}"),"slug":format!("team-{id}")}))
        .collect();
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.com/orgs/acme/teams?per_page=100",
        json!(teams),
    );
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.com/orgs/acme/teams?per_page=100&page=2>; rel=\"next\"".into(),
    );
    let (state,mock)=org_fixture(vec![owner("admin","active",8),org_identity(100),first,
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams?per_page=100&page=2",json!([{"id":101,"name":"Last team","slug":"team-101"}])),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams/team-101",json!({"id":101,"name":"Last team","slug":"team-101","organization":{"id":100,"login":"acme"}})),
    ],false).await;
    let view = delegate(&state).await;
    let ghinvite_ui::link_form::OrganizationOffer::Available(choices) =
        view.organization_offer().await.unwrap()
    else {
        panic!("organization choices")
    };
    assert_eq!(choices.teams.len(), 101);
    let target = org_targets(Some(101));
    assert_eq!(
        view.can_create_targets(&target).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(view.creation_installations(&target).unwrap(), (2, None));
    assert_eq!(
        view.can_mutate_targets(&target).await.unwrap(),
        Access::Allowed
    );
    // Every read used the organization app's Console scope (members: read).
    assert_eq!(console_generation(true), 1);
    mock.assert_exhausted();
}
#[tokio::test]
async fn selected_team_must_match_numeric_organization_and_not_known_sync() {
    for (org, synced) in [(999, None), (100, Some("group=managed"))] {
        let (state,mock)=org_fixture(vec![owner("admin","active",8),org_identity(100),Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams/team-1",json!({"id":1,"name":"Team","slug":"team-1","organization":{"id":org,"login":"acme"},"ldap_dn":synced}))],false).await;
        assert_eq!(
            delegate(&state)
                .await
                .can_create_targets(&org_targets(Some(1)))
                .await
                .unwrap(),
            Access::Denied
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn pending_or_mismatched_user_never_gets_organization_authority() {
    for (state_name, id) in [("pending", 8), ("active", 999)] {
        let (state, mock) = org_fixture(vec![owner("admin", state_name, id)], false).await;
        assert!(
            AuthorityView::open(&state, 8, "alice", &account())
                .await
                .unwrap()
                .is_none()
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn mixed_selection_requires_both_current_installations_and_repo_identity() {
    let (state, mock) = org_fixture(
        vec![
            owner("admin", "active", 8),
            org_identity(100),
            page(&[1]),
            identity(1, 100),
        ],
        true,
    )
    .await;
    let view = delegate(&state).await;
    let mut target = org_targets(None);
    target.repos = targets(&[1]).repos;
    assert_eq!(
        view.can_create_targets(&target).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(view.creation_installations(&target).unwrap(), (1, Some(2)));
    mock.assert_exhausted();
    let (state, mock) =
        org_fixture(vec![owner("admin", "active", 8), org_identity(100)], false).await;
    let view = delegate(&state).await;
    assert_eq!(
        view.can_create_targets(&target).await.unwrap(),
        Access::Denied
    );
    assert!(view.creation_installations(&target).is_err());
    mock.assert_exhausted();
}
#[tokio::test]
async fn later_team_page_failure_never_exposes_a_partial_picker() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.com/orgs/acme/teams?per_page=100",
        json!([{"id":1,"name":"One","slug":"one"}]),
    );
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.com/orgs/other/teams?page=2>; rel=\"next\"".into(),
    );
    let (state, mock) = org_fixture(
        vec![owner("admin", "active", 8), org_identity(100), first],
        false,
    )
    .await;
    assert!(delegate(&state).await.organization_offer().await.is_err());
    mock.assert_exhausted();
}
