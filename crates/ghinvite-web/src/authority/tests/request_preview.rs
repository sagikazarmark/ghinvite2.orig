use super::*;
use ghinvite_core::bulk_preview::{MembershipRole, MembershipState, ObservedAccess, PreviewAction};

fn user(id: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/users/alice",
        json!({"id":id,"login":"alice"}),
    )
    .require_header("authorization", REPO_BEARER)
}
async fn view(state: &AppState) -> AuthorityView {
    AuthorityView::open(state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn preview_reads_are_numeric_bound_cached_across_views_and_share_the_authority_budget() {
    let (state, _, mock, limit) = fixture(
        vec![user(8), identity(1, 100), permission(1, 8, "read")],
        false,
    )
    .await;
    let first = view(&state)
        .await
        .request_preview(&targets(&[1]), 8, "alice")
        .await;
    assert_eq!(first.targets.len(), 1);
    assert_eq!(first.targets[0].action, PreviewAction::WillRaise);
    assert_eq!(
        first.targets[0].observed,
        ObservedAccess::Repository {
            role: ghinvite_core::targets::EffectiveRole::Read
        }
    );
    assert!(first.observed_at.unwrap() <= chrono::Utc::now());
    let second = view(&state)
        .await
        .request_preview(&targets(&[1]), 8, "alice")
        .await;
    assert_eq!(second.targets, first.targets);
    assert_eq!(second.observed_at, first.observed_at);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 3);
    mock.assert_exhausted();
}

#[tokio::test]
async fn preview_never_converts_wrong_user_repository_owner_or_permission_identity_into_access() {
    for mismatch in 0..4 {
        let mut script = vec![user(if mismatch == 0 { 99 } else { 8 })];
        if mismatch != 0 {
            let mut repo = identity(1, if mismatch == 2 { 999 } else { 100 });
            if mismatch == 1 {
                repo.response.body = serde_json::to_vec(
                    &json!({"id":999,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}}),
                )
                .unwrap();
            }
            script.push(repo);
        }
        if mismatch == 3 {
            script.push(permission(1, 99, "admin"));
        }
        let (state, _, mock, _) = fixture(script, false).await;
        let preview = view(&state)
            .await
            .request_preview(&targets(&[1]), 8, "alice")
            .await;
        assert_eq!(preview.targets.len(), 1);
        assert_eq!(
            preview.targets[0].observed,
            ObservedAccess::Unavailable,
            "case {mismatch}"
        );
        assert!(preview.observed_at.is_none());
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn preview_cannot_reset_per_request_budget_and_preserves_every_unavailable_target() {
    let (state, _, mock, limit) = fixture(vec![user(8)], false).await;
    let authority = view(&state).await;
    authority.state.lock().unwrap().reads = MAX_READS - 1;
    let result = authority
        .clone()
        .request_preview(&targets(&[1, 2]), 8, "alice")
        .await;
    assert_eq!(result.targets.len(), 2);
    assert!(
        result
            .targets
            .iter()
            .all(|p| p.observed == ObservedAccess::Unavailable)
    );
    assert_eq!(authority.state.lock().unwrap().reads, MAX_READS);
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
    mock.assert_exhausted();
}

#[tokio::test]
async fn preview_ignores_future_and_expired_cache_evidence() {
    for offset in [-300, 60] {
        let (mut state, _, mock, limit) = fixture(
            vec![user(8), identity(1, 100), permission(1, 8, "read")],
            false,
        )
        .await;
        state.authority_cache = Arc::new(StaleCache(CacheEntry {
            observed_at: chrono::Utc::now().timestamp() + offset,
            value: json!({"observed_at":chrono::Utc::now(),"value":{"id":999,"login":"alice"}}),
        }));
        let result = view(&state)
            .await
            .request_preview(&targets(&[1]), 8, "alice")
            .await;
        assert_eq!(result.targets[0].action, PreviewAction::WillRaise);
        assert_eq!(limit.calls.load(Ordering::SeqCst), 3);
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn preview_checks_org_team_and_user_before_using_identity_free_team_membership() {
    use ghinvite_core::targets::{MemberGrant, OrganizationTarget, TeamTarget};
    for wrong_team_org in [false, true] {
        let membership = |login: &str, id: u64, role: &str, state: &str| {
            Expectation::ok_json(
                Method::Get,
                format!("https://api.github.com/orgs/acme/memberships/{login}"),
                json!({"role":role,"state":state,"user":{"id":id,"login":login},"organization":{"id":100,"login":"acme"}}),
            )
        };
        let mut script = vec![
            membership("installer", 9, "admin", "active"),
            // Looked up with the organization app's token.
            user(8).require_header("authorization", "Bearer ghs_fixed_2_2_1"),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.com/orgs/acme",
                json!({"id":100,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
            ),
            membership("alice", 8, "member", "active"),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.com/orgs/acme/teams/build",
                json!({"id":5,"slug":"build","name":"Build","organization":{"id":if wrong_team_org {999} else {100},"login":"acme"}}),
            ),
        ];
        if !wrong_team_org {
            script.push(Expectation::ok_json(
                Method::Get,
                "https://api.github.com/orgs/acme/teams/build/memberships/alice",
                json!({"role":"maintainer","state":"pending"}),
            ));
        }
        let mut installation = Installation::repo(account());
        installation.app = App::Org;
        installation.account.installation_id = 2;
        let (state, _, mock, _) = fixture_with(script, false, installation).await;
        let targets = Targets {
            organization: Some(OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: MemberGrant::Member,
            }),
            teams: vec![TeamTarget {
                team_id: 5,
                team_slug: "build".into(),
                grant: MemberGrant::Member,
            }],
            repos: vec![],
        };
        let result = view(&state)
            .await
            .request_preview(&targets, 8, "alice")
            .await;
        assert_eq!(result.targets.len(), 2);
        assert_eq!(result.targets[0].action, PreviewAction::Satisfied);
        assert_eq!(
            result.targets[1].observed,
            if wrong_team_org {
                ObservedAccess::Unavailable
            } else {
                ObservedAccess::Membership {
                    state: MembershipState::Pending,
                    role: MembershipRole::Maintainer,
                }
            }
        );
        mock.assert_exhausted();
    }
}
