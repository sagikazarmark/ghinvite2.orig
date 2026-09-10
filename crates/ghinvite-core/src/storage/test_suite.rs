//! Cross-cutting Storage behavior tests, parameterized over any `Storage` impl.
//! Per-impl test files (e.g. `ghinvite-storage-sqlx/tests/sqlx_suite.rs`)
//! call `run_suite(make_storage)`
//! with a factory so each scenario gets a fresh database.

use super::{GithubInvitationUpdate, RequestDecision, Storage};
use crate::audit::{ActorKind, AuditEvent, EventType, TargetKind};
use crate::{
    Account, AccountType, AuditEventId, GithubInvitation, GithubInvitationId, InvitationLink,
    InvitationLinkId, InvitationLinkRepo, InvitationRequest, InvitationState, Permission,
    RequestId, RequestState, SelectedRepos, Slug, User,
};
use chrono::{DateTime, Utc};

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn slug_with_seed(seed: u64) -> Slug {
    use rand::SeedableRng;
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
    Slug::generate(&mut rng)
}

fn sample_account(installation_id: u64, account_id: u64, login: &str) -> Account {
    Account {
        installation_id,
        account_id,
        account_login: login.into(),
        account_type: AccountType::Organization,
        installed_at: dt("2026-05-04T12:00:00Z"),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

fn sample_user(user_id: u64, login: &str) -> User {
    User {
        user_id,
        login: login.into(),
        avatar_url: None,
        last_seen_at: dt("2026-05-04T12:00:00Z"),
    }
}

fn sample_link(
    account_id: u64,
    installation_id: u64,
    created_by: u64,
    slug_seed: u64,
) -> InvitationLink {
    InvitationLink {
        id: InvitationLinkId::new(),
        slug: slug_with_seed(slug_seed),
        installation_id,
        account_id,
        created_by,
        created_at: dt("2026-05-04T12:00:00Z"),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: false,
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "acme/api".into(),
        }],
    }
}

fn sample_request(link: InvitationLinkId, requester: u64) -> InvitationRequest {
    InvitationRequest {
        id: RequestId::new(),
        invitation_link_id: link,
        requester_id: requester,
        justification: None,
        state: RequestState::Pending,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    }
}

/// Run the full cross-cutting suite against any `Storage` impl.
///
/// `make_storage` is invoked once per scenario so every scenario starts against
/// a fresh database — scenarios can use simple IDs without worrying about
/// cross-scenario collisions.
pub async fn run_suite<S, F, Fut>(make_storage: F)
where
    S: Storage,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = S>,
{
    scenario_install_uninstall_reinstall(make_storage().await).await;
    scenario_invitation_link_lifecycle(make_storage().await).await;
    scenario_invitation_link_metadata(make_storage().await).await;
    scenario_request_uses_and_uniqueness(make_storage().await).await;
    scenario_request_decision(make_storage().await).await;
    scenario_github_invitation_lifecycle(make_storage().await).await;
    scenario_audit_appends(make_storage().await).await;
    scenario_audit_pages(make_storage().await).await;
    scenario_timestamp_precision(make_storage().await).await;
}

/// Account history survives installation changes and missing actors/resources;
/// filtering happens before the bounded page, with exclusive nanosecond/ID seeks.
pub async fn scenario_audit_pages<S: Storage>(s: S) {
    use super::{AuditBoundary, AuditPosition};
    assert!(
        s.list_audit_events(9001, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    let mut expected = Vec::new();
    for n in 0..76 {
        let event = AuditEvent {
            id: AuditEventId::from_ulid(ulid::Ulid::from(n + 1)),
            account_id: if n == 75 { 9002 } else { 9001 },
            occurred_at: dt("2026-05-04T12:00:00.123456789Z")
                + chrono::Duration::nanoseconds((n / 3) as i64),
            event_type: if n < 50 {
                EventType::RequestApproved
            } else {
                EventType::RequestCreated
            },
            actor_kind: ActorKind::User,
            actor_id: Some(999),
            target_kind: TargetKind::Installation,
            target_id: "old-installation".into(),
            metadata: serde_json::Value::Null,
            request_id: None,
        };
        s.audit(&event).await.unwrap();
        if n < 50 {
            expected.push(event);
        }
    }
    expected.reverse();
    let filter = Some(EventType::RequestApproved);
    let first = s
        .list_audit_events(9001, filter, AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(first.events, expected[..25]);
    assert!(first.has_older);
    assert!(!first.has_newer);
    let older = AuditPosition::Before(AuditBoundary::from(first.events.last().unwrap()));
    let second = s.list_audit_events(9001, filter, older).await.unwrap();
    assert_eq!(second.events, expected[25..]);
    assert!(!second.has_older);
    assert!(second.has_newer);
    let newer = s
        .list_audit_events(
            9001,
            filter,
            AuditPosition::After(AuditBoundary::from(&second.events[0])),
        )
        .await
        .unwrap();
    assert_eq!(newer.events, first.events);
    let mut arrival = expected[0].clone();
    arrival.id = AuditEventId::new();
    arrival.occurred_at += chrono::Duration::days(1);
    s.audit(&arrival).await.unwrap();
    assert_eq!(
        s.list_audit_events(9001, filter, older)
            .await
            .unwrap()
            .events,
        second.events
    );
    let empty = s
        .list_audit_events(
            9001,
            filter,
            AuditPosition::Before(AuditBoundary::from(expected.last().unwrap())),
        )
        .await
        .unwrap();
    assert!(empty.events.is_empty());
    let foreign = s
        .list_audit_events(9002, None, AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(foreign.events.len(), 1);
    assert!(!foreign.has_older && !foreign.has_newer);
}

async fn scenario_install_uninstall_reinstall<S: Storage>(s: S) {
    s.insert_installation(&sample_account(1, 9001, "acme1"))
        .await
        .unwrap();

    s.mark_installation_uninstalled(1, dt("2026-05-04T18:00:00Z"))
        .await
        .unwrap();
    assert!(
        s.get_active_installation_by_account_id(9001)
            .await
            .unwrap()
            .is_none()
    );

    let mut reinstall = sample_account(2, 9001, "acme1");
    reinstall.installed_at = dt("2026-05-05T00:00:00Z");
    s.insert_installation(&reinstall).await.unwrap();

    let active = s
        .get_active_installation_by_account_id(9001)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.installation_id, 2);
}

async fn scenario_invitation_link_lifecycle<S: Storage>(s: S) {
    s.insert_installation(&sample_account(1, 9002, "acme2"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "creator")).await.unwrap();

    let link = sample_link(9002, 1, 701, 100);
    s.insert_invitation_link(&link).await.unwrap();

    let by_slug = s
        .get_invitation_link_by_slug(link.slug.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_slug.id, link.id);
    assert_eq!(by_slug.description, "AI coding workshop");

    s.mark_invitation_link_revoked(link.id, 701, dt("2026-05-04T20:00:00Z"))
        .await
        .unwrap();
    let revoked = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(revoked.revoked_by, Some(701));
    assert!(!revoked.is_active(dt("2026-05-04T21:00:00Z")));
}

async fn scenario_invitation_link_metadata<S: Storage>(s: S) {
    s.insert_installation(&sample_account(1, 9002, "acme2"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "creator")).await.unwrap();
    let mut expected = sample_link(9002, 1, 701, 101);
    expected.permission = Permission::Push;
    expected.approval_required = true;
    expected.max_uses = Some(1);
    expected.expires_at = Some(dt("2026-06-04T12:00:00Z"));
    expected.internal_note = Some("Original private note".into());
    s.insert_invitation_link(&expected).await.unwrap();

    s.update_invitation_link_metadata(
        9002,
        expected.id,
        "Corrected description",
        Some("Private\ncontext"),
    )
    .await
    .unwrap();
    expected.description = "Corrected description".into();
    expected.internal_note = Some("Private\ncontext".into());
    assert_eq!(
        s.get_invitation_link_by_id(expected.id).await.unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        s.list_invitation_links_for_account(9002).await.unwrap(),
        vec![expected.clone()]
    );

    for (account_id, id) in [(9999, expected.id), (9002, InvitationLinkId::new())] {
        assert!(matches!(
            s.update_invitation_link_metadata(account_id, id, "Forbidden", None)
                .await,
            Err(super::Error::NotFound)
        ));
    }
    assert_eq!(
        s.get_invitation_link_by_id(expected.id).await.unwrap(),
        Some(expected.clone())
    );

    // A request and a revocation can land after the editor loaded its metadata.
    s.insert_invitation_request_and_increment_uses(&sample_request(expected.id, 701))
        .await
        .unwrap();
    expected.uses_count = 1;
    s.mark_invitation_link_revoked(expected.id, 701, dt("2026-05-04T20:00:00Z"))
        .await
        .unwrap();
    expected.revoked_by = Some(701);
    expected.revoked_at = Some(dt("2026-05-04T20:00:00Z"));

    // Repeating an edit is successful, including clearing an already-absent note.
    for _ in 0..2 {
        s.update_invitation_link_metadata(9002, expected.id, "Inactive link", None)
            .await
            .unwrap();
        expected.description = "Inactive link".into();
        expected.internal_note = None;
        let actual = s
            .get_invitation_link_by_id(expected.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, expected);
        assert!(!actual.is_active(dt("2026-07-04T12:00:00Z")));
    }
}

async fn scenario_request_uses_and_uniqueness<S: Storage>(s: S) {
    s.insert_installation(&sample_account(1, 9003, "acme3"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(702, "creator")).await.unwrap();
    s.upsert_user(&sample_user(802, "asker")).await.unwrap();

    let mut link = sample_link(9003, 1, 702, 200);
    link.max_uses = Some(2);
    s.insert_invitation_link(&link).await.unwrap();

    let r1 = sample_request(link.id, 802);
    s.insert_invitation_request_and_increment_uses(&r1)
        .await
        .unwrap();

    let after_one = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(after_one.uses_count, 1);

    // Second pending for same (link, requester) should conflict.
    let r2 = sample_request(link.id, 802);
    let err = s
        .insert_invitation_request_and_increment_uses(&r2)
        .await
        .unwrap_err();
    assert!(matches!(err, super::Error::Conflict(_)));

    // After we decide r1, requester can ask again.
    s.record_request_decision(&RequestDecision {
        request_id: r1.id,
        state: RequestState::Declined,
        decided_by: Some(702),
        decided_at: dt("2026-05-04T13:00:00Z"),
        decline_reason: Some("not yet".into()),
    })
    .await
    .unwrap();

    let r3 = sample_request(link.id, 802);
    s.insert_invitation_request_and_increment_uses(&r3)
        .await
        .unwrap();

    let after_two = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(after_two.uses_count, 2);
    assert!(
        !after_two.is_active(dt("2026-05-04T20:00:00Z")),
        "max_uses exhausted"
    );
}

async fn scenario_request_decision<S: Storage>(s: S) {
    s.insert_installation(&sample_account(1, 9004, "acme4"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(703, "creator")).await.unwrap();
    s.upsert_user(&sample_user(803, "asker")).await.unwrap();

    let link = sample_link(9004, 1, 703, 300);
    s.insert_invitation_link(&link).await.unwrap();
    let req = sample_request(link.id, 803);
    s.insert_invitation_request_and_increment_uses(&req)
        .await
        .unwrap();

    // First decision wins; second on the same row should NotFound.
    s.record_request_decision(&RequestDecision {
        request_id: req.id,
        state: RequestState::Approved,
        decided_by: Some(703),
        decided_at: dt("2026-05-04T13:00:00Z"),
        decline_reason: None,
    })
    .await
    .unwrap();
    let err = s
        .record_request_decision(&RequestDecision {
            request_id: req.id,
            state: RequestState::Declined,
            decided_by: Some(703),
            decided_at: dt("2026-05-04T13:01:00Z"),
            decline_reason: Some("changed mind".into()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, super::Error::NotFound));

    let pending = s.list_pending_requests_for_account(9004).await.unwrap();
    assert!(pending.is_empty());
}

async fn scenario_github_invitation_lifecycle<S: Storage>(s: S) {
    s.insert_installation(&sample_account(1, 9005, "acme5"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(704, "creator")).await.unwrap();
    s.upsert_user(&sample_user(804, "asker")).await.unwrap();

    let link = sample_link(9005, 1, 704, 400);
    s.insert_invitation_link(&link).await.unwrap();
    let req = sample_request(link.id, 804);
    s.insert_invitation_request_and_increment_uses(&req)
        .await
        .unwrap();

    let g = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: req.id,
        repo_id: 10,
        github_invitation_id: None,
        state: InvitationState::Sending,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:00:00Z"),
    };
    s.insert_github_invitation(&g).await.unwrap();

    s.update_github_invitation(&GithubInvitationUpdate {
        id: g.id,
        state: InvitationState::Sent,
        github_invitation_id: Some(99001),
        error_message: None,
        updated_at: dt("2026-05-04T13:01:00Z"),
    })
    .await
    .unwrap();

    let by_gid = s
        .get_github_invitation_by_github_id(99001)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_gid.id, g.id);

    let pending = s
        .list_pending_github_invitations_for_installation(1)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);

    s.update_github_invitation(&GithubInvitationUpdate {
        id: g.id,
        state: InvitationState::Accepted,
        github_invitation_id: None,
        error_message: None,
        updated_at: dt("2026-05-04T13:05:00Z"),
    })
    .await
    .unwrap();

    let accepted = s.get_github_invitation(g.id).await.unwrap().unwrap();
    assert_eq!(accepted.state, InvitationState::Accepted);
    assert_eq!(accepted.github_invitation_id, None);

    assert!(
        s.get_github_invitation_by_github_id(99001)
            .await
            .unwrap()
            .is_none()
    );

    let now_done = s
        .list_pending_github_invitations_for_installation(1)
        .await
        .unwrap();
    assert!(now_done.is_empty());
}

async fn scenario_audit_appends<S: Storage>(s: S) {
    let e = AuditEvent {
        id: AuditEventId::new(),
        account_id: 9999,
        occurred_at: dt("2026-05-04T15:00:00Z"),
        event_type: EventType::InvitationLinkCreated,
        actor_kind: ActorKind::User,
        actor_id: Some(701),
        target_kind: TargetKind::InvitationLink,
        target_id: "01HFAUDIT1".into(),
        metadata: serde_json::json!({"slug": "ABCD"}),
        request_id: Some("inv-abc".into()),
    };
    s.audit(&e).await.unwrap();
    assert!(
        s.audit(&e).await.is_err(),
        "existing audit types still reject duplicate IDs"
    );
    let metadata = AuditEvent {
        id: AuditEventId::new(),
        event_type: EventType::InvitationLinkMetadataUpdated,
        metadata: serde_json::json!({"changed_fields": ["description"]}),
        ..e
    };
    s.audit(&metadata).await.unwrap();
    s.audit(&metadata).await.unwrap();
    // (No read on the trait; per-impl tests can verify via their own debug helpers.)
}

async fn scenario_timestamp_precision<S: Storage>(s: S) {
    use chrono::TimeZone;
    s.insert_installation(&sample_account(1, 9007, "acme7"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(707, "creator")).await.unwrap();
    let mut link = sample_link(9007, 1, 707, 700);
    // 123_456 microseconds added to a zero-second base
    link.created_at = Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap()
        + chrono::Duration::microseconds(123_456);
    s.insert_invitation_link(&link).await.unwrap();
    let got = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(
        got.created_at, link.created_at,
        "microsecond precision lost in round-trip"
    );
}
