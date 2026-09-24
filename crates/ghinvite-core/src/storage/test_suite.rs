//! Cross-cutting Storage behavior tests, parameterized over any `Storage` impl.
//! `ghinvite-storage-sqlx/tests/sqlx_suite.rs` calls `run_suite(make_storage)`
//! with a factory so each scenario gets a fresh database; the Worker storage
//! gate (`tests/worker/storage-suite.mjs`) runs each of [`SCENARIOS`] against
//! a fresh D1 database inside workerd. Links and requests
//! are seeded through the projector, their only production writer.

use super::Storage;
use super::projection::{ProjectionStorage, fixture};
use crate::audit::{ActorKind, AuditEvent, EventType, TargetKind};
use crate::{
    Account, AccountType, AuditEventId, GithubInvitation, GithubInvitationId, InvitationLink,
    InvitationLinkId, InvitationLinkRepo, InvitationRequest, InvitationState, Permission,
    RequestId, RequestState, SelectedRepos, User,
};
use chrono::{DateTime, Utc};

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
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

fn sample_link(account_id: u64, installation_id: u64, created_by: u64) -> InvitationLink {
    InvitationLink {
        id: InvitationLinkId::new(),
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
        decision_deadline: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    }
}

async fn seed<S: ProjectionStorage>(
    s: &S,
    link: &InvitationLink,
    requests: &[InvitationRequest],
    revision: u64,
) {
    s.apply_transition(&fixture::envelope(link, requests, revision))
        .await
        .unwrap();
}

/// Declares every scenario once: the name list adapters iterate and the
/// by-name dispatcher that runs one scenario against one store.
macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        /// Every scenario's name, in suite order.
        pub const SCENARIOS: &[&str] = &[$(stringify!($name)),*];

        /// Run the named scenario against `s`, which must be a fresh store:
        /// scenarios use simple fixed IDs. Returns `false` for an unknown name.
        pub async fn run_scenario<S: Storage + ProjectionStorage>(name: &str, s: S) -> bool {
            match name {
                $(stringify!($name) => $name(s).await,)*
                _ => return false,
            }
            true
        }
    };
}

scenarios![
    scenario_install_uninstall_reinstall,
    scenario_integers_stored_exactly_or_refused,
    scenario_invitation_link_lifecycle,
    scenario_recorded_request_deadlines,
    scenario_request_history,
    scenario_github_invitation_lifecycle,
    scenario_audit_appends,
    scenario_audit_pages,
    scenario_timestamp_precision,
    scenario_delivery_audit,
    scenario_delivery_projection_recovery,
    scenario_attempt_continuations,
    scenario_delivery_attempt_fence,
    scenario_member_webhook_binding,
    scenario_settlement_audit_conflict,
    scenario_insert_conflict_kinds,
    scenario_installation_update_not_found,
];

/// Run the full cross-cutting suite against any `Storage` impl.
///
/// `make_storage` is invoked once per scenario so every scenario starts against
/// a fresh database. Adapters that cannot run in-process (D1 inside workerd)
/// iterate [`SCENARIOS`] and call [`run_scenario`] themselves.
pub async fn run_suite<S, F, Fut>(make_storage: F)
where
    S: Storage + ProjectionStorage,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = S>,
{
    for name in SCENARIOS {
        assert!(run_scenario(name, make_storage().await).await, "{name}");
    }
}

/// Equal timestamps, mixed precision, terminal rows and foreign cursors must not
/// skip, duplicate or disclose records while traversing bounded pages.
pub async fn scenario_request_history<S: Storage + ProjectionStorage>(s: S) {
    use super::request_history::Boundary;
    s.insert_installation(&sample_account(1, 9001, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "admin")).await.unwrap();
    let link = sample_link(9001, 1, 701);
    seed(&s, &link, &[], 1).await;
    assert!(
        s.request_history(9001, link.id, None)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
    let mut ids = Vec::new();
    for i in 0..52 {
        let mut request = sample_request(link.id, 701);
        request.id = RequestId::from_ulid(ulid::Ulid::from(i + 1));
        request.state = match i % 4 {
            0 => RequestState::Approved,
            1 => RequestState::Declined,
            2 => RequestState::Expired,
            _ => RequestState::Cancelled,
        };
        request.created_at = dt(if i == 51 {
            "2026-05-04T12:30:00.000000001Z"
        } else {
            "2026-05-04T12:30:00Z"
        });
        seed(&s, &link, &[request.clone()], i as u64 + 2).await;
        ids.push(request.id);
    }
    ids.reverse();
    let first = s.request_history(9001, link.id, None).await.unwrap();
    assert_eq!(
        first.requester_logins.get(&701).map(String::as_str),
        Some("admin")
    );
    assert_eq!(
        first.requests.iter().map(|r| r.id).collect::<Vec<_>>(),
        ids[..25]
    );
    let second = s.request_history(9001, link.id, first.older).await.unwrap();
    assert_eq!(
        second.requests.iter().map(|r| r.id).collect::<Vec<_>>(),
        ids[25..50]
    );
    let third = s
        .request_history(9001, link.id, second.older)
        .await
        .unwrap();
    assert_eq!(
        third.requests.iter().map(|r| r.id).collect::<Vec<_>>(),
        ids[50..]
    );
    assert!(third.older.is_none());
    assert!(
        s.request_history(9999, link.id, first.older)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
    assert!(
        s.request_history(9001, InvitationLinkId::new(), None)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
    assert!(
        s.request_history(
            9001,
            link.id,
            Some(Boundary::from(third.requests.last().unwrap()))
        )
        .await
        .unwrap()
        .requests
        .is_empty()
    );
}

/// Historical deadlines survive every request read, including after a decision.
pub async fn scenario_recorded_request_deadlines<S: Storage + ProjectionStorage>(s: S) {
    s.insert_installation(&sample_account(1, 9008, "acme8"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(708, "admin")).await.unwrap();
    let link = sample_link(9008, 1, 708);
    let deadline = Some(dt("2026-05-06T14:15:16.123456789Z"));
    let mut request = sample_request(link.id, 708);
    request.decision_deadline = deadline;
    seed(&s, &link, &[request.clone()], 1).await;
    assert_eq!(
        s.get_invitation_request(request.id).await.unwrap(),
        Some(request.clone())
    );
    assert_eq!(s.count_pending_requests_for_account(9008).await.unwrap(), 1);
    assert_eq!(s.count_pending_requests_for_account(9009).await.unwrap(), 0);
    request.state = RequestState::Declined;
    request.decided_by = Some(708);
    request.decided_at = Some(dt("2026-05-05T12:00:00Z"));
    seed(&s, &link, &[request.clone()], 2).await;
    assert_eq!(
        s.get_invitation_request(request.id).await.unwrap(),
        Some(request)
    );
    assert_eq!(s.count_pending_requests_for_account(9008).await.unwrap(), 0);
}

/// Confirmed create facts are account history, even after lifecycle advances.
pub async fn scenario_delivery_audit<S: Storage + ProjectionStorage>(s: S) {
    use super::AuditPosition;
    s.insert_installation(&sample_account(1, 100, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(7, "admin")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let link = sample_link(100, 1, 7);
    let request = sample_request(link.id, 8);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let id = GithubInvitationId::new();
    let receipt: crate::delivery::CreateReceipt = serde_json::from_value(serde_json::json!({
        "command": {"invitation_id":id,"link_id":link.id,"request_id":request.id,
            "approval_id":"approval-64","account_id":100,"installation_id":1,"requester_id":8,
            "repo_id":10,"repo_full_name":"acme/api","permission":"pull","approved_at":request.created_at},
        "outcome":{"kind":"created","upstream_id":99123},"revision":2,
        "confirmed_at":"2026-05-04T13:00:00.123456789Z"
    })).unwrap();
    s.project_delivery(&receipt).await.unwrap();
    let invitation = s.get_github_invitation(id).await.unwrap().unwrap();
    assert_eq!(invitation.invitation_request_id, request.id);
    assert_eq!(invitation.repo_id, 10);
    assert_eq!(invitation.state, InvitationState::Sent);
    assert_eq!(invitation.github_invitation_id, Some(99123));
    assert_eq!(invitation.created_at, request.created_at);
    assert_eq!(invitation.updated_at, dt("2026-05-04T13:00:00.123456789Z"));
    let events = s
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    assert_eq!(
        events.len(),
        1,
        "confirmed create must publish account history"
    );
    let event = &events[0];
    assert_eq!(event.event_type, EventType::InvitationSent);
    assert_eq!(event.account_id, 100);
    assert_eq!(event.occurred_at, dt("2026-05-04T13:00:00.123456789Z"));
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, id.to_string());
    assert_eq!(
        event.metadata,
        serde_json::json!({"repo_full_name":"acme/api","requester_id":8,"github_invitation_id":99123})
    );
    // The invitation settles; replaying the create receipt changes nothing.
    let sent = s.get_github_invitation(id).await.unwrap().unwrap();
    s.settle_github_invitation(&super::settlement::Settlement {
        expected: sent,
        state: InvitationState::Declined,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: dt("2026-05-05T12:00:00Z"),
            event_type: EventType::InvitationDeclined,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: id.to_string(),
            metadata: serde_json::Value::Null,
            request_id: None,
        },
    })
    .await
    .unwrap();
    s.project_delivery(&receipt).await.unwrap();
    assert_eq!(
        s.list_audit_events(100, Some(EventType::InvitationSent), AuditPosition::Latest)
            .await
            .unwrap()
            .events,
        events
    );
    assert_eq!(
        s.get_github_invitation(id).await.unwrap().unwrap().state,
        InvitationState::Declined
    );
    assert!(
        s.list_audit_events(101, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    for (outcome, kind, actor, reason) in [
        (
            serde_json::json!({"kind":"already_collaborator"}),
            EventType::InvitationAccepted,
            ActorKind::Github,
            "already_collaborator",
        ),
        (
            serde_json::json!({"kind":"failed","status":422}),
            EventType::InvitationSendFailed,
            ActorKind::System,
            "github_rejected",
        ),
    ] {
        let mut value = serde_json::to_value(&receipt).unwrap();
        let id = GithubInvitationId::new();
        value["command"]["invitation_id"] = serde_json::json!(id);
        value["outcome"] = outcome;
        let confirmed: crate::delivery::CreateReceipt = serde_json::from_value(value).unwrap();
        s.project_delivery(&confirmed).await.unwrap();
        s.project_delivery(&confirmed).await.unwrap();
        let events = s
            .list_audit_events(100, Some(kind), AuditPosition::Latest)
            .await
            .unwrap()
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].account_id, 100);
        assert_eq!(events[0].target_kind, TargetKind::GithubInvitation);
        assert_eq!(events[0].target_id, id.to_string());
        assert_eq!(events[0].actor_kind, actor);
        assert_eq!(events[0].actor_id, None);
        assert_eq!(events[0].occurred_at, dt("2026-05-04T13:00:00.123456789Z"));
        assert_eq!(
            events[0].metadata,
            serde_json::json!({"repo_full_name":"acme/api","requester_id":8,"reason":reason})
        );
    }
    let before = s
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    for outcome in [
        serde_json::json!({"kind":"blocked","reason":"private-upstream-diagnostic"}),
        serde_json::json!({"kind":"outcome_unknown"}),
    ] {
        let mut value = serde_json::to_value(&receipt).unwrap();
        value["command"]["invitation_id"] = serde_json::json!(GithubInvitationId::new());
        value["outcome"] = outcome;
        value.as_object_mut().unwrap().remove("confirmed_at");
        let unconfirmed = serde_json::from_value(value).unwrap();
        s.project_delivery(&unconfirmed).await.unwrap();
    }
    assert_eq!(
        s.list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events,
        before
    );

    // A later observation can arrive first; historical events still arrive.
    let mut newer = receipt.clone();
    newer.command.invitation_id = GithubInvitationId::new();
    newer.revision = 3;
    newer.confirmed_at = None;
    newer.outcome = crate::delivery::CreateOutcome::OutcomeUnknown;
    s.project_delivery(&newer).await.unwrap();
    let mut older = receipt.clone();
    older.command = newer.command.clone();
    s.project_delivery(&older).await.unwrap();
    s.project_delivery(&older).await.unwrap();
    let events = s
        .list_audit_events(100, Some(EventType::InvitationSent), AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    assert_eq!(
        events.len(),
        2,
        "stale snapshot must not suppress its historical event"
    );
    assert!(
        s.list_delivery_for_request(request.id)
            .await
            .unwrap()
            .contains(&newer)
    );
    let mut conflict = older.clone();
    conflict.confirmed_at = Some(dt("2026-05-04T14:00:00Z"));
    assert!(
        s.project_delivery(&conflict).await.is_err(),
        "same event identity cannot change content even on stale receipt"
    );
    assert_eq!(
        s.list_audit_events(100, Some(EventType::InvitationSent), AuditPosition::Latest)
            .await
            .unwrap()
            .events,
        events
    );
    let mut wrong_account = older.clone();
    wrong_account.command.account_id = 101;
    assert!(
        s.project_delivery(&wrong_account).await.is_err(),
        "stale receipts must still enforce command identity"
    );
    assert!(
        s.list_audit_events(101, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
}

/// Complete receipt application, including interrupted acknowledgement and
/// atomic conflicts, through the same port on SQLx and actual D1.
pub async fn scenario_delivery_projection_recovery<S: Storage + ProjectionStorage>(s: S) {
    use super::{AuditPosition, Error};
    use crate::delivery::{CreateCommand, CreateOutcome, CreateReceipt};
    s.insert_installation(&sample_account(1, 100, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(7, "admin")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let link = sample_link(100, 1, 7);
    let request = sample_request(link.id, 8);
    let receipt = CreateReceipt {
        command: CreateCommand {
            invitation_id: GithubInvitationId::new(),
            link_id: link.id,
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: dt("2026-05-04T12:45:00Z"),
        },
        outcome: CreateOutcome::Created { upstream_id: 99123 },
        revision: 2,
        confirmed_at: Some(dt("2026-05-04T13:00:00.123456789Z")),
    };
    let id = receipt.command.invitation_id;
    assert!(matches!(
        s.project_delivery(&receipt).await,
        Err(Error::ProjectionDependency)
    ));
    assert!(s.get_github_invitation(id).await.unwrap().is_none());
    assert!(
        s.list_delivery_for_request(request.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    seed(&s, &link, std::slice::from_ref(&request), 1).await;

    // The adapter commits, but its caller loses the acknowledgement. A retry
    // is the only available action; it must return exactly the committed facts.
    let lost_ack: Result<(), Error> = async {
        s.project_delivery(&receipt).await?;
        Err(Error::Database(
            "fixture: commit acknowledgement lost".into(),
        ))
    }
    .await;
    assert!(lost_ack.is_err());
    let invitation = s.get_github_invitation(id).await.unwrap().unwrap();
    let events = s
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    s.project_delivery(&receipt).await.unwrap();
    assert_eq!(
        s.get_github_invitation(id).await.unwrap(),
        Some(invitation.clone())
    );
    assert_eq!(invitation.created_at, dt("2026-05-04T12:45:00Z"));
    assert_eq!(invitation.updated_at, dt("2026-05-04T13:00:00.123456789Z"));
    assert_eq!(events.len(), 1);

    let mut stale = receipt.clone();
    stale.revision = 1;
    stale.confirmed_at = None;
    stale.outcome = CreateOutcome::Blocked {
        reason: "installation unavailable".into(),
    };
    s.project_delivery(&stale).await.unwrap();
    let mut equal_conflict = stale.clone();
    equal_conflict.revision = 2;
    let mut identity_conflict = stale;
    identity_conflict.command.approval_id = "different approval".into();
    let mut audit_conflict = receipt.clone();
    audit_conflict.revision = 3;
    audit_conflict.confirmed_at = Some(dt("2026-05-04T14:00:00Z"));
    for conflict in [equal_conflict, identity_conflict, audit_conflict] {
        assert!(matches!(
            s.project_delivery(&conflict).await,
            Err(Error::ProjectionInvariant(_))
        ));
        assert_eq!(
            s.list_delivery_for_request(request.id).await.unwrap(),
            vec![receipt.clone()]
        );
        assert_eq!(
            s.get_github_invitation(id).await.unwrap(),
            Some(invitation.clone())
        );
        assert_eq!(
            s.list_audit_events(100, None, AuditPosition::Latest)
                .await
                .unwrap()
                .events,
            events
        );
    }

    // A pre-existing relational row must be checked even without a receipt.
    let mut wrong_row = invitation.clone();
    wrong_row.id = GithubInvitationId::new();
    wrong_row.repo_id = 99;
    s.insert_github_invitation(&wrong_row).await.unwrap();
    let mut conflict = receipt;
    conflict.command.invitation_id = wrong_row.id;
    assert!(matches!(
        s.project_delivery(&conflict).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_github_invitation(wrong_row.id).await.unwrap(),
        Some(wrong_row)
    );
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

/// An integer identity is stored exactly or refused, never silently rounded:
/// D1 binds JavaScript doubles, exact only up to 2^53.
async fn scenario_integers_stored_exactly_or_refused<S: Storage>(s: S) {
    let id = (1u64 << 53) + 1;
    let account = sample_account(id, 9012, "acme12");
    match s.insert_installation(&account).await {
        Ok(()) => assert_eq!(s.get_installation(id).await.unwrap(), Some(account)),
        Err(error) => {
            assert!(matches!(error, super::Error::Database(_)), "{error:?}");
            assert!(
                s.get_active_installation_by_account_id(9012)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }
}

async fn scenario_invitation_link_lifecycle<S: Storage + ProjectionStorage>(s: S) {
    s.insert_installation(&sample_account(1, 9002, "acme2"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "creator")).await.unwrap();

    let mut link = sample_link(9002, 1, 701);
    seed(&s, &link, &[], 1).await;
    assert_eq!(
        s.get_invitation_link_by_id(link.id).await.unwrap(),
        Some(link.clone())
    );

    link.revoked_by = Some(701);
    link.revoked_at = Some(dt("2026-05-04T20:00:00Z"));
    seed(&s, &link, &[], 2).await;
    let revoked = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(revoked.revoked_by, Some(701));
    assert!(!revoked.is_active(dt("2026-05-04T21:00:00Z")));

    // A stale snapshot cannot regress the row.
    link.revoked_by = None;
    link.revoked_at = None;
    seed(&s, &link, &[], 1).await;
    assert_eq!(
        s.get_invitation_link_by_id(link.id).await.unwrap(),
        Some(revoked)
    );
}

async fn scenario_github_invitation_lifecycle<S: Storage + ProjectionStorage>(s: S) {
    s.insert_installation(&sample_account(1, 9005, "acme5"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(704, "creator")).await.unwrap();
    s.upsert_user(&sample_user(804, "asker")).await.unwrap();

    let link = sample_link(9005, 1, 704);
    let req = sample_request(link.id, 804);
    seed(&s, &link, std::slice::from_ref(&req), 1).await;

    let g = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: req.id,
        repo_id: 10,
        github_invitation_id: Some(99001),
        state: InvitationState::Sent,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:01:00Z"),
    };
    s.insert_github_invitation(&g).await.unwrap();

    let by_gid = s
        .get_github_invitation_by_github_id(99001)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_gid.id, g.id);
    assert_eq!(
        s.list_pending_github_invitations_for_account(9005)
            .await
            .unwrap(),
        vec![g.clone()]
    );

    s.settle_github_invitation(&super::settlement::Settlement {
        expected: g.clone(),
        state: InvitationState::Accepted,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 9005,
            occurred_at: dt("2026-05-04T13:05:00Z"),
            event_type: EventType::InvitationAccepted,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: g.id.to_string(),
            metadata: serde_json::Value::Null,
            request_id: None,
        },
    })
    .await
    .unwrap();

    let accepted = s.get_github_invitation(g.id).await.unwrap().unwrap();
    assert_eq!(accepted.state, InvitationState::Accepted);
    assert!(
        s.list_pending_github_invitations_for_account(9005)
            .await
            .unwrap()
            .is_empty()
    );
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
        matches!(s.audit(&e).await, Err(super::Error::Database(_))),
        "existing audit types still reject duplicate IDs as storage failures"
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

async fn scenario_timestamp_precision<S: Storage + ProjectionStorage>(s: S) {
    use chrono::TimeZone;
    s.insert_installation(&sample_account(1, 9007, "acme7"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(707, "creator")).await.unwrap();
    let mut link = sample_link(9007, 1, 707);
    // 123_456 microseconds added to a zero-second base
    link.created_at = Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap()
        + chrono::Duration::microseconds(123_456);
    seed(&s, &link, &[], 1).await;
    let got = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(
        got.created_at, link.created_at,
        "microsecond precision lost in round-trip"
    );
}

/// Browser continuations: the first writer wins by ID and by logical binding,
/// per scope, and a scope lists in retention order; expired records are
/// invisible and reclaimed; release forgets one.
async fn scenario_attempt_continuations<S: Storage>(s: S) {
    let retained = |id: &'static str, binding: &'static str, payload: &'static str| {
        s.retain_attempt_continuation("scope", id, binding, payload, 200, 100)
    };
    let first = retained("a1", "bind", "p1").await.unwrap();
    assert_eq!((first.id.as_str(), first.payload.as_str()), ("a1", "p1"));
    let same_id = retained("a1", "other", "p2").await.unwrap();
    assert_eq!(
        (same_id.id.as_str(), same_id.payload.as_str()),
        ("a1", "p1")
    );
    let same_binding = retained("a2", "bind", "p3").await.unwrap();
    assert_eq!(
        (same_binding.id.as_str(), same_binding.payload.as_str()),
        ("a1", "p1")
    );
    let other_scope = s
        .retain_attempt_continuation("other", "a1", "bind", "p4", 200, 100)
        .await
        .unwrap();
    assert_eq!(other_scope.payload, "p4");
    assert_eq!(
        s.get_attempt_continuation("scope", "a1", 100)
            .await
            .unwrap()
            .map(|a| a.payload),
        Some("p1".into())
    );
    assert!(
        s.get_attempt_continuation("scope", "a2", 100)
            .await
            .unwrap()
            .is_none()
    );
    retained("0-later", "later", "p7").await.unwrap();
    let listed = s.list_attempt_continuations("scope", 100).await.unwrap();
    assert_eq!(
        listed.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["a1", "0-later"],
        "listed in retention order, not by ID"
    );
    assert!(
        s.get_attempt_continuation("scope", "a1", 200)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.list_attempt_continuations("scope", 200)
            .await
            .unwrap()
            .is_empty()
    );
    let reclaimed = s
        .retain_attempt_continuation("scope", "a3", "bind", "p5", 300, 200)
        .await
        .unwrap();
    assert_eq!(
        (reclaimed.id.as_str(), reclaimed.payload.as_str()),
        ("a3", "p5")
    );
    s.release_attempt_continuation("scope", "a3").await.unwrap();
    s.release_attempt_continuation("scope", "a3").await.unwrap();
    assert!(
        s.get_attempt_continuation("scope", "a3", 200)
            .await
            .unwrap()
            .is_none()
    );
    // A record that is already expired cannot be read back after its write:
    // a retryable storage failure, not a missing resource.
    assert!(matches!(
        s.retain_attempt_continuation("scope", "a4", "b4", "p6", 200, 200)
            .await,
        Err(super::Error::Database(_))
    ));
}

/// One PUT per generation: replay after acknowledgement loss stays fenced, only
/// rejecting the current generation reopens it, and the fence is input-bound.
async fn scenario_delivery_attempt_fence<S: Storage>(s: S) {
    let id = GithubInvitationId::new();
    let command: crate::delivery::CreateCommand = serde_json::from_value(serde_json::json!({
        "invitation_id":id,"link_id":InvitationLinkId::new(),"request_id":RequestId::new(),
        "approval_id":"approval-1","account_id":100,"installation_id":1,"requester_id":8,
        "repo_id":10,"repo_full_name":"acme/api","permission":"pull",
        "approved_at":"2026-05-04T12:30:00Z"
    }))
    .unwrap();
    assert!(!s.delivery_attempt_exists(id).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), Some(1));
    assert!(s.delivery_attempt_exists(id).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), None);
    s.reject_delivery_attempt(id, 1).await.unwrap();
    assert!(!s.delivery_attempt_exists(id).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), Some(2));
    s.reject_delivery_attempt(id, 1).await.unwrap();
    assert!(s.delivery_attempt_exists(id).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), None);
    let changed = crate::delivery::CreateCommand {
        repo_id: 11,
        ..command
    };
    assert!(matches!(
        s.claim_delivery_attempt(&changed).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
}

/// The first match (including none) retained for a webhook body is final.
async fn scenario_member_webhook_binding<S: Storage + ProjectionStorage>(s: S) {
    s.insert_installation(&sample_account(1, 9010, "acme10"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(710, "creator")).await.unwrap();
    s.upsert_user(&sample_user(810, "asker")).await.unwrap();
    let link = sample_link(9010, 1, 710);
    let request = sample_request(link.id, 810);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let invitation = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: request.id,
        repo_id: 10,
        github_invitation_id: Some(99010),
        state: InvitationState::Sent,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:00:00Z"),
    };
    s.insert_github_invitation(&invitation).await.unwrap();
    let unmatched = "a".repeat(64);
    let matched = "b".repeat(64);
    assert_eq!(s.bind_member_webhook(&unmatched, None).await.unwrap(), None);
    assert_eq!(
        s.bind_member_webhook(&unmatched, Some(invitation.id))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        s.bind_member_webhook(&matched, Some(invitation.id))
            .await
            .unwrap(),
        Some(invitation.id)
    );
    assert_eq!(
        s.bind_member_webhook(&matched, None).await.unwrap(),
        Some(invitation.id)
    );
}

/// A settlement whose audit ID holds different content is a retryable storage
/// failure that changes nothing; stale evidence after settling is a no-op.
async fn scenario_settlement_audit_conflict<S: Storage + ProjectionStorage>(s: S) {
    use super::AuditPosition;
    s.insert_installation(&sample_account(1, 9011, "acme11"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(711, "creator")).await.unwrap();
    s.upsert_user(&sample_user(811, "asker")).await.unwrap();
    let link = sample_link(9011, 1, 711);
    let request = sample_request(link.id, 811);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let sent = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: request.id,
        repo_id: 10,
        github_invitation_id: Some(99011),
        state: InvitationState::Sent,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:00:00Z"),
    };
    s.insert_github_invitation(&sent).await.unwrap();
    let event = AuditEvent {
        id: AuditEventId::new(),
        account_id: 9011,
        occurred_at: dt("2026-05-04T13:05:00Z"),
        event_type: EventType::InvitationAccepted,
        actor_kind: ActorKind::Github,
        actor_id: None,
        target_kind: TargetKind::GithubInvitation,
        target_id: sent.id.to_string(),
        metadata: serde_json::Value::Null,
        request_id: None,
    };
    s.audit(&AuditEvent {
        target_id: "someone-else".into(),
        ..event.clone()
    })
    .await
    .unwrap();
    let settlement = |event: AuditEvent| super::settlement::Settlement {
        expected: sent.clone(),
        state: InvitationState::Accepted,
        event,
    };
    assert!(matches!(
        s.settle_github_invitation(&settlement(event.clone())).await,
        Err(super::Error::Database(_))
    ));
    assert_eq!(
        s.get_github_invitation(sent.id).await.unwrap(),
        Some(sent.clone())
    );
    let winner = AuditEvent {
        id: AuditEventId::new(),
        ..event.clone()
    };
    s.settle_github_invitation(&settlement(winner.clone()))
        .await
        .unwrap();
    s.settle_github_invitation(&settlement(winner.clone()))
        .await
        .unwrap();
    s.settle_github_invitation(&settlement(AuditEvent {
        id: AuditEventId::new(),
        ..winner.clone()
    }))
    .await
    .unwrap();
    assert_eq!(
        s.get_github_invitation(sent.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        InvitationState::Accepted
    );
    let history = s
        .list_audit_events(9011, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    assert_eq!(
        history
            .iter()
            .filter(|e| e.target_id == sent.id.to_string())
            .collect::<Vec<_>>(),
        [&winner]
    );
}

/// A refused insert names the constraint it hit, not just "the write failed":
/// re-onboarding branches on [`ConflictKind::DuplicateId`], and only the
/// partial unique index tells an account's second *active* installation apart.
async fn scenario_insert_conflict_kinds<S: Storage + ProjectionStorage>(s: S) {
    use super::{ConflictKind, Error};
    s.insert_installation(&sample_account(1, 9013, "acme13"))
        .await
        .unwrap();
    let second_active = s
        .insert_installation(&sample_account(2, 9013, "acme13"))
        .await;
    assert!(
        matches!(
            second_active,
            Err(Error::Conflict(ConflictKind::DuplicateActiveInstallation))
        ),
        "{second_active:?}"
    );
    // Account 9014 has no active row, so only the primary key is violated.
    let reused_id = s
        .insert_installation(&sample_account(1, 9014, "acme14"))
        .await;
    assert!(
        matches!(reused_id, Err(Error::Conflict(ConflictKind::DuplicateId))),
        "{reused_id:?}"
    );

    s.upsert_user(&sample_user(713, "creator")).await.unwrap();
    s.upsert_user(&sample_user(813, "asker")).await.unwrap();
    let link = sample_link(9013, 1, 713);
    let request = sample_request(link.id, 813);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let invitation = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: request.id,
        repo_id: 10,
        github_invitation_id: None,
        state: InvitationState::Sending,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:00:00Z"),
    };
    s.insert_github_invitation(&invitation).await.unwrap();
    let replayed = s.insert_github_invitation(&invitation).await;
    assert!(
        matches!(replayed, Err(Error::Conflict(ConflictKind::DuplicateId))),
        "{replayed:?}"
    );
    let orphan = s
        .insert_github_invitation(&GithubInvitation {
            id: GithubInvitationId::new(),
            invitation_request_id: RequestId::new(),
            ..invitation
        })
        .await;
    assert!(
        matches!(orphan, Err(Error::Conflict(ConflictKind::ForeignKey))),
        "{orphan:?}"
    );
}

/// Installation updates address one live row: an unknown or already
/// uninstalled installation is missing, not a storage failure, and the
/// refused update leaves the row alone.
async fn scenario_installation_update_not_found<S: Storage>(s: S) {
    use super::Error;
    let subset = SelectedRepos::Subset(vec![10]);
    assert!(matches!(
        s.mark_installation_uninstalled(1, dt("2026-05-04T18:00:00Z"))
            .await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        s.update_installation_repos(1, &subset).await,
        Err(Error::NotFound)
    ));
    s.insert_installation(&sample_account(1, 9015, "acme15"))
        .await
        .unwrap();
    s.update_installation_repos(1, &subset).await.unwrap();
    s.mark_installation_uninstalled(1, dt("2026-05-04T18:00:00Z"))
        .await
        .unwrap();
    assert!(matches!(
        s.mark_installation_uninstalled(1, dt("2026-05-04T19:00:00Z"))
            .await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        s.update_installation_repos(1, &SelectedRepos::All).await,
        Err(Error::NotFound)
    ));
    let row = s.get_installation(1).await.unwrap().unwrap();
    assert_eq!(row.selected_repos, subset);
    assert_eq!(row.uninstalled_at, Some(dt("2026-05-04T18:00:00Z")));
}
