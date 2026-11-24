use super::*;
use ghinvite_github::{
    AppSlot, GithubApps, InstalledApp, Method,
    mocks::{self, Expectation, FixedTokens, MockTransport},
};
use ghinvite_storage_sqlx::SqlxStorage;
use serde_json::json;
use std::sync::Arc;

fn now() -> chrono::DateTime<chrono::Utc> {
    "2026-10-16T12:00:00Z".parse().unwrap()
}
fn command() -> CreateCommand {
    CreateCommand {
        batch: None,
        budget_attribution: Default::default(),
        access_lifetime_seconds: None,
        budget_share_pct: 50,
        invitation_id: Some(ghinvite_core::GithubInvitationId::new()),
        link_id: Some(ghinvite_core::InvitationLinkId::new()),
        request_id: ghinvite_core::RequestId::new(),
        delivery_generation: 1,
        approval_id: "approved".into(),
        account_id: 100,
        installation_id: 9,
        requester_id: 8,
        repo_id: 10,
        repo_full_name: "acme/api".into(),
        permission: ghinvite_core::Permission::Push,
        approved_at: now(),
    }
}
fn ready(role: EffectiveRole, pending: Option<PendingInvitation>) -> PreRead {
    PreRead::Ready {
        login: "alice".into(),
        role,
        pending,
    }
}
fn permission(role: &str) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/collaborators/alice/permission",
        json!({"user":{"id":8,"login":"alice"},"role_name":role}),
    )
}
fn put(status: u16) -> Expectation {
    let mut e = Expectation::ok_json(
        Method::Put,
        "https://api.github.test/repos/acme/api/collaborators/alice",
        if status == 201 {
            json!({"id":99,"permissions":"write"})
        } else {
            json!({"message":"rejected-fixture-marker"})
        },
    );
    e.response.status = status;
    if status == 204 {
        e.response.body.clear();
    }
    e.expected_body = Some(br#"{"permission":"push"}"#.to_vec());
    e
}
fn invitations(value: serde_json::Value) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100",
        value,
    )
}
fn repository() -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api",
        json!({"id":10,"full_name":"acme/api","private":true}),
    )
}
/// The repository app over `script`, with `FixedTokens` (App 123).
fn github(script: Vec<Expectation>) -> (GithubApp, FixedTokens, MockTransport) {
    let mock = MockTransport::scripted(script);
    let tokens = FixedTokens::new(123);
    let apps = GithubApps::for_tests(&mock, [(AppSlot::Repo, tokens.clone())]);
    (apps.app(AppSlot::Repo).unwrap(), tokens, mock)
}
fn github_transport(
    transport: Arc<dyn ghinvite_github::HttpTransportDyn>,
) -> (GithubApp, FixedTokens) {
    let tokens = FixedTokens::new(123);
    let env =
        ghinvite_github::rest::RestEnv::new(transport, mocks::API_BASE, "ghinvite-test").unwrap();
    let installed = InstalledApp {
        slot: AppSlot::Repo,
        app: mocks::app(123, mocks::UNREACHABLE_APP_BASE),
        user_agent: "p1/repo".into(),
    };
    let apps = GithubApps::with_sources(
        env,
        [(
            installed,
            Arc::new(tokens.clone()) as Arc<dyn ghinvite_github::TokenSource>,
        )],
    )
    .unwrap();
    (apps.app(AppSlot::Repo).unwrap(), tokens)
}
fn bearer(generation: u64) -> String {
    format!("Bearer {}", FixedTokens::secret(123, 9, generation))
}
/// Only `scope`'s tokens were used (`FixedTokens` secrets omit the scope).
fn used_only(tokens: &FixedTokens, scope: &octotoken::TokenScope) {
    for other in [&*profiles::REPO_READ, &*profiles::REPO_INVITE] {
        assert_eq!(
            tokens.generation(9, other) > 0,
            other == scope,
            "token scope used"
        );
    }
}
#[test]
fn role_matrix_never_lowers_effective_or_pending_grants() {
    let roles = [
        RepoRole::Read,
        RepoRole::Triage,
        RepoRole::Write,
        RepoRole::Maintain,
        RepoRole::Admin,
    ];
    for existing in roles {
        for target in roles {
            let outcome = plan(&ready(existing.into(), None), target);
            assert_eq!(
                matches!(
                    outcome,
                    Plan::Complete(CreateOutcome::AlreadySatisfied { .. })
                ),
                existing.rank() >= target.rank()
            );
            let outcome = plan(
                &ready(
                    EffectiveRole::None,
                    Some(PendingInvitation {
                        timing: None,
                        upstream_id: 99,
                        permission: existing.into(),
                    }),
                ),
                target,
            );
            assert_eq!(
                matches!(
                    outcome,
                    Plan::Complete(CreateOutcome::AlreadyPending { .. })
                ),
                existing.rank() >= target.rank()
            );
            assert_eq!(
                matches!(outcome, Plan::Patch { upstream_id: 99 }),
                existing.rank() < target.rank()
            );
        }
    }
    let custom = EffectiveRole::Custom(CustomRoleName::new("security-reviewer").unwrap());
    assert!(matches!(
        plan(&ready(custom, None), RepoRole::Write),
        Plan::Complete(CreateOutcome::SkippedCustomRole { .. })
    ));
}
#[tokio::test]
async fn pre_read_uses_read_profile_and_ignores_expired_and_other_identity_invites() {
    let mut script = vec![
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api",
            json!({"id":10,"full_name":"acme/api","private":true}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/user/8",
            json!({"id":8,"login":"alice"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/users/alice",
            json!({"id":8,"login":"alice"}),
        ),
        permission("read"),
    ];
    let invitation = |id, invitee, expired| json!({"id":id,"invitee":{"id":invitee,"login":"fixture"},"permissions":"admin","created_at":"2026-10-15T00:00:00Z","expired":expired});
    let mut page = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?per_page=100",
        json!([invitation(90, 8, true), invitation(91, 7, false)]),
    );
    page.response.headers.insert(
        "link".into(),
        "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=\"next\"".into(),
    );
    script.push(page);
    script.push(Expectation::ok_json(
        Method::Get,
        "https://api.github.test/repos/acme/api/invitations?page=2",
        json!([invitation(99, 8, false)]),
    ));
    let (app, tokens, mock) = github(script);
    let read = pre_read(&app, 9, &command(), now()).await;
    assert!(matches!(
        plan(&read, RepoRole::Write),
        Plan::Complete(CreateOutcome::AlreadyPending {
            upstream_id: 99,
            permission: RepoRole::Admin
        })
    ));
    used_only(&tokens, &profiles::REPO_READ);
    mock.assert_exhausted();
}
#[tokio::test]
async fn changed_role_is_observed_before_a_fence_or_write() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let command = command();
    let (app, tokens, mock) = github(vec![permission("maintain")]);
    assert_eq!(
        write(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::Read, None),
            now()
        )
        .await,
        CreateOutcome::AlreadySatisfied {
            effective_role: EffectiveRole::Maintain
        }
    );
    assert!(!storage.delivery_attempt_exists(&command).await.unwrap());
    used_only(&tokens, &profiles::REPO_INVITE);
    mock.assert_exhausted();
}
#[tokio::test]
async fn rejected_401_releases_then_reclaims_once() {
    for final_status in [201, 401] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let command = command();
        // The 401 invalidated the token: the retry, after the release,
        // writes with a new one.
        let (app, tokens, mock) = github(vec![
            permission("read").require_header("authorization", bearer(1)),
            put(401).require_header("authorization", bearer(1)),
            put(final_status).require_header("authorization", bearer(2)),
        ]);
        let outcome = write(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::Read, None),
            now(),
        )
        .await;
        if final_status == 201 {
            assert_eq!(outcome, CreateOutcome::Created { upstream_id: 99 });
            assert!(storage.delivery_attempt_exists(&command).await.unwrap());
            storage.reject_delivery_attempt(&command, 2).await.unwrap();
        } else {
            assert_eq!(outcome, blocked(BlockedReason::InstallationUnavailable));
            assert!(!storage.delivery_attempt_exists(&command).await.unwrap());
        }
        assert_eq!(
            storage.claim_delivery_attempt(&command).await.unwrap(),
            Some(3)
        );
        used_only(&tokens, &profiles::REPO_INVITE);
        // Two tokens either way: a last 401 invalidates without creating a
        // third (P1 spec §9.4: `call` invalidates, `ensure` re-mints).
        assert_eq!(tokens.generation(9, &profiles::REPO_INVITE), 2);
        assert_eq!(tokens.replace_calls(), 0);
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn ambiguous_write_is_never_repeated_across_stage_replays() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let command = command();
    let (app, tokens, mock) = github(vec![
        permission("read"),
        put(500),
        invitations(json!([])),
        repository(),
        invitations(json!([])),
        repository(),
    ]);
    for _ in 0..3 {
        assert_eq!(
            write(
                &app,
                9,
                &storage,
                &command,
                &ready(EffectiveRole::Read, None),
                now()
            )
            .await,
            CreateOutcome::OutcomeUnknown
        );
    }
    assert!(storage.delivery_attempt_exists(&command).await.unwrap());
    // One write token; the fenced recoveries read with the read scope.
    assert_eq!(tokens.generation(9, &profiles::REPO_INVITE), 1);
    assert_eq!(tokens.generation(9, &profiles::REPO_READ), 1);
    mock.assert_exhausted();
}
#[tokio::test]
async fn lost_success_recovers_only_positive_invitation_evidence_without_another_write() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let command = command();
    let invite = |invitee, permission, expired| json!({"id":99,"invitee":{"id":invitee,"login":"alice"},"permissions":permission,"created_at":"2026-10-15T00:00:00Z","expired":expired});
    let (app, _tokens, mock) = github(vec![
        permission("read"),
        put(201),
        invitations(json!([invite(7, "admin", false)])),
        repository(),
        invitations(json!([invite(8, "write", true)])),
        repository(),
        invitations(json!([invite(8, "read", false)])),
        repository(),
        invitations(json!([invite(8, "write", false)])),
        repository(),
    ]);
    for expected in [
        CreateOutcome::Created { upstream_id: 99 },
        CreateOutcome::OutcomeUnknown,
        CreateOutcome::OutcomeUnknown,
        CreateOutcome::OutcomeUnknown,
        CreateOutcome::Created { upstream_id: 99 },
    ] {
        assert_eq!(
            write(
                &app,
                9,
                &storage,
                &command,
                &ready(EffectiveRole::Read, None),
                now()
            )
            .await,
            expected
        );
    }
    assert!(storage.delivery_attempt_exists(&command).await.unwrap());
    mock.assert_exhausted();
}
#[tokio::test]
async fn lower_pending_is_patched_and_204_records_an_upgrade() {
    let mut patch = Expectation::ok_json(
        Method::Patch,
        "https://api.github.test/repos/acme/api/invitations/77",
        json!({"id":77,"invitee":{"id":8,"login":"alice"},"permissions":"write","created_at":"2026-10-15T00:00:00Z","expired":false}),
    );
    patch.expected_body = Some(br#"{"permissions":"write"}"#.to_vec());
    let (app, _tokens, mock) = github(vec![permission("read"), patch]);
    let storage = SqlxStorage::in_memory().await.unwrap();
    let read = ready(
        EffectiveRole::Read,
        Some(PendingInvitation {
            timing: None,
            upstream_id: 77,
            permission: EffectiveRole::Read,
        }),
    );
    assert_eq!(
        write(&app, 9, &storage, &command(), &read, now()).await,
        CreateOutcome::AlreadyPending {
            upstream_id: 77,
            permission: RepoRole::Write
        }
    );
    mock.assert_exhausted();
    let (app, _tokens, mock) = github(vec![permission("read"), put(204)]);
    let storage = SqlxStorage::in_memory().await.unwrap();
    assert_eq!(
        write(
            &app,
            9,
            &storage,
            &command(),
            &ready(EffectiveRole::Read, None),
            now()
        )
        .await,
        CreateOutcome::Upgraded {
            from: EffectiveRole::Read,
            to: RepoRole::Write
        }
    );
    mock.assert_exhausted();
}

/// A PUT's 201 for an invitation that appeared after S1 carries it at its
/// own role (GitHub keeps a pending invitation's role whatever the PUT
/// asks), so it is planned like a pending invitation found by S1.
#[tokio::test]
async fn raced_pending_invitation_in_a_put_201_is_planned_like_a_pending_one() {
    fn raced(permission: &str) -> Expectation {
        let mut e = put(201);
        e.response.body = serde_json::to_vec(&json!({"id":77,"permissions":permission})).unwrap();
        e
    }
    let mut patch = Expectation::ok_json(
        Method::Patch,
        "https://api.github.test/repos/acme/api/invitations/77",
        json!({"id":77,"invitee":{"id":8,"login":"alice"},"permissions":"write","created_at":"2026-10-15T00:00:00Z","expired":false}),
    );
    patch.expected_body = Some(br#"{"permissions":"write"}"#.to_vec());

    // Lower: the PUT changed nothing, so its generation is released and the
    // next one PATCHes the invitation.
    let storage = SqlxStorage::in_memory().await.unwrap();
    let command = command();
    let (app, _tokens, mock) = github(vec![permission("read"), raced("read"), patch.clone()]);
    let result = write_stage(
        &app,
        9,
        &storage,
        &command,
        &ready(EffectiveRole::Read, None),
        now(),
    )
    .await;
    assert_eq!(
        result.outcome,
        CreateOutcome::AlreadyPending {
            upstream_id: 77,
            permission: RepoRole::Write
        }
    );
    assert_eq!(result.rejected_generation, None);
    assert_eq!(result.timing.map(|t| t.upstream_id), Some(77));
    storage.reject_delivery_attempt(&command, 2).await.unwrap();
    assert_eq!(
        storage.claim_delivery_attempt(&command).await.unwrap(),
        Some(3)
    );
    mock.assert_exhausted();

    // After a 401 the reminted token PUTs, then PATCHes once more.
    let storage = SqlxStorage::in_memory().await.unwrap();
    let (app, tokens, mock) = github(vec![
        permission("read"),
        put(401).require_header("authorization", bearer(1)),
        raced("read").require_header("authorization", bearer(2)),
        patch.require_header("authorization", bearer(2)),
    ]);
    assert_eq!(
        write(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::Read, None),
            now()
        )
        .await,
        CreateOutcome::AlreadyPending {
            upstream_id: 77,
            permission: RepoRole::Write
        }
    );
    storage.reject_delivery_attempt(&command, 3).await.unwrap();
    assert_eq!(
        storage.claim_delivery_attempt(&command).await.unwrap(),
        Some(4)
    );
    assert_eq!(tokens.generation(9, &profiles::REPO_INVITE), 2);
    mock.assert_exhausted();

    // Higher or custom: never lowered, never PATCHed; the PUT's generation
    // is released after the outcome is journaled.
    for (role, expected) in [
        (
            "admin",
            CreateOutcome::AlreadyPending {
                upstream_id: 77,
                permission: RepoRole::Admin,
            },
        ),
        (
            "security-reviewer",
            CreateOutcome::SkippedCustomRole {
                role_name: CustomRoleName::new("security-reviewer").unwrap(),
            },
        ),
    ] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let (app, _tokens, mock) = github(vec![permission("read"), raced(role)]);
        let result = write_stage(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::Read, None),
            now(),
        )
        .await;
        assert_eq!(result.outcome, expected);
        assert_eq!(result.rejected_generation, Some(1));
        mock.assert_exhausted();
    }
}

async fn write(
    app: &GithubApp,
    installation: u64,
    storage: &dyn DeliveryStorage,
    command: &CreateCommand,
    read: &PreRead,
    now: chrono::DateTime<chrono::Utc>,
) -> CreateOutcome {
    let result = write_stage(app, installation, storage, command, read, now).await;
    if let Some(generation) = result.rejected_generation {
        storage
            .reject_delivery_attempt(command, generation)
            .await
            .unwrap();
    }
    result.outcome
}

#[tokio::test]
async fn positive_rejection_is_retained_before_generation_release() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let command = command();
    let (app, _tokens, mock) = github(vec![permission("read"), put(403)]);
    let result = write_stage(
        &app,
        9,
        &storage,
        &command,
        &ready(EffectiveRole::Read, None),
        now(),
    )
    .await;
    assert!(matches!(result.outcome, CreateOutcome::Blocked { .. }));
    assert_eq!(result.rejected_generation, Some(1));
    assert!(storage.delivery_attempt_exists(&command).await.unwrap());
    // Replaying S5's exact release is idempotent and never releases generation2.
    storage.reject_delivery_attempt(&command, 1).await.unwrap();
    storage.reject_delivery_attempt(&command, 1).await.unwrap();
    assert_eq!(
        storage.claim_delivery_attempt(&command).await.unwrap(),
        Some(2)
    );
    storage.reject_delivery_attempt(&command, 1).await.unwrap();
    assert!(storage.delivery_attempt_exists(&command).await.unwrap());
    mock.assert_exhausted();
}

#[tokio::test]
async fn policy_write_rejections_release_only_their_generation_after_journaling() {
    for (status, message, expected) in [
        (
            403,
            "Inviting outside collaborators is restricted by organization policy.",
            BlockedReason::OutsideCollaboratorsRestricted,
        ),
        (
            422,
            "Inviting outside collaborators is restricted by enterprise policy.",
            BlockedReason::OutsideCollaboratorsRestricted,
        ),
        (
            403,
            "Although you appear to have the correct authorization credentials, the `policy-marker` organization has an IP allow list enabled, and your IP address is not permitted to access this resource.",
            BlockedReason::IpAllowList,
        ),
        (
            403,
            "Outside collaborators may be restricted by organization policy.",
            BlockedReason::InstallationUnavailable,
        ),
    ] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let command = command();
        let mut refusal = put(status);
        refusal.response.body =
            serde_json::to_vec(&json!({"message":message,"private":"POLICY_SECRET_MARKER"}))
                .unwrap();
        let (app, _tokens, mock) = github(vec![permission("read"), refusal]);
        let result = write_stage(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::Read, None),
            now(),
        )
        .await;
        assert_eq!(result.outcome, blocked(expected));
        assert_eq!(result.rejected_generation, Some(1));
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("POLICY_SECRET_MARKER")
        );
        // S4 returns a retained refusal first; the owner releases after journaling.
        assert!(storage.delivery_attempt_exists(&command).await.unwrap());
        storage.reject_delivery_attempt(&command, 1).await.unwrap();
        assert_eq!(
            storage.claim_delivery_attempt(&command).await.unwrap(),
            Some(2)
        );
        storage.reject_delivery_attempt(&command, 1).await.unwrap();
        assert!(storage.delivery_attempt_exists(&command).await.unwrap());
        mock.assert_exhausted();
    }
}

// Historical InstallationClient assertions now exercise the production typed stages.
fn identity_reads(repo_id: u64, full_name: &str) -> Vec<Expectation> {
    vec![
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api",
            json!({"id":repo_id,"full_name":full_name,"private":true}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/user/8",
            json!({"id":8,"login":"alice"}),
        ),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/users/alice",
            json!({"id":8,"login":"alice"}),
        ),
        permission("none"),
        invitations(json!([])),
    ]
}

#[tokio::test]
async fn typed_pre_read_verifies_numeric_repository_including_rename() {
    for name in ["acme/api", "acme/api-renamed"] {
        let (app, _tokens, mock) = github(identity_reads(10, name));
        let read = pre_read(&app, 9, &command(), now()).await;
        assert!(matches!(plan(&read, RepoRole::Write), Plan::Put));
        mock.assert_exhausted();
    }
    let mut script = identity_reads(12, "acme/api");
    script.truncate(1);
    let (app, _tokens, mock) = github(script);
    let read = pre_read(&app, 9, &command(), now()).await;
    assert!(matches!(
        plan(&read, RepoRole::Write),
        Plan::Complete(CreateOutcome::Blocked {
            reason: BlockedReason::TargetUnavailable
        })
    ));
    mock.assert_exhausted();
}

#[tokio::test]
async fn typed_pre_read_failures_never_claim_a_write_fence() {
    for (status, wait) in [(404, None), (503, None), (403, Some(30_i64))] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let command = command();
        let mut failed = repository();
        failed.response.status = status;
        failed.response.body = br#"{"message":"unavailable"}"#.to_vec();
        if let Some(seconds) = wait {
            failed
                .response
                .headers
                .insert("retry-after".into(), seconds.to_string());
        }
        let (app, _tokens, mock) = github(vec![failed]);
        let read = pre_read(&app, 9, &command, now()).await;
        let result = write_stage(&app, 9, &storage, &command, &read, now()).await;
        match wait {
            Some(seconds) => assert_eq!(
                result.outcome,
                CreateOutcome::Throttled {
                    until: now() + chrono::Duration::seconds(seconds),
                    reason: None
                }
            ),
            None => assert_eq!(result.outcome, blocked(BlockedReason::TargetUnavailable)),
        }
        assert!(!storage.delivery_attempt_exists(&command).await.unwrap());
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn typed_transport_failure_is_unread_without_authorizing_a_write() {
    struct Unreachable;
    impl ghinvite_github::HttpTransport for Unreachable {
        async fn send(
            &self,
            _: ghinvite_github::Request,
        ) -> std::result::Result<ghinvite_github::Response, ghinvite_github::TransportError>
        {
            Err(ghinvite_github::TransportError::SentNoResponse { timed_out: false })
        }
    }
    let (app, tokens) = github_transport(Arc::new(Unreachable));
    let read = pre_read(&app, 9, &command(), now()).await;
    assert!(matches!(
        plan(&read, RepoRole::Write),
        Plan::Complete(CreateOutcome::Blocked {
            reason: BlockedReason::TargetUnavailable
        })
    ));
    used_only(&tokens, &profiles::REPO_READ);
}

#[tokio::test]
async fn typed_positive_throttle_and_permissions_release_only_rejected_writes() {
    for (status, wait, expected) in [
        (
            403,
            Some(45),
            CreateOutcome::Throttled {
                until: now() + chrono::Duration::seconds(45),
                reason: None,
            },
        ),
        (403, None, blocked(BlockedReason::InstallationUnavailable)),
        (502, None, CreateOutcome::OutcomeUnknown),
    ] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let command = command();
        let mut response = put(status);
        response.response.body =
            br#"{"message":"Resource not accessible by integration"}"#.to_vec();
        if let Some(wait) = wait {
            response
                .response
                .headers
                .insert("retry-after".into(), wait.to_string());
        }
        let (app, tokens, mock) = github(vec![permission("none"), response]);
        let result = write_stage(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::None, None),
            now(),
        )
        .await;
        assert_eq!(result.outcome, expected);
        assert!(storage.delivery_attempt_exists(&command).await.unwrap());
        if let Some(generation) = result.rejected_generation {
            assert_eq!(generation, 1);
            storage
                .reject_delivery_attempt(&command, generation)
                .await
                .unwrap();
            assert_eq!(
                storage.claim_delivery_attempt(&command).await.unwrap(),
                Some(2)
            );
        } else {
            assert_eq!(status, 502);
            assert_eq!(
                storage.claim_delivery_attempt(&command).await.unwrap(),
                None
            );
        }
        used_only(&tokens, &profiles::REPO_INVITE);
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn typed_failed_write_mint_never_claims_a_fence() {
    use octoauth::Error as GitHub;
    for (error, expected) in [
        (
            GitHub::Transient {
                kind: octoauth::TransientKind::Server { status: 502 },
            },
            blocked(BlockedReason::MintForbidden),
        ),
        (
            GitHub::PermissionsNotGranted,
            blocked(BlockedReason::InstallationPermissions),
        ),
        (
            GitHub::Forbidden {
                reason: octoauth::ForbiddenReason::IpAllowList,
            },
            blocked(BlockedReason::IpAllowList),
        ),
        (
            GitHub::InstallationSuspended,
            blocked(BlockedReason::MintForbidden),
        ),
        (
            GitHub::RateLimited {
                scope: octoauth::RateLimitScope::Secondary,
                retry_after: Some(std::time::Duration::from_secs(30)),
            },
            CreateOutcome::Throttled {
                until: now() + chrono::Duration::seconds(30),
                reason: None,
            },
        ),
    ] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let command = command();
        let (app, tokens, mock) = github(vec![]);
        tokens.fail_next_token(octotoken::TokenError::GitHub(error.clone()));
        let result = write_stage(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::None, None),
            now(),
        )
        .await;
        assert_eq!(result.outcome, expected, "{error:?}");
        assert_eq!(result.rejected_generation, None);
        assert_eq!(result.budget.effect, BudgetEffect::ReleaseUnused);
        assert_eq!(
            result.budget.limit.is_some(),
            matches!(error, GitHub::RateLimited { .. })
        );
        assert!(!storage.delivery_attempt_exists(&command).await.unwrap());
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn typed_fenced_recovery_requires_every_page_and_preserves_fence_on_failed_reads() {
    for final_status in [200, 403, 503] {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let command = command();
        assert_eq!(
            storage.claim_delivery_attempt(&command).await.unwrap(),
            Some(1)
        );
        let mut first = invitations(
            json!([{"id":1,"invitee":{"id":41,"login":"bob"},"permissions":"write","expired":false,"created_at":"2026-10-15T00:00:00Z"}]),
        );
        first.response.headers.insert(
            "link".into(),
            "<https://api.github.test/repos/acme/api/invitations?page=2>; rel=\"next\"".into(),
        );
        let mut second = Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?page=2",
            json!([{"id":99,"invitee":{"id":8,"login":"alice"},"permissions":"write","expired":false,"created_at":"2026-10-15T00:00:00Z"}]),
        );
        second.response.status = final_status;
        if final_status != 200 {
            second.response.body = br#"{"message":"read unavailable"}"#.to_vec();
            second
                .response
                .headers
                .insert("retry-after".into(), "90".into());
        }
        let mut script = vec![first, second];
        if final_status == 200 {
            script.push(repository());
        }
        let (app, tokens, mock) = github(script);
        let result = write_stage(
            &app,
            9,
            &storage,
            &command,
            &ready(EffectiveRole::None, None),
            now(),
        )
        .await;
        assert_eq!(
            result.outcome,
            if final_status == 200 {
                CreateOutcome::Created { upstream_id: 99 }
            } else {
                CreateOutcome::OutcomeUnknown
            }
        );
        assert_eq!(result.rejected_generation, None);
        assert_eq!(
            storage.claim_delivery_attempt(&command).await.unwrap(),
            None
        );
        // An existing fence permits observation only: no write token.
        used_only(&tokens, &profiles::REPO_READ);
        mock.assert_exhausted();
    }
}
