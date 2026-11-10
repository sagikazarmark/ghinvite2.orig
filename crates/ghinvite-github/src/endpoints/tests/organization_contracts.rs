use super::*;
use crate::{
    endpoints::organization::*,
    roles::{MembershipState, OrgRole, TeamRole},
};
fn invite(id: u64) -> serde_json::Value {
    json!({"id":id,"login":"alice","email":null,"role":"direct_member","created_at":"2026-01-01T00:00:00Z","inviter":{"id":5,"login":"owner"},"team_count":1})
}
#[tokio::test]
async fn typed_memberships_preserve_pending_and_admin_maintainer() {
    let mock = MockTransport::scripted(vec![
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/memberships/alice",
            json!({"role":"admin","state":"pending","user":{"id":8,"login":"alice"},"organization":{"id":9,"login":"acme"}}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/teams/ops/memberships/alice",
            json!({"role":"maintainer","state":"active"}),
        ),
        Expectation::ok_json(
            Method::Put,
            "https://api.github.test/orgs/acme/teams/ops/memberships/alice",
            json!({"role":"maintainer","state":"active"}),
        )
        .require_body(br#"{"role":"maintainer"}"#.to_vec()),
    ]);
    let env = env(&mock);
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    let membership = organization_membership(&rest, "acme", "alice")
        .await
        .unwrap();
    assert_eq!(membership.role, OrgRole::Admin);
    assert_eq!(membership.state, MembershipState::Pending);
    assert_eq!(membership.organization.id, 9);
    let team = team_membership(&rest, "acme", "ops", "alice")
        .await
        .unwrap();
    assert_eq!(team.role, TeamRole::Maintainer);
    assert_eq!(
        put_team_membership(&rest, "acme", "ops", "alice", team.role)
            .await
            .unwrap(),
        team
    );
    mock.assert_exhausted();
}
#[test]
fn unknown_roles_states_and_missing_facts_never_default_to_authority() {
    for value in [
        json!({"role":"custom","state":"active"}),
        json!({"role":"member","state":"accepted"}),
        json!({"role":"member"}),
    ] {
        assert!(serde_json::from_value::<TeamMembership>(value).is_err());
    }
    assert!(serde_json::from_str::<OrgRole>("\"owner\"").is_err());
    let org: Organization =
        serde_json::from_value(json!({"id":9,"login":"acme","created_at":"2026-01-01T00:00:00Z"}))
            .unwrap();
    assert_eq!(org.plan, None);
    assert_eq!(org.two_factor_requirement_enabled, None);
}
#[tokio::test]
async fn invitation_identity_body_and_201_are_explicit() {
    let input = OrganizationInvitationInput {
        invitee: Invitee::User { invitee_id: 8 },
        role: CreateInvitationRole::DirectMember,
        team_ids: vec![41, 42],
    };
    assert_eq!(
        serde_json::to_value(&input).unwrap(),
        json!({"invitee_id":8,"role":"direct_member","team_ids":[41,42]})
    );
    let mut e = Expectation::ok_json(
        Method::Post,
        "https://api.github.test/orgs/acme/invitations",
        invite(71),
    )
    .require_body(serde_json::to_vec(&input).unwrap());
    e.response.status = 201;
    let mock = MockTransport::scripted(vec![e]);
    let env = env(&mock);
    let cred = installation();
    assert_eq!(
        create_invitation(&Rest::new(&env, &cred), "acme", &input)
            .await
            .unwrap()
            .id,
        71
    );
    mock.assert_exhausted();
    let email = OrganizationInvitationInput {
        invitee: Invitee::Email {
            email: "alice@example.test".into(),
        },
        role: CreateInvitationRole::DirectMember,
        team_ids: vec![],
    };
    let wire = serde_json::to_value(email).unwrap();
    assert_eq!(wire["email"], "alice@example.test");
    assert!(wire.get("invitee_id").is_none());
    let mock = MockTransport::scripted(vec![Expectation::ok_json(
        Method::Post,
        "https://api.github.test/orgs/acme/invitations",
        invite(71),
    )]);
    let env = super::env(&mock);
    assert!(matches!(
        create_invitation(&Rest::new(&env, &cred), "acme", &input).await,
        Err(ApiError::Decode { .. })
    ));
    mock.assert_exhausted();
}
#[tokio::test]
async fn invitation_post_failure_is_one_attempt_and_not_retry_safe() {
    let mock = MockTransport::scripted(vec![Expectation::status(
        Method::Post,
        "https://api.github.test/orgs/acme/invitations",
        503,
    )]);
    let env = env(&mock);
    let cred = installation();
    let err = create_invitation(
        &Rest::new(&env, &cred),
        "acme",
        &OrganizationInvitationInput {
            invitee: Invitee::User { invitee_id: 8 },
            role: CreateInvitationRole::DirectMember,
            team_ids: vec![],
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ApiError::Server { status: 503 }));
    assert!(!Endpoint::CreateOrgInvitation.meta().retry_safe);
    assert!(!ApiError::Ambiguous.is_transient_for(&Endpoint::CreateOrgInvitation.meta()));
    mock.assert_exhausted();
}
#[tokio::test]
async fn pending_failed_and_team_lists_walk_link_pages_without_guessing_expiry() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/orgs/acme/invitations?per_page=100",
        json!([invite(1)]),
    );
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.test/orgs/acme/invitations?per_page=100&page=2>; rel=\"next\"".into(),
    );
    let mut failed = invite(3);
    failed["failed_at"] = json!("2026-01-02T00:00:00Z");
    failed["failed_reason"] = json!("UNVERIFIED_UPSTREAM_PROSE");
    let mock = MockTransport::scripted(vec![
        first,
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/invitations?per_page=100&page=2",
            json!([invite(2)]),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/failed_invitations?per_page=100",
            json!([failed]),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/teams?per_page=100",
            json!([{"id":4,"slug":"ops","name":"Operations"}]),
        ),
    ]);
    let env = env(&mock);
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    assert_eq!(
        list_invitations(&rest, "acme")
            .await
            .unwrap()
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let failed = list_failed_invitations(&rest, "acme").await.unwrap();
    assert_eq!(failed[0].invitation.id, 3);
    assert!(!format!("{failed:?}").contains("UNVERIFIED_UPSTREAM_PROSE"));
    assert_eq!(list_teams(&rest, "acme").await.unwrap()[0].id, 4);
    mock.assert_exhausted();
}
#[tokio::test]
async fn org_pagination_never_returns_a_partial_prefix_after_later_failure() {
    for next in [
        "https://evil.test/orgs/acme/invitations?page=2",
        "https://api.github.test/orgs/other/invitations?page=2",
        "https://api.github.test/orgs/acme/invitations?page=2",
    ] {
        let mut first = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/invitations?per_page=100",
            json!([invite(1)]),
        );
        first
            .response
            .headers
            .insert("link".into(), format!("<{next}>; rel=\"next\""));
        let mut script = vec![first];
        if next == "https://api.github.test/orgs/acme/invitations?page=2" {
            script.push(Expectation::status(Method::Get, next, 500));
        }
        let mock = MockTransport::scripted(script);
        let env = env(&mock);
        let cred = installation();
        assert!(
            list_invitations(&Rest::new(&env, &cred), "acme")
                .await
                .is_err()
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn org_and_team_deletes_require_204_and_encode_each_path_segment() {
    let mock = MockTransport::scripted(vec![
        Expectation::status(
            Method::Delete,
            "https://api.github.test/orgs/acme/invitations/71",
            204,
        ),
        Expectation::status(
            Method::Delete,
            "https://api.github.test/orgs/acme/memberships/alice",
            204,
        ),
        Expectation::status(
            Method::Delete,
            "https://api.github.test/orgs/acme/teams/ops%2Fstaff/memberships/alice",
            204,
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/teams/ops",
            json!({"id":41,"slug":"ops","name":"Ops"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme",
            json!({"id":9,"login":"acme","created_at":"2026-01-01T00:00:00Z","plan":{"name":"free"}}),
        ),
    ]);
    let env = env(&mock);
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    cancel_invitation(&rest, "acme", 71).await.unwrap();
    remove_organization_member(&rest, "acme", "alice")
        .await
        .unwrap();
    remove_team_membership(&rest, "acme", "ops/staff", "alice")
        .await
        .unwrap();
    assert_eq!(get_team(&rest, "acme", "ops").await.unwrap().id, 41);
    assert_eq!(
        get_organization(&rest, "acme")
            .await
            .unwrap()
            .plan
            .unwrap()
            .seats,
        None
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn public_org_helpers_refuse_app_credentials_before_http() {
    let mock = MockTransport::scripted(vec![]);
    let env = env(&mock);
    {
        let cred = app();
        let rest = Rest::new(&env, &cred);
        assert!(matches!(
            list_failed_invitations(&rest, "acme").await,
            Err(ApiError::Forbidden {
                reason: ForbiddenReason::WrongCredential
            })
        ));
        assert!(matches!(
            put_team_membership(&rest, "acme", "ops", "alice", TeamRole::Member).await,
            Err(ApiError::Forbidden {
                reason: ForbiddenReason::WrongCredential
            })
        ));
    }
    mock.assert_exhausted();
}

#[test]
fn documented_failed_list_blank_timestamp_remains_unknown() {
    let mut wire = invite(1);
    wire["failed_at"] = json!("");
    let failed: FailedOrganizationInvitation = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(failed.failed_at, None);
    wire["failed_at"] = json!("not-a-date");
    assert!(serde_json::from_value::<FailedOrganizationInvitation>(wire).is_err());
}

#[test]
fn list_org_teams_permission_unverified() {
    assert_eq!(
        Endpoint::ListOrgTeams.meta().permission,
        Some((Permission::Members, Access::Read))
    );
}

#[test]
fn built_in_role_ranks_do_not_demote_org_admins_or_team_maintainers() {
    assert!(OrgRole::Admin.rank() > OrgRole::Member.rank());
    assert!(TeamRole::Maintainer.rank() > TeamRole::Member.rank());
    assert_eq!(OrgRole::Admin.as_str(), "admin");
    assert_eq!(TeamRole::Maintainer.as_str(), "maintainer");
    assert_eq!(
        Endpoint::GetOrganizationMembership,
        Endpoint::GetOrgMembership
    );
}

#[tokio::test]
async fn invitation_team_evidence_walks_complete_scoped_pages() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/orgs/acme/invitations/99/teams?per_page=100",
        json!([{"id":5,"slug":"ops","name":"Ops"}]),
    );
    first.response.headers.insert("link".into(),"<https://api.github.test/orgs/acme/invitations/99/teams?per_page=100&page=2>; rel=\"next\"".into());
    let mock = MockTransport::scripted(vec![
        first,
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/orgs/acme/invitations/99/teams?per_page=100&page=2",
            json!([{"id":6,"slug":"dev","name":"Dev"}]),
        ),
    ]);
    let env = env(&mock);
    let credential = installation();
    let teams = list_invitation_teams(&Rest::new(&env, &credential), "acme", 99)
        .await
        .unwrap();
    assert_eq!(teams.iter().map(|t| t.id).collect::<Vec<_>>(), vec![5, 6]);
    assert_eq!(
        Endpoint::ListOrgInvitationTeams.meta().permission,
        Some((Permission::Members, Access::Read))
    );
    mock.assert_exhausted();
}
