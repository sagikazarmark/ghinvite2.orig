use super::*;
use crate::{
    credential::{AppCredential, Credential, InstallationCredential},
    mocks::{self, Expectation, MockTransport},
    rest::RestEnv,
};
use serde_json::json;
use std::sync::Arc;

fn env(mock: &MockTransport) -> RestEnv {
    mocks::env(mock, "endpoint-contract")
}
fn installation() -> InstallationCredential {
    mocks::installation_credential(123, 9, "endpoint-fixture")
}
fn app() -> AppCredential {
    mocks::app_credential(123)
}
fn credential(kind: CredentialKind) -> Box<dyn Credential> {
    match kind {
        CredentialKind::Installation => Box::new(installation()),
        _ => Box::new(app()),
    }
}
fn invitation(id: u64) -> serde_json::Value {
    json!({"id":id,"invitee":{"id":8,"login":"alice"},"permissions":"read","created_at":"2026-01-01T00:00:00Z","expired":false})
}
const REPO: Repository<'static> = Repository {
    owner: "acme",
    name: "api",
};

#[tokio::test]
async fn collaborator_endpoint_preserves_only_typed_policy_refusals() {
    use crate::error::{ForbiddenReason, ValidationCode};
    for status in [403, 422] {
        let mut refusal = Expectation::ok_json(
            Method::Put,
            "https://api.github.test/repos/acme/api/collaborators/alice",
            json!({"message":"Inviting outside collaborators is restricted by organization policy.","private":"POLICY_SECRET_MARKER"}),
        );
        refusal.response.status = status;
        let mock = MockTransport::scripted(vec![refusal]);
        let env = env(&mock);
        let cred = installation();
        let error = put_collaborator(&Rest::new(&env, &cred), REPO, "alice", &RepoRole::Write)
            .await
            .unwrap_err();
        match error {
            ApiError::Forbidden { reason } => {
                assert_eq!(reason, ForbiddenReason::OutsideCollaboratorsRestricted)
            }
            ApiError::Validation { ref codes, .. } => {
                assert!(codes.contains(&ValidationCode::OutsideCollaboratorsRestricted))
            }
            _ => panic!("unexpected policy classification: {error:?}"),
        }
        assert!(!format!("{error} {error:?}").contains("POLICY_SECRET_MARKER"));
        mock.assert_exhausted();
    }
}

#[test]
fn all_endpoint_metadata_is_explicit() {
    let mut names = std::collections::BTreeSet::new();
    for endpoint in Endpoint::all() {
        assert!(names.insert(format!("{endpoint:?}")));
        let meta = endpoint.meta();
        assert!(!meta.credential.is_empty());
        assert_eq!(
            meta.permission.is_none(),
            endpoint.permission_exemption().is_some(),
            "{endpoint:?}"
        );
        assert!(!endpoint.retry_reason().is_empty());
        assert_eq!(
            meta.retry_safe,
            !matches!(
                endpoint,
                Endpoint::UpdateRepoInvitation | Endpoint::CreateOrgInvitation
            )
        );
        assert!(!meta.paginated || meta.method == Method::Get);
        assert_eq!(
            meta.content_creating,
            matches!(
                endpoint,
                Endpoint::PutCollaborator
                    | Endpoint::UpdateRepoInvitation
                    | Endpoint::DeleteRepoInvitation
                    | Endpoint::RemoveCollaborator
                    | Endpoint::CreateOrgInvitation
                    | Endpoint::CancelOrgInvitation
                    | Endpoint::RemoveOrgMember
                    | Endpoint::PutTeamMembership
                    | Endpoint::RemoveTeamMembership
            )
        );
        assert_eq!(meta.api_version, None);
        let path = meta.path.seg("o").seg("r");
        // Repository root writes have no variant (generic deny-list tested in rest).
        if matches!(meta.method, Method::Delete | Method::Patch) {
            assert_ne!(path.build().ok().as_deref(), Some("/repos/o/r"));
        }
    }
    // gh-toolkit's 44 endpoints less the user-token, token-management and
    // pull-request ones ghinvite does not call.
    assert_eq!(names.len(), 26);
    for endpoint in [
        Endpoint::GetInstallation,
        Endpoint::ListInstallationRequests,
    ] {
        assert_eq!(endpoint.meta().credential, &[CredentialKind::AppJwt]);
    }
    assert!(!ApiError::Ambiguous.is_transient_for(&Endpoint::UpdateRepoInvitation.meta()));
    assert!(ApiError::Ambiguous.is_transient_for(&Endpoint::PutCollaborator.meta()));
}

#[test]
fn permissions_map_onto_octoauth_scopes() {
    for (permission, key) in [
        (Permission::Administration, "administration"),
        (Permission::Members, "members"),
        (Permission::Metadata, "metadata"),
    ] {
        assert_eq!(permission.as_str(), key);
        assert_eq!(octoauth::Permission::from(permission).as_str(), key);
    }
    for access in [Access::Read, Access::Write, Access::Admin] {
        assert_eq!(octoauth::Access::from(access).as_str(), access.as_str());
    }
}
#[test]
// covers: TR-13
fn direct_collaborator_permission_metadata_unverified() {
    assert_eq!(
        Endpoint::ListCollaborators.meta().permission,
        Some((Permission::Metadata, Access::Read))
    );
}
#[test]
fn numeric_user_lookup_path_unverified() {
    assert_eq!(
        Endpoint::GetUserById.meta().path.seg("8").build().unwrap(),
        "/user/8"
    );
}
#[test]
// covers: TR-15
fn every_endpoint_rejects_wrong_credentials_before_transport() {
    let mock = MockTransport::scripted(vec![]);
    let env = env(&mock);
    for endpoint in Endpoint::all() {
        for kind in [CredentialKind::AppJwt, CredentialKind::Installation] {
            let cred = credential(kind);
            let rest = Rest::new(&env, &*cred);
            assert_eq!(
                endpoint.authorize(&rest).is_ok(),
                endpoint.meta().credential.contains(&kind)
            );
        }
    }
    mock.assert_exhausted();
}
#[tokio::test]
async fn role_name_wins_over_collapsed_permission_and_custom_is_unrankable() {
    for (name, expected) in [
        ("maintain", RepoRole::Maintain),
        (
            "security-auditor",
            RepoRole::Custom("security-auditor".into()),
        ),
    ] {
        let mock = MockTransport::scripted(vec![Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/collaborators/alice/permission",
            json!({"user":{"id":8,"login":"alice"},"role_name":name,"permission":"write"}),
        )]);
        let env = env(&mock);
        let cred = installation();
        assert_eq!(
            collaborator_permission(&Rest::new(&env, &cred), REPO, "alice")
                .await
                .unwrap()
                .role,
            expected
        );
        mock.assert_exhausted();
    }
    let mock = MockTransport::scripted(vec![Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/collaborators/alice/permission",
        json!({"user":{"id":8,"login":"alice"},"permission":"admin"}),
    )]);
    let env = env(&mock);
    let cred = installation();
    assert!(matches!(
        collaborator_permission(&Rest::new(&env, &cred), REPO, "alice").await,
        Err(ApiError::Decode { .. })
    ));
}
#[tokio::test]
// covers: TR-12
async fn put_status_and_patch_wire_roles_are_distinct() {
    let mut created = Expectation::ok_json(
        Method::Put,
        "https://api.github.test/repos/acme/api/collaborators/alice",
        json!({"id":99,"permissions":"write"}),
    )
    .require_body(br#"{"permission":"push"}"#.to_vec());
    created.response.status = 201;
    // A pending invitation keeps its own role: the 201 reports it.
    let mut existing = Expectation::ok_json(
        Method::Put,
        "https://api.github.test/repos/acme/api/collaborators/alice",
        json!({"id":98,"permissions":"read"}),
    )
    .require_body(br#"{"permission":"push"}"#.to_vec());
    existing.response.status = 201;
    let mut unreported = Expectation::ok_json(
        Method::Put,
        "https://api.github.test/repos/acme/api/collaborators/alice",
        json!({"id":97}),
    );
    unreported.response.status = 201;
    let mock = MockTransport::scripted(vec![
        created,
        existing,
        unreported,
        Expectation::status(
            Method::Put,
            "https://api.github.test/repos/acme/api/collaborators/alice",
            204,
        )
        .require_body(br#"{"permission":"pull"}"#.to_vec()),
        Expectation::ok_json(
            Method::Patch,
            "https://api.github.test/repos/acme/api/invitations/99",
            {
                let mut value = invitation(99);
                value["permissions"] = json!("write");
                value
            },
        )
        .require_body(br#"{"permissions":"write"}"#.to_vec()),
    ]);
    let env = env(&mock);
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    assert_eq!(
        put_collaborator(&rest, REPO, "alice", &RepoRole::Write)
            .await
            .unwrap(),
        CollaboratorWrite::Created {
            invitation_id: 99,
            created_at: None,
            permission: RepoRole::Write,
        }
    );
    assert_eq!(
        put_collaborator(&rest, REPO, "alice", &RepoRole::Write)
            .await
            .unwrap(),
        CollaboratorWrite::Created {
            invitation_id: 98,
            created_at: None,
            permission: RepoRole::Read,
        }
    );
    // Without its role a 201 proves no grant.
    assert_eq!(
        put_collaborator(&rest, REPO, "alice", &RepoRole::Write).await,
        Err(ApiError::Decode {
            what: "invitation_permission"
        })
    );
    assert_eq!(
        put_collaborator(&rest, REPO, "alice", &RepoRole::Read)
            .await
            .unwrap(),
        CollaboratorWrite::NoContent
    );
    assert_eq!(
        update_repo_invitation(&rest, REPO, 99, &RepoRole::Write)
            .await
            .unwrap()
            .permissions,
        RepoRole::Write
    );
    for role in [
        RepoRole::None,
        RepoRole::Custom("organization-admin".into()),
    ] {
        assert!(put_collaborator(&rest, REPO, "alice", &role).await.is_err());
        assert!(
            update_repo_invitation(&rest, REPO, 99, &role)
                .await
                .is_err()
        );
    }
    mock.assert_exhausted();
}
#[tokio::test]
async fn invitations_walk_101_items_and_keep_expired_and_unknown_roles() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100",
        json!((1..=100).map(invitation).collect::<Vec<_>>()),
    );
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.test/repos/acme/api/invitations?per_page=100&page=2>; rel=\"next\""
            .into(),
    );
    let mut last = invitation(101);
    last["expired"] = json!(true);
    last["permissions"] = json!("custom-auditor");
    let mock = MockTransport::scripted(vec![
        first,
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
            json!([last]),
        ),
    ]);
    let env = env(&mock);
    let cred = installation();
    let all = list_repo_invitations(&Rest::new(&env, &cred), REPO)
        .await
        .unwrap();
    assert_eq!(all.len(), 101);
    assert!(all[100].expired);
    assert_eq!(all[100].permissions.rank(), None);
    mock.assert_exhausted();
}
#[tokio::test]
// covers: PG-01 PG-02
async fn later_page_failure_is_error_and_streaming_never_claims_completion() {
    for streaming in [false, true] {
        let mut first = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            json!([invitation(1)]),
        );
        first.response.headers.insert(
            "link".into(),
            "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=next".into(),
        );
        let mock = MockTransport::scripted(vec![
            first,
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?page=2",
                503,
            ),
        ]);
        let env = env(&mock);
        let cred = installation();
        let rest = Rest::new(&env, &cred);
        if streaming {
            let mut count = 0;
            let result = rest
                .for_each_page::<RepoInvitation>(
                    Endpoint::ListRepoInvitations,
                    REPO.path(Endpoint::ListRepoInvitations),
                    &[],
                    |page| {
                        count += page.len();
                        Ok(())
                    },
                )
                .await;
            assert!(matches!(result, Err(ApiError::Server { .. })));
            assert_eq!(count, 1);
        } else {
            assert!(matches!(
                list_repo_invitations(&rest, REPO).await,
                Err(ApiError::Server { .. })
            ));
        }
        mock.assert_exhausted();
    }
}
#[tokio::test]
// covers: TR-06 PG-01
async fn untrusted_links_and_cycles_never_yield_a_successful_prefix() {
    for link in [
        "<https://evil.test/repos/acme/api/invitations?page=2>; rel=next",
        "<https://api.github.test/user/emails?page=2>; rel=next",
        "<https://api.github.test/repos/acme/api/invitations?per_page=100>; rel=next",
        "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=next, <broken",
        "<https://api.github.test/repos/acme/api/invitations?page=2#secret>; rel=next",
    ] {
        let mut first = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            json!([invitation(1)]),
        );
        first.response.headers.insert("link".into(), link.into());
        let mock = MockTransport::scripted(vec![first]);
        let env = env(&mock);
        let cred = installation();
        assert!(
            list_repo_invitations(&Rest::new(&env, &cred), REPO)
                .await
                .is_err(),
            "{link}"
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn sso_partial_page_fails_closed_and_retains_numeric_org_ids_only() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100",
        json!([invitation(1)]),
    );
    first.response.headers.insert(
        "x-github-sso".into(),
        "partial-results; organizations=12,secret,34".into(),
    );
    let mock = MockTransport::scripted(vec![first]);
    let env = env(&mock);
    let cred = installation();
    assert!(
        matches!(list_repo_invitations(&Rest::new(&env,&cred),REPO).await,Err(ApiError::Partial{org_ids,..}) if org_ids.len()==2)
    );
}
#[tokio::test]
async fn installation_repository_envelope_walks_every_page() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/installation/repositories?per_page=100",
        json!({"total_count":2,"repositories":[{"id":1,"full_name":"acme/a","private":true}]}),
    );
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.test/installation/repositories?page=2>; rel=next".into(),
    );
    let second = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/installation/repositories?page=2",
        json!({"total_count":2,"repositories":[{"id":2,"full_name":"acme/b","private":true}]}),
    );
    let mock = MockTransport::scripted(vec![first, second]);
    let env = env(&mock);
    let cred = installation();
    let all = list_installation_repos(&Rest::new(&env, &cred))
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    mock.assert_exhausted();
}

#[tokio::test]
async fn installation_repository_count_prevents_partial_scope_without_next_link() {
    let mock = MockTransport::scripted(vec![Expectation::ok_json(
        Method::Get,
        "https://api.github.test/installation/repositories?per_page=100",
        json!({"total_count":101,"repositories":[{"id":1,"full_name":"acme/a","private":true}]}),
    )]);
    let env = env(&mock);
    let cred = installation();
    assert!(matches!(
        list_installation_repos(&Rest::new(&env, &cred)).await,
        Err(ApiError::Decode {
            what: "incomplete_installation_repositories"
        })
    ));
    mock.assert_exhausted();
}

#[tokio::test]
async fn duplicate_repository_entries_cannot_satisfy_the_reported_total() {
    let repo = json!({"id":1,"full_name":"acme/a","private":true});
    let mock = MockTransport::scripted(vec![Expectation::ok_json(
        Method::Get,
        "https://api.github.test/installation/repositories?per_page=100",
        json!({"total_count":2,"repositories":[repo.clone(),repo]}),
    )]);
    let env = env(&mock);
    let cred = installation();
    assert!(matches!(
        list_installation_repos(&Rest::new(&env, &cred)).await,
        Err(ApiError::Decode {
            what: "duplicate_installation_repository"
        })
    ));
    mock.assert_exhausted();
}

#[tokio::test]
async fn typed_endpoint_sends_an_octoauth_installation_token() {
    let lookup = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/users/alice",
        json!({"id":8,"login":"alice"}),
    )
    .require_header("authorization", "Bearer opaque-fixture")
    .require_header("accept", "application/vnd.github+json")
    .require_header("x-github-api-version", "2022-11-28")
    .require_header("user-agent", "endpoint-contract");
    let list = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100",
        json!([invitation(99)]),
    )
    .require_header("authorization", "Bearer opaque-fixture")
    .require_header("user-agent", "endpoint-contract");
    let mock = MockTransport::scripted(vec![lookup, list]);
    let env = env(&mock);
    let token = mocks::installation_token(123, 9, "opaque-fixture");
    let credential = InstallationCredential::new(token);
    let rest = Rest::new(&env, &credential);
    assert_eq!(
        rest.bucket(),
        crate::rest::QuotaBucket::Installation {
            app: crate::ids::AppId::new(123),
            installation: crate::ids::InstallationId::new(9),
        }
    );
    let user = get_user_by_login(&rest, "alice").await.unwrap();
    let invitations = list_repo_invitations(&rest, REPO).await.unwrap();
    assert_eq!(invitations.len(), 1);
    assert_eq!(user.id, 8);
    mock.assert_exhausted();
}

#[tokio::test]
async fn app_endpoints_send_the_app_credential_and_refuse_installation_tokens() {
    let installation_row = json!({
        "id":77,"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "account":{"id":100,"login":"acme","type":"Organization"},
        "repository_selection":"all","permissions":{"metadata":"read"},"suspended_at":null
    });
    let credential = app();
    let jwt = crate::credential::sealed::Sealed::authorization(&credential)
        .value
        .to_owned();
    let mock = MockTransport::scripted(vec![
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/app/installations/77",
            installation_row,
        )
        .require_header("authorization", format!("Bearer {jwt}")),
    ]);
    let env = env(&mock);
    let installation = get_installation(&Rest::new(&env, &credential), 77)
        .await
        .unwrap();
    assert_eq!(installation.id, 77);
    assert_eq!(installation.app_id, 123);
    let token = self::installation();
    assert!(matches!(
        get_installation(&Rest::new(&env, &token), 77).await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
    assert!(matches!(
        list_installation_requests(&Rest::new(&env, &token)).await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
    mock.assert_exhausted();
}

#[tokio::test]
async fn public_endpoint_wrong_credential_does_not_send() {
    let mock = MockTransport::scripted(vec![]);
    let env = env(&mock);
    let cred = app();
    assert!(matches!(
        put_collaborator(&Rest::new(&env, &cred), REPO, "alice", &RepoRole::Write).await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
    mock.assert_exhausted();
}

#[tokio::test]
async fn malformed_link_after_a_next_relation_is_not_silently_truncated() {
    for link in [
        "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=\"next\" garbage",
        "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=\"next\", garbage",
        "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=\"next\", <https://api.github.test/repos/acme/api/invitations?page=3>; rel=next",
    ] {
        let mut page = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            json!([invitation(1)]),
        );
        page.response.headers.insert("link".into(), link.into());
        let mock = MockTransport::scripted(vec![page]);
        let env = env(&mock);
        let cred = installation();
        assert!(
            list_repo_invitations(&Rest::new(&env, &cred), REPO)
                .await
                .is_err()
        );
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn repository_identity_endpoint_uses_installation_metadata_read() {
    let meta = Endpoint::GetRepository.meta();
    assert_eq!(meta.method, Method::Get);
    assert_eq!(meta.permission, Some((Permission::Metadata, Access::Read)));
    assert_eq!(meta.credential, &[CredentialKind::Installation]);
    let mock = MockTransport::scripted(vec![Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api",
        json!({"id":10,"full_name":"acme/api","private":true}),
    )]);
    let env = env(&mock);
    let cred = installation();
    let repo = get_repository(&Rest::new(&env, &cred), REPO).await.unwrap();
    assert_eq!(repo.id, 10);
    assert_eq!(repo.full_name, "acme/api");
    mock.assert_exhausted();
}

#[tokio::test]
async fn authority_page_cursor_never_changes_endpoint_and_membership_is_installation_only() {
    use super::authority::*;
    let mut page = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/installation/repositories?per_page=100",
        json!({"total_count":2,"repositories":[{"id":1,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}}]}),
    );
    page.response.headers.insert(
        "link".into(),
        "<https://api.github.test/installation/repositories?per_page=100&page=2>; rel=\"next\""
            .into(),
    );
    let mock = MockTransport::scripted(vec![
        page,
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/installation/repositories?per_page=100&page=2",
            json!({"total_count":2,"repositories":[{"id":2,"full_name":"acme/r2","owner":{"id":100,"login":"acme"}}]}),
        ),
    ]);
    let env = env(&mock);
    let cred = installation();
    let rest = Rest::new(&env, &cred);
    let first = installation_repository_page(&rest, None).await.unwrap();
    assert_eq!(first.repositories[0].owner.id, 100);
    let second = installation_repository_page(&rest, first.next.as_ref())
        .await
        .unwrap();
    assert!(second.next.is_none());
    mock.assert_exhausted();
    for path in [
        "/user?page=2",
        "/installation/repositories?page=0",
        "/installation/repositories?page=2&page=3",
        "/installation/repositories?access_token=bad",
        "/installation/repositories?page=2#secret",
    ] {
        let cursor = serde_json::from_value(json!(path)).unwrap();
        assert!(
            installation_repository_page(&rest, Some(&cursor))
                .await
                .is_err()
        );
    }
    let user = app();
    assert!(matches!(
        organization_membership(&Rest::new(&env, &user), "acme", "alice").await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
}

mod organization_contracts;

#[tokio::test]
async fn endpoint_api_version_override_is_request_local_and_preserves_credentials() {
    const OVERRIDE: &str = "2099-01-01"; // Synthetic contract fixture, not a live endpoint version.
    let mock = MockTransport::scripted(
        ["2022-11-28", OVERRIDE, "2022-11-28"]
            .into_iter()
            .map(|version| {
                Expectation::ok_json(
                    Method::Get,
                    "https://api.github.test/users/alice",
                    json!({"id":8,"login":"alice"}),
                )
                .require_header("x-github-api-version", version)
                .require_header("accept", "application/vnd.github+json")
                .require_header("user-agent", "endpoint-contract")
                .require_header("authorization", "Bearer endpoint-fixture")
            })
            .collect(),
    );
    let env = env(&mock);
    let credential = installation();
    let rest = Rest::new(&env, &credential);
    get_user_by_login(&rest, "alice").await.unwrap();
    let mut meta = Endpoint::GetUserByLogin.meta();
    meta.api_version = Some(OVERRIDE);
    let response: ApiResponse<serde_json::Value> =
        call_meta::<(), _>(&rest, meta, &["alice"], None)
            .await
            .unwrap();
    assert_eq!(response.body["id"], 8);
    // Public generic calls keep their default, even after an overridden typed call.
    rest.call::<(), serde_json::Value>(
        Method::Get,
        PathTemplate::new("/users/{}").seg("alice"),
        &[],
        None,
    )
    .await
    .unwrap();
    mock.assert_exhausted();
}

#[tokio::test]
// covers: TR-05
async fn endpoint_api_version_override_applies_to_every_validated_page() {
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100",
        json!([invitation(99)]),
    )
    .require_header("x-github-api-version", "2099-01-01")
    .require_header("authorization", "Bearer endpoint-fixture");
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.test/repos/acme/api/invitations?per_page=100&page=2>; rel=\"next\""
            .into(),
    );
    let second = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
        json!([invitation(100)]),
    )
    .require_header("x-github-api-version", "2099-01-01")
    .require_header("authorization", "Bearer endpoint-fixture");
    let mock = MockTransport::scripted(vec![first, second]);
    let env = env(&mock);
    let credential = installation();
    let rest = Rest::new(&env, &credential);
    let mut meta = Endpoint::ListRepoInvitations.meta();
    meta.api_version = Some("2099-01-01");
    let mut ids = vec![];
    rest.walk_pages_meta(
        meta,
        REPO.path(Endpoint::ListRepoInvitations),
        &[],
        |response| {
            let page: Vec<serde_json::Value> = serde_json::from_slice(&response.body).unwrap();
            ids.extend(page.into_iter().map(|item| item["id"].as_u64().unwrap()));
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(ids, [99, 100]);
    mock.assert_exhausted();
}

#[tokio::test]
async fn endpoint_api_version_override_does_not_bypass_credential_validation() {
    let mock = MockTransport::scripted(vec![]);
    let env = env(&mock);
    let credential = installation();
    let installation_rest = Rest::new(&env, &credential);
    let mut meta = Endpoint::GetInstallation.meta();
    meta.api_version = Some("2099-01-01");
    assert!(matches!(
        call_meta::<(), serde_json::Value>(&installation_rest, meta, &["9"], None).await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
    let credential = app();
    let rest = Rest::new(&env, &credential);
    let mut meta = Endpoint::ListInstallationRepos.meta();
    meta.api_version = Some("2099-01-01");
    assert!(matches!(
        rest.walk_pages_meta(
            meta,
            PathTemplate::new("/installation/repositories"),
            &[],
            |_| Ok(())
        )
        .await,
        Err(ApiError::Forbidden {
            reason: ForbiddenReason::WrongCredential
        })
    ));
    mock.assert_exhausted();
}

#[tokio::test]
async fn unknown_membership_roles_and_states_fail_closed_without_upstream_text() {
    for is_team in [false, true] {
        for field in ["role", "state"] {
            let mut body = json!({"role":"member","state":"active","user":{"id":8,"login":"alice"},"organization":{"id":9,"login":"acme"}});
            body[field] = json!("FUTURE_ROLE_PRIVATE_MARKER");
            let path = if is_team {
                "/orgs/acme/teams/team/memberships/alice"
            } else {
                "/orgs/acme/memberships/alice"
            };
            let mock = MockTransport::scripted(vec![Expectation::ok_json(
                Method::Get,
                format!("https://api.github.test{path}"),
                body,
            )]);
            let env = env(&mock);
            let credential = installation();
            let rest = Rest::new(&env, &credential);
            let error = if is_team {
                organization::team_membership(&rest, "acme", "team", "alice")
                    .await
                    .unwrap_err()
            } else {
                organization::organization_membership(&rest, "acme", "alice")
                    .await
                    .unwrap_err()
            };
            assert_eq!(error, ApiError::Decode { what: "shape" });
            assert!(!format!("{error:?} {error}").contains("FUTURE_ROLE_PRIVATE_MARKER"));
            mock.assert_exhausted();
        }
    }
}

mod partial;

#[tokio::test]
async fn invitation_numeric_alias_requires_consistent_first_page_repository_evidence() {
    fn bound(id: u64, name: Option<&str>) -> serde_json::Value {
        let mut row = invitation(1);
        row["repository"] = json!({"id":id});
        if let Some(name) = name {
            row["repository"]["full_name"] = json!(name);
        }
        row
    }
    for body in [
        json!([invitation(1)]),
        json!([]),
        json!([bound(0, None)]),
        json!([bound(77, None), bound(78, None)]),
        json!([bound(77, None), invitation(2)]),
        json!([bound(77, Some("other/repository"))]),
        json!([bound(78, Some("acme/api"))]),
    ] {
        let mut first = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            body,
        );
        first.response.headers.insert(
            "link".into(),
            "<https://api.github.test/repositories/77/invitations?page=2>; rel=\"next\"".into(),
        );
        let mock = MockTransport::scripted(vec![first]);
        let environment = env(&mock);
        let cred = installation();
        assert!(matches!(
            list_repo_invitations(&Rest::new(&environment, &cred), REPO).await,
            Err(ApiError::Decode { what: "pagination" })
        ));
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn invitation_numeric_alias_is_pinned_and_partial_metadata_survives() {
    for changed in [false, true] {
        let mut row = invitation(1);
        row["repository"] = json!({"id":77,"full_name":"Acme/API"});
        let mut first = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            json!([row]),
        );
        first.response.headers.insert(
            "link".into(),
            "<https://api.github.test/repositories/77/invitations?page=2>; rel=\"next\"".into(),
        );
        first.response.headers.insert(
            "x-github-sso".into(),
            "partial-results; organizations=5".into(),
        );
        let mut second = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repositories/77/invitations?page=2",
            json!([invitation(2)]),
        );
        if changed {
            second.response.headers.insert(
                "link".into(),
                "<https://api.github.test/repositories/78/invitations?page=3>; rel=\"next\"".into(),
            );
        }
        let mock = MockTransport::scripted(vec![first, second]);
        let environment = env(&mock);
        let cred = installation();
        let result = Rest::new(&environment, &cred)
            .allow_partial()
            .list_all::<RepoInvitation>(
                Endpoint::ListRepoInvitations,
                REPO.path(Endpoint::ListRepoInvitations),
                &[],
            )
            .await;
        if changed {
            assert!(matches!(
                result,
                Err(ApiError::Decode { what: "pagination" })
            ));
        } else {
            let result = result.unwrap();
            assert_eq!(result.items.len(), 2);
            assert_eq!(result.partial.unwrap().org_ids, [crate::ids::OrgId::new(5)]);
        }
        mock.assert_exhausted();
    }
}

mod stub_contract;

#[cfg(not(target_arch = "wasm32"))]
mod http_contract;
mod wire_contract;
#[cfg(not(target_arch = "wasm32"))]
mod wire_router;
