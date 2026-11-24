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
    InvitationLinkId, InvitationRequest, InvitationState, Permission, RequestId, RequestState,
    SelectedRepos, User,
};
use chrono::{DateTime, Utc};

/// Historical fixture assertions deliberately decode retained Legacy identities;
/// production readers consume the snapshot directly and never route by ULID.
async fn legacy_read<S: super::ConsoleStorage>(
    s: &S,
    request: RequestId,
    repo: u64,
    generation: u64,
) -> super::Result<Option<GithubInvitation>> {
    Ok(
        s.get_delivery(&crate::delivery::delivery_key(request, repo, generation))
            .await?
            .map(|snapshot| legacy_fixture_view(&snapshot)),
    )
}
fn legacy_fixture_view(snapshot: &crate::delivery::DeliverySnapshot) -> GithubInvitation {
    use crate::InvitationState;
    use crate::delivery::CreateOutcome;
    let (state, upstream_id, error) = match &snapshot.create.outcome {
        CreateOutcome::Created { upstream_id }
        | CreateOutcome::AlreadyPending { upstream_id, .. } => {
            (InvitationState::Sent, Some(*upstream_id), None)
        }
        CreateOutcome::AlreadySatisfied { .. } | CreateOutcome::Upgraded { .. } => {
            (InvitationState::Accepted, None, None)
        }
        CreateOutcome::Failed { reason } => (
            InvitationState::Failed,
            None,
            Some(serde_json::to_string(reason).expect("typed reason serializes")),
        ),
        CreateOutcome::SkippedCustomRole { .. } | CreateOutcome::Aborted { .. } => {
            (InvitationState::Failed, None, None)
        }
        _ => (InvitationState::Sending, None, None),
    };
    crate::GithubInvitation {
        id: snapshot
            .create
            .command
            .invitation_id
            .expect("legacy fixture compatibility identity"),
        delivery_generation: snapshot.create.command.delivery_generation,
        invitation_request_id: snapshot.create.command.request_id,
        repo_id: snapshot.create.command.repo_id,
        github_invitation_id: upstream_id,
        state: snapshot.settlement.as_ref().map_or(state, |s| s.state),
        error_message: error,
        created_at: snapshot.create.command.approved_at,
        updated_at: snapshot
            .settlement
            .as_ref()
            .map(|s| s.event.occurred_at)
            .or(snapshot.create.confirmed_at)
            .unwrap_or(snapshot.create.command.approved_at),
    }
}

async fn seed_legacy<S: super::DeliveryStorage>(
    s: &S,
    link: &InvitationLink,
    request: &InvitationRequest,
    row: &GithubInvitation,
) {
    let mut snapshot = delivery_fixture(link, request, row);
    snapshot.create.command.delivery_generation = row.delivery_generation;
    if row.state == InvitationState::Sending {
        snapshot.create.outcome = crate::delivery::CreateOutcome::Planned;
        snapshot.create.confirmed_at = None;
    }
    s.project_delivery(&snapshot).await.unwrap();
}

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

fn sample_org_installation(id: u64, account: u64, login: &str) -> crate::account::Installation {
    let mut installation = crate::account::Installation::repo(sample_account(id, account, login));
    installation.app = crate::account::App::Org;
    installation.permissions.insert(
        "members".into(),
        crate::account::InstallationPermission::Write,
    );
    installation
}

fn sample_user(user_id: u64, login: &str) -> User {
    User {
        user_id,
        login: login.into(),
        avatar_url: None,
        github_created_at: None,
        last_seen_at: dt("2026-05-04T12:00:00Z"),
    }
}

fn sample_link(account_id: u64, installation_id: u64, created_by: u64) -> InvitationLink {
    InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: InvitationLinkId::new(),
        organization_installation_id: None,
        installation_id,
        account_id,
        created_by,
        created_at: dt("2026-05-04T12:00:00Z"),
        expires_at: None,
        max_uses: Some(1000),
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,

        approval_required: false,
        description: "AI coding workshop".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        targets: crate::targets::Targets {
            repos: vec![crate::targets::RepoTarget {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                grant: crate::targets::RepoRole::Read,
            }],
            ..Default::default()
        },

        secret: crate::LinkSecretState {
            secret_generation: 1,
            sha256: crate::SecretHash::from_bytes([1; 32]),
        },
    }
}

fn sample_request(link: InvitationLinkId, requester: u64) -> InvitationRequest {
    InvitationRequest {
        batch: None,
        domain_evidence: None,
        id: RequestId::new(),
        invitation_link_id: Some(link),
        requester_id: requester,
        justification: None,
        state: RequestState::Pending,
        decided_by: None,
        decided_at: None,
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
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
    fixture::seed(s, link, requests, revision).await.unwrap();
}

/// Declares every scenario once: the name list adapters iterate and the
/// by-name dispatcher that runs one scenario against one store.
macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        /// Every scenario's name, in suite order.
        pub const SCENARIOS: &[&str] = &[$(stringify!($name)),*];

        /// Run the named scenario against `s`, which must be a fresh store:
        /// scenarios use simple fixed IDs. Returns `false` for an unknown name.
        pub async fn run_scenario<S: Storage + ProjectionStorage + super::invite_budget::InviteBudgetStorage + super::blocked_digest::DailyDigestStorage + super::access_grants::AccessGrantStorage + super::bulk::BulkStorage + super::email_delivery::EmailDeliveryStorage>(name: &str, s: S) -> bool {
            match name {
                $(stringify!($name) => $name(s).await,)*
                _ => return false,
            }
            true
        }
    };
}

mod requester_profiles;
pub use requester_profiles::scenario_requester_profiles;
mod email_audit;
pub use email_audit::scenario_email_transition_audit;
mod access_grants;
pub use access_grants::scenario_access_grants;
mod bulk;
pub use bulk::scenario_bulk_review;
mod bulk_audit;
pub use bulk_audit::scenario_bulk_delegated_audit;
mod link_installations;
pub use link_installations::scenario_link_installation_bindings;
mod org_delivery;
pub use org_delivery::scenario_organization_delivery;
mod org_delivery_reads;
pub use org_delivery_reads::scenario_organization_delivery_reads;
mod org_webhooks;
pub use org_webhooks::scenario_organization_webhook_bindings;
mod link_pause_tests;
pub use link_pause_tests::scenario_link_pause;
mod blocked_digest_tests;
pub use blocked_digest_tests::scenario_blocked_digest;
mod sweep_digest;
pub use sweep_digest::scenario_sweep_digest;
mod organization_telemetry;
pub use organization_telemetry::scenario_organization_telemetry;
mod operational_tests;
pub use operational_tests::scenario_repository_telemetry;
mod link_share_notices;
pub use link_share_notices::scenario_link_share_notices;

mod installation_audit;
pub use installation_audit::scenario_installation_transition_audit;
mod canonical_audit;
pub use canonical_audit::scenario_canonical_delivery_audit;
mod delivery_audit;
pub use delivery_audit::scenario_delivery_transition_audit;
mod email_domains;
pub use email_domains::scenario_email_domains;
mod final_results;
pub use final_results::scenario_final_result_projection;
mod reissue;
pub use reissue::scenario_request_reissue;
mod request_lapsed;
pub use request_lapsed::scenario_request_lapsed;
mod requester_message;
pub use requester_message::scenario_requester_message;
mod request_cancellation;
pub use request_cancellation::{scenario_request_cancellation, scenario_revoke_receipt};

mod invitation_timing;
pub use invitation_timing::scenario_invitation_timing;

scenarios![
    scenario_invitation_timing,
    scenario_bulk_delegated_audit,
    scenario_email_transition_audit,
    scenario_bulk_review,
    scenario_access_grants,
    scenario_request_reissue,
    scenario_installation_transition_audit,
    scenario_canonical_delivery_audit,
    scenario_delivery_transition_audit,
    scenario_email_domains,
    scenario_link_installation_bindings,
    scenario_organization_delivery,
    scenario_organization_delivery_reads,
    scenario_final_result_projection,
    scenario_request_lapsed,
    scenario_organization_webhook_bindings,
    scenario_link_share_notices,
    scenario_request_cancellation,
    scenario_requester_message,
    scenario_requester_profiles,
    scenario_revoke_receipt,
    scenario_link_pause,
    scenario_blocked_digest,
    scenario_sweep_digest,
    scenario_invite_budget_projection,
    scenario_repository_telemetry,
    scenario_organization_telemetry,
    scenario_console_scope,
    scenario_app_installations_and_delegation,
    scenario_app_installation_projection_order,
    scenario_link_capabilities,
    scenario_heterogeneous_link_targets,
    scenario_signin_budget,
    scenario_foreign_pending_generation,
    scenario_install_uninstall_reinstall,
    scenario_integers_stored_exactly_or_refused,
    scenario_invitation_link_lifecycle,
    scenario_recorded_request_deadlines,
    scenario_request_owner_projection,
    scenario_link_projection_conflicts,
    scenario_request_history,
    scenario_github_invitation_lifecycle,
    scenario_audit_appends,
    scenario_audit_pages,
    scenario_timestamp_precision,
    scenario_delivery_audit,
    scenario_delivery_projection_recovery,
    scenario_attempt_continuations,
    scenario_continuation_rotation,
    scenario_delivery_attempt_fence,
    scenario_repository_projection_identity,
    scenario_settlement_audit_conflict,
    scenario_insert_conflict_kinds,
    scenario_installation_revision_fencing,
];

pub async fn scenario_link_projection_conflicts<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "admin")).await.unwrap();
    let mut link = sample_link(9001, 1, 701);
    link.id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
    let original = fixture::envelope(&link, 2);
    s.apply_transition(&original).await.unwrap();
    let mut conflicts = Vec::new();
    let mut changed = original.clone();
    changed.link.uses += 1;
    conflicts.push(changed);
    let mut changed = original.clone();
    changed.link.metadata = Some(crate::invitation_link::LinkMetadata {
        description: "Conflicting metadata".into(),
        internal_note: None,
    });
    conflicts.push(changed);
    for revision in [1, 2, 3] {
        let mut changed = original.clone();
        changed.link.revision = revision;
        changed.link.creation.budget_share_pct = 51;
        conflicts.push(changed);
        let mut changed = original.clone();
        changed.link.revision = revision;
        changed
            .link
            .creation
            .targets
            .repos
            .push(crate::targets::RepoTarget {
                grant: crate::targets::RepoRole::Read,
                repo_id: 11,
                repo_full_name: "acme/web".into(),
            });
        conflicts.push(changed);
        let mut changed = original.clone();
        changed.link.revision = revision;
        changed.link.creation.description = "Changed original input".into();
        conflicts.push(changed);
    }
    for conflict in conflicts {
        assert!(
            matches!(
                s.apply_transition(&conflict).await,
                Err(super::Error::ProjectionInvariant(_))
            ),
            "{conflict:?}"
        );
        s.apply_transition(&original).await.unwrap();
        assert_eq!(
            s.get_invitation_link_by_id(link.id).await.unwrap(),
            Some(link.clone())
        );
    }
    let mut newer = original.clone();
    newer.link.revision += 1;
    newer.link.uses += 1;
    s.apply_transition(&newer).await.unwrap();
    s.apply_transition(&original).await.unwrap();
    assert_eq!(
        s.get_invitation_link_by_id(link.id)
            .await
            .unwrap()
            .unwrap()
            .uses_count,
        1
    );
}

/// Both adapters remove the repository child rows after the conflict scenario,
/// then run these same assertions through their production projector.
pub async fn verify_link_projection_repair<S: Storage + ProjectionStorage>(s: S) {
    let mut link = sample_link(9001, 1, 701);
    link.id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
    let original = fixture::envelope(&link, 2);
    let mut expanded = original.clone();
    expanded
        .link
        .creation
        .targets
        .repos
        .push(crate::targets::RepoTarget {
            grant: crate::targets::RepoRole::Read,
            repo_id: 11,
            repo_full_name: "acme/web".into(),
        });
    assert!(matches!(
        s.apply_transition(&expanded).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    assert!(
        s.get_invitation_link_by_id(link.id)
            .await
            .unwrap()
            .unwrap()
            .targets
            .repos
            .is_empty()
    );
    // A legitimate stale snapshot repairs scope without regressing current uses.
    s.apply_transition(&original).await.unwrap();
    link.uses_count = 1;
    assert_eq!(
        s.get_invitation_link_by_id(link.id).await.unwrap(),
        Some(link)
    );
    s.apply_transition(&original).await.unwrap();
}

pub async fn scenario_request_owner_projection<S: Storage + ProjectionStorage>(s: S) {
    use super::projection::{AuditIntent, RequestProjectionEnvelope};
    use crate::request_lifecycle::{RequestSnapshot, TerminalDecision};
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "admin")).await.unwrap();
    let link = sample_link(9001, 1, 701);
    let id = RequestId::new();
    let admitted = dt("2026-05-04T12:00:00Z");
    let deadline = dt("2026-05-11T12:00:00Z");
    let initial = RequestSnapshot {
        batch: None,
        domain_evidence: None,
        request_id: id,
        link_id: Some(link.id),
        account_id: 9001,
        requester_id: 701,
        justification: None,
        state: RequestState::Pending,
        admitted_at: admitted,
        decision_deadline: Some(deadline),
        revision: 1,
        decision: None,
    };
    let created = AuditIntent {
        event_id: format!("request.created/{id}"),
        kind: EventType::RequestCreated,
        actor_id: Some(701),
        target_id: id.to_string(),
        effective_at: admitted,
        evaluated_at: admitted,
    };
    let mut terminal = initial.clone();
    terminal.state = RequestState::Expired;
    terminal.revision = 2;
    terminal.decision = Some(TerminalDecision {
        decision_id: format!("request.expired/{id}"),
        decided_by: None,
        effective_at: deadline,
        evaluated_at: deadline,
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
    });
    let expired = AuditIntent {
        event_id: format!("request.expired/{id}"),
        kind: EventType::RequestExpired,
        actor_id: None,
        target_id: id.to_string(),
        effective_at: deadline,
        evaluated_at: deadline,
    };
    let envelope = RequestProjectionEnvelope {
        approval: None,
        request: terminal.clone(),
        events: vec![expired],
    };
    let mut old_link_envelope = serde_json::to_value(fixture::envelope(&link, 1)).unwrap();
    old_link_envelope["requests"] = serde_json::json!([terminal]);
    assert!(
        serde_json::from_value::<super::projection::ProjectionEnvelope>(old_link_envelope).is_err(),
        "the retired link-owned request writer must not be an accepted protocol"
    );
    assert!(matches!(
        s.apply_request(&envelope).await,
        Err(super::Error::ProjectionDependency)
    ));
    seed(&s, &link, &[], 1).await;
    let lost_ack: Result<(), super::Error> = async {
        s.apply_request(&envelope).await?;
        Err(super::Error::Database(
            "fixture: committed request acknowledgement lost".into(),
        ))
    }
    .await;
    assert!(lost_ack.is_err());
    assert_eq!(
        s.get_invitation_request(id).await.unwrap().unwrap().state,
        RequestState::Expired
    );
    // A late initialization supplies missing audit without regressing a terminal row.
    s.apply_request(&RequestProjectionEnvelope {
        approval: None,
        request: initial,
        events: vec![created],
    })
    .await
    .unwrap();
    s.apply_request(&envelope).await.unwrap();
    assert_eq!(
        s.get_invitation_request(id).await.unwrap().unwrap().state,
        RequestState::Expired
    );
    let audit = s
        .list_audit_events(9001, None, super::AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(
        audit
            .events
            .iter()
            .filter(|e| e.target_id == id.to_string())
            .count(),
        2
    );
    // Even fields that are not displayed by SQL are part of equal-revision content.
    terminal.decision.as_mut().unwrap().evaluated_at = deadline + chrono::Duration::seconds(1);
    assert!(matches!(
        s.apply_request(&RequestProjectionEnvelope {
            approval: None,
            request: terminal,
            events: vec![]
        })
        .await,
        Err(super::Error::ProjectionInvariant(_))
    ));
}

/// Run the full cross-cutting suite against any `Storage` impl.
///
/// `make_storage` is invoked once per scenario so every scenario starts against
/// a fresh database. Adapters that cannot run in-process (D1 inside workerd)
/// iterate [`SCENARIOS`] and call [`run_scenario`] themselves.
pub async fn run_suite<S, F, Fut>(make_storage: F)
where
    S: Storage
        + ProjectionStorage
        + super::invite_budget::InviteBudgetStorage
        + super::blocked_digest::DailyDigestStorage
        + super::access_grants::AccessGrantStorage
        + super::bulk::BulkStorage
        + super::email_delivery::EmailDeliveryStorage,
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
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
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
        request.cancellation_reason = (request.state == RequestState::Cancelled)
            .then_some(crate::RequestCancellationReason::LinkRevoked);
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
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9008, "acme8")).clone(),
    ))
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
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
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
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    let invitation = legacy_read(&s, request.id, 10, 1).await.unwrap().unwrap();
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
    let mut settled = crate::delivery::DeliverySnapshot::from(receipt.clone());
    settled.revision += 1;
    settled.settlement = Some(crate::delivery::Settlement {
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
    });
    s.project_delivery(&settled).await.unwrap();
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    assert_eq!(
        s.list_audit_events(100, Some(EventType::InvitationSent), AuditPosition::Latest)
            .await
            .unwrap()
            .events,
        events
    );
    assert_eq!(
        legacy_read(&s, request.id, 10, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        InvitationState::Declined
    );
    assert!(
        s.list_audit_events(101, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    let mut next_delivery_generation = 1;
    for (outcome, kind, actor, reason) in [
        (
            serde_json::json!({"kind":"already_satisfied","effective_role":"read"}),
            EventType::DeliveryAlreadySatisfied,
            ActorKind::Github,
            "already_collaborator",
        ),
        (
            serde_json::json!({"kind":"failed","reason":{"kind":"rejected","status":422}}),
            EventType::InvitationSendFailed,
            ActorKind::System,
            "github_rejected",
        ),
    ] {
        let mut value = serde_json::to_value(&receipt).unwrap();
        let id = GithubInvitationId::new();
        value["command"]["invitation_id"] = serde_json::json!(id);
        next_delivery_generation += 1;
        value["command"]["delivery_generation"] = serde_json::json!(next_delivery_generation);
        value["outcome"] = outcome;
        let confirmed: crate::delivery::CreateReceipt = serde_json::from_value(value).unwrap();
        s.project_delivery(&confirmed.clone().into()).await.unwrap();
        s.project_delivery(&confirmed.into()).await.unwrap();
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
        serde_json::json!({"kind":"blocked","reason":"installation_unavailable"}),
        serde_json::json!({"kind":"outcome_unknown"}),
    ] {
        let mut value = serde_json::to_value(&receipt).unwrap();
        value["command"]["invitation_id"] = serde_json::json!(GithubInvitationId::new());
        next_delivery_generation += 1;
        value["command"]["delivery_generation"] = serde_json::json!(next_delivery_generation);
        value["outcome"] = outcome;
        value.as_object_mut().unwrap().remove("confirmed_at");
        let unconfirmed: crate::delivery::CreateReceipt = serde_json::from_value(value).unwrap();
        s.project_delivery(&unconfirmed.into()).await.unwrap();
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
    newer.command.invitation_id = Some(GithubInvitationId::new());
    next_delivery_generation += 1;
    newer.command.delivery_generation = next_delivery_generation;
    newer.revision = 3;
    newer.confirmed_at = None;
    newer.outcome = crate::delivery::CreateOutcome::OutcomeUnknown;
    s.project_delivery(&newer.clone().into()).await.unwrap();
    let mut older = receipt.clone();
    older.command = newer.command.clone();
    s.project_delivery(&older.clone().into()).await.unwrap();
    s.project_delivery(&older.clone().into()).await.unwrap();
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
            .contains(&newer.into())
    );
    let mut conflict = older.clone();
    conflict.confirmed_at = Some(dt("2026-05-04T14:00:00Z"));
    assert!(
        s.project_delivery(&conflict.into()).await.is_err(),
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
        s.project_delivery(&wrong_account.into()).await.is_err(),
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
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(7, "admin")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let link = sample_link(100, 1, 7);
    let request = sample_request(link.id, 8);
    let receipt = CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            delivery_generation: 1,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(link.id),
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
    assert!(matches!(
        s.project_delivery(&receipt.clone().into()).await,
        Err(Error::ProjectionDependency)
    ));
    assert!(legacy_read(&s, request.id, 10, 1).await.unwrap().is_none());
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
        s.project_delivery(&receipt.clone().into()).await?;
        Err(Error::Database(
            "fixture: commit acknowledgement lost".into(),
        ))
    }
    .await;
    assert!(lost_ack.is_err());
    let invitation = legacy_read(&s, request.id, 10, 1).await.unwrap().unwrap();
    let events = s
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    assert_eq!(
        legacy_read(&s, request.id, 10, 1).await.unwrap(),
        Some(invitation.clone())
    );
    assert_eq!(invitation.created_at, dt("2026-05-04T12:45:00Z"));
    assert_eq!(invitation.updated_at, dt("2026-05-04T13:00:00.123456789Z"));
    assert_eq!(events.len(), 1);

    let mut stale = receipt.clone();
    stale.revision = 1;
    stale.confirmed_at = None;
    stale.outcome = CreateOutcome::Blocked {
        reason: crate::delivery::BlockedReason::InstallationUnavailable,
    };
    s.project_delivery(&stale.clone().into()).await.unwrap();
    let mut equal_conflict = stale.clone();
    equal_conflict.revision = 2;
    let mut identity_conflict = stale;
    identity_conflict.command.approval_id = "different approval".into();
    let mut audit_conflict = receipt.clone();
    audit_conflict.revision = 3;
    audit_conflict.confirmed_at = Some(dt("2026-05-04T14:00:00Z"));
    for conflict in [equal_conflict, identity_conflict, audit_conflict] {
        assert!(matches!(
            s.project_delivery(&conflict.into()).await,
            Err(Error::ProjectionInvariant(_))
        ));
        assert_eq!(
            s.list_delivery_for_request(request.id).await.unwrap(),
            vec![crate::delivery::DeliverySnapshot::from(receipt.clone())]
        );
        assert_eq!(
            legacy_read(&s, request.id, 10, 1).await.unwrap(),
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

    // Canonical-key replay cannot change the immutable target/command. Raw
    // relational restore corruption is separately exercised by adapter tests.
    let mut conflict = receipt.clone();
    conflict.command.repo_full_name = "other/repo".into();
    assert!(matches!(
        s.project_delivery(&conflict.into()).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_delivery(&receipt.command.delivery_key())
            .await
            .unwrap(),
        Some(receipt.into())
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
    use crate::account::{App, Installation};
    let mut first = Installation::repo(sample_account(1, 9001, "acme1"));
    s.put_app_installation(&first).await.unwrap();
    first.revision += 1;
    first.account.uninstalled_at = Some(dt("2026-05-04T18:00:00Z"));
    s.put_app_installation(&first).await.unwrap();
    assert!(
        s.active_app_installation(App::Repo, 9001)
            .await
            .unwrap()
            .is_none()
    );
    let mut reinstall = Installation::repo(sample_account(2, 9001, "acme1"));
    reinstall.account.installed_at = dt("2026-05-05T00:00:00Z");
    s.put_app_installation(&reinstall).await.unwrap();
    assert_eq!(
        s.active_app_installation(App::Repo, 9001).await.unwrap(),
        Some(reinstall)
    );
    assert_eq!(s.app_installation(App::Repo, 1).await.unwrap(), Some(first));
}

/// An integer identity is stored exactly or refused, never silently rounded:
/// D1 binds JavaScript doubles, exact only up to 2^53.
async fn scenario_integers_stored_exactly_or_refused<S: Storage>(s: S) {
    let id = (1u64 << 53) + 1;
    let account = sample_account(id, 9012, "acme12");
    assert!(matches!(
        s.put_app_installation(&crate::account::Installation::repo((account).clone()))
            .await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    assert!(
        s.active_app_installation(crate::account::App::Repo, 9012)
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap()
            .is_none()
    );
}

async fn scenario_invitation_link_lifecycle<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9002, "acme2")).clone(),
    ))
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
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9005, "acme5")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(704, "creator")).await.unwrap();
    s.upsert_user(&sample_user(804, "asker")).await.unwrap();

    let link = sample_link(9005, 1, 704);
    let req = sample_request(link.id, 804);
    seed(&s, &link, std::slice::from_ref(&req), 1).await;

    let g = GithubInvitation {
        delivery_generation: 1,
        id: GithubInvitationId::new(),
        invitation_request_id: req.id,
        repo_id: 10,
        github_invitation_id: Some(99001),
        state: InvitationState::Sent,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:01:00Z"),
    };
    seed_legacy(&s, &link, &req, &g).await;

    assert_eq!(
        legacy_read(&s, req.id, 10, 1).await.unwrap(),
        Some(g.clone())
    );
    assert_eq!(
        s.list_delivery_for_request(req.id).await.unwrap(),
        vec![delivery_fixture(&link, &req, &g)]
    );

    let mut settled = delivery_fixture(&link, &req, &g);
    settled.revision += 1;
    settled.settlement = Some(crate::delivery::Settlement {
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
    });
    s.project_delivery(&settled).await.unwrap();

    let accepted = legacy_read(&s, req.id, 10, 1).await.unwrap().unwrap();
    assert_eq!(accepted.state, InvitationState::Accepted);
    assert_eq!(
        s.list_delivery_for_request(req.id).await.unwrap(),
        vec![settled]
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
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9007, "acme7")).clone(),
    ))
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
    assert!(
        s.retain_attempt_continuation("scope", "invalid", "invalid", "payload", i64::MAX, 300)
            .await
            .is_err()
    );
    assert_eq!(
        s.get_attempt_continuation("scope", "a1", 100)
            .await
            .unwrap()
            .map(|v| v.payload),
        Some("p1".into()),
        "invalid integers must fail before cleanup writes"
    );
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
    assert!(!s.delivery_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), Some(1));
    assert!(s.delivery_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), None);
    s.reject_delivery_attempt(&command, 1).await.unwrap();
    assert!(!s.delivery_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), Some(2));
    s.reject_delivery_attempt(&command, 1).await.unwrap();
    assert!(s.delivery_attempt_exists(&command).await.unwrap());
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), None);
    // A DELETE has its own fence; the consumed PUT does not authorize or block
    // the initial cancellation. Lost result acknowledgements never reauthorize.
    let cancel = super::delivery_attempts::CancelCommand {
        create: command.clone(),
        upstream_id: 500,
        reason: crate::delivery::SettlementReason::LinkRevoked,
    };
    assert!(!s.cancellation_attempt_exists(&cancel).await.unwrap());
    let wire = serde_json::to_string(&cancel).unwrap();
    assert_eq!(
        serde_json::from_str::<super::delivery_attempts::CancelCommand>(&wire).unwrap(),
        cancel
    );
    assert_eq!(
        s.claim_cancellation_attempt(&cancel).await.unwrap(),
        Some(1)
    );
    assert_eq!(s.claim_cancellation_attempt(&cancel).await.unwrap(), None);
    s.reject_delivery_attempt(&command, 2).await.unwrap();
    assert_eq!(s.claim_cancellation_attempt(&cancel).await.unwrap(), None);
    let mut changed_cancel = cancel.clone();
    changed_cancel.upstream_id += 1;
    assert!(matches!(
        s.claim_cancellation_attempt(&changed_cancel).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    s.reject_cancellation_attempt(&changed_cancel, 1)
        .await
        .unwrap();
    assert_eq!(s.claim_cancellation_attempt(&cancel).await.unwrap(), None);
    assert!(s.cancellation_attempt_exists(&cancel).await.unwrap());
    s.reject_cancellation_attempt(&cancel, 1).await.unwrap();
    assert!(!s.cancellation_attempt_exists(&cancel).await.unwrap());
    assert_eq!(
        s.claim_cancellation_attempt(&cancel).await.unwrap(),
        Some(2)
    );
    s.reject_cancellation_attempt(&cancel, 1).await.unwrap();
    assert_eq!(s.claim_cancellation_attempt(&cancel).await.unwrap(), None);
    changed_cancel = cancel.clone();
    changed_cancel.reason = crate::delivery::SettlementReason::OwnExpiry;
    assert!(matches!(
        s.claim_cancellation_attempt(&changed_cancel).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    changed_cancel = cancel.clone();
    changed_cancel.create.repo_id += 1;
    assert!(matches!(
        s.claim_cancellation_attempt(&changed_cancel).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    changed_cancel = cancel.clone();
    changed_cancel.create.invitation_id = Some(GithubInvitationId::new());
    assert!(matches!(
        s.claim_cancellation_attempt(&changed_cancel).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    // Cancelling generation 2 cannot release the independently re-claimed PUT.
    assert_eq!(s.claim_delivery_attempt(&command).await.unwrap(), Some(3));
    s.reject_cancellation_attempt(&cancel, 2).await.unwrap();
    assert!(s.delivery_attempt_exists(&command).await.unwrap());
    changed_cancel = cancel;
    changed_cancel.upstream_id = 0;
    assert!(matches!(
        s.claim_cancellation_attempt(&changed_cancel).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    let changed = crate::delivery::CreateCommand {
        budget_attribution: Default::default(),
        budget_share_pct: 50,
        repo_id: 11,
        ..command.clone()
    };
    assert!(matches!(
        s.claim_delivery_attempt(&changed).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    // V2 keys do not carry an invitation ULID. Complete immutable inputs still
    // fence conflicts, and stale attempt acknowledgements cannot open a retry.
    let mut canonical = command.clone();
    canonical.invitation_id = None;
    canonical.delivery_generation = 2;
    assert_eq!(s.claim_delivery_attempt(&canonical).await.unwrap(), Some(1));
    let mut wrong = canonical.clone();
    wrong.requester_id += 1;
    assert!(matches!(
        s.delivery_attempt_exists(&wrong).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    s.reject_delivery_attempt(&wrong, 1).await.unwrap();
    assert!(s.delivery_attempt_exists(&canonical).await.unwrap());
    assert!(matches!(
        s.claim_delivery_attempt(&wrong).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    s.reject_delivery_attempt(&canonical, 1).await.unwrap();
    assert_eq!(s.claim_delivery_attempt(&canonical).await.unwrap(), Some(2));
    s.reject_delivery_attempt(&canonical, 1).await.unwrap();
    assert!(s.delivery_attempt_exists(&canonical).await.unwrap());
    let mut separate = canonical.clone();
    separate.repo_id += 1;
    assert_eq!(s.claim_delivery_attempt(&separate).await.unwrap(), Some(1));
    s.reject_delivery_attempt(&separate, 1).await.unwrap();
    assert!(s.delivery_attempt_exists(&canonical).await.unwrap());
}

/// Repository history follows the canonical request and delivery generation,
/// never an upstream invitation number or a historical member-event match.
async fn scenario_repository_projection_identity<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(sample_account(
        1, 9010, "acme10",
    )))
    .await
    .unwrap();
    s.upsert_user(&sample_user(710, "creator")).await.unwrap();
    s.upsert_user(&sample_user(810, "asker")).await.unwrap();
    let link = sample_link(9010, 1, 710);
    let first = sample_request(link.id, 810);
    let second = sample_request(link.id, 810);
    seed(&s, &link, &[first.clone(), second.clone()], 1).await;
    let mut projected = Vec::new();
    for (request, generation) in [(&first, 1), (&first, 2), (&second, 1)] {
        let invitation = GithubInvitation {
            delivery_generation: generation,
            id: GithubInvitationId::new(),
            invitation_request_id: request.id,
            repo_id: 10,
            // A repeated upstream observation cannot retarget a request/generation.
            github_invitation_id: Some(99010),
            state: InvitationState::Sent,
            error_message: None,
            created_at: dt("2026-05-04T13:00:00Z"),
            updated_at: dt("2026-05-04T13:00:00Z"),
        };
        let mut snapshot = delivery_fixture(&link, request, &invitation);
        snapshot.create.command.delivery_generation = generation;
        s.project_delivery(&snapshot).await.unwrap();
        projected.push(snapshot);
    }
    let mut first_rows = s.list_delivery_for_request(first.id).await.unwrap();
    first_rows.sort_by_key(|row| row.create.command.delivery_generation);
    assert_eq!(first_rows, projected[..2]);
    assert_eq!(
        s.list_delivery_for_request(second.id).await.unwrap(),
        vec![projected[2].clone()]
    );
    assert!(
        s.list_delivery_for_request(RequestId::new())
            .await
            .unwrap()
            .is_empty()
    );
    // A stale receipt arriving after a newer observation preserves the exact row.
    let mut newer = projected[1].clone();
    newer.revision += 1;
    s.project_delivery(&newer).await.unwrap();
    s.project_delivery(&projected[1]).await.unwrap();
    let rows = s.list_delivery_for_request(first.id).await.unwrap();
    assert!(rows.contains(&newer));
    assert_eq!(rows.len(), 2);
}

/// A settlement whose audit ID holds different content is a retryable storage
/// failure that changes nothing; stale evidence after settling is a no-op.
async fn scenario_settlement_audit_conflict<S: Storage + ProjectionStorage>(s: S) {
    use super::AuditPosition;
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9011, "acme11")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(711, "creator")).await.unwrap();
    s.upsert_user(&sample_user(811, "asker")).await.unwrap();
    let link = sample_link(9011, 1, 711);
    let request = sample_request(link.id, 811);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let sent = GithubInvitation {
        delivery_generation: 1,
        id: GithubInvitationId::new(),
        invitation_request_id: request.id,
        repo_id: 10,
        github_invitation_id: Some(99011),
        state: InvitationState::Sent,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:00:00Z"),
    };
    seed_legacy(&s, &link, &request, &sent).await;
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
    let settlement = |event: AuditEvent| {
        let mut snapshot = delivery_fixture(&link, &request, &sent);
        snapshot.revision += 1;
        snapshot.settlement = Some(crate::delivery::Settlement {
            state: InvitationState::Accepted,
            event,
        });
        snapshot
    };
    assert!(matches!(
        s.project_delivery(&settlement(event.clone())).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        legacy_read(&s, request.id, 10, 1).await.unwrap(),
        Some(sent.clone())
    );
    let winner = AuditEvent {
        id: AuditEventId::new(),
        ..event.clone()
    };
    s.project_delivery(&settlement(winner.clone()))
        .await
        .unwrap();
    s.project_delivery(&settlement(winner.clone()))
        .await
        .unwrap();
    assert!(
        s.project_delivery(&settlement(AuditEvent {
            id: AuditEventId::new(),
            ..winner.clone()
        }))
        .await
        .is_err()
    );
    assert_eq!(
        legacy_read(&s, request.id, 10, 1)
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
            .filter(|e| e.target_id == sent.id.to_string()
                && e.event_type == EventType::InvitationAccepted)
            .collect::<Vec<_>>(),
        [&winner]
    );
}

/// A complete owner envelope for storage conformance fixtures.
pub fn delivery_fixture(
    link: &InvitationLink,
    request: &InvitationRequest,
    row: &GithubInvitation,
) -> crate::delivery::DeliverySnapshot {
    crate::delivery::CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: crate::delivery::CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            delivery_generation: 1,
            invitation_id: Some(row.id),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "fixture-approval".into(),
            account_id: link.account_id,
            installation_id: link.installation_id,
            requester_id: request.requester_id,
            repo_id: row.repo_id,
            repo_full_name: "acme/api".into(),
            permission: link
                .targets
                .repos
                .iter()
                .find(|target| target.repo_id == row.repo_id)
                .unwrap()
                .grant
                .into(),
            approved_at: row.created_at,
        },
        outcome: match row.github_invitation_id {
            Some(upstream_id) => crate::delivery::CreateOutcome::Created { upstream_id },
            None => crate::delivery::CreateOutcome::Planned,
        },
        revision: 1,
        confirmed_at: row.github_invitation_id.map(|_| row.updated_at),
    }
    .into()
}

/// A refused insert names the constraint it hit, not just "the write failed":
/// re-onboarding branches on [`ConflictKind::DuplicateId`], and only the
/// partial unique index tells an account's second *active* installation apart.
async fn scenario_insert_conflict_kinds<S: Storage + ProjectionStorage>(s: S) {
    use super::{ConflictKind, Error};
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9013, "acme13")).clone(),
    ))
    .await
    .unwrap();
    let second_active = s
        .put_app_installation(&crate::account::Installation::repo(
            (sample_account(2, 9013, "acme13")).clone(),
        ))
        .await;
    assert!(
        matches!(
            second_active,
            Err(Error::Conflict(ConflictKind::DuplicateActiveInstallation))
        ),
        "{second_active:?}"
    );
    // The same app/installation identity cannot move to a different account.
    let reused_id = s
        .put_app_installation(&crate::account::Installation::repo(
            (sample_account(1, 9014, "acme14")).clone(),
        ))
        .await;
    assert!(
        matches!(reused_id, Err(Error::ProjectionInvariant(_))),
        "{reused_id:?}"
    );

    s.upsert_user(&sample_user(713, "creator")).await.unwrap();
    s.upsert_user(&sample_user(813, "asker")).await.unwrap();
    let link = sample_link(9013, 1, 713);
    let request = sample_request(link.id, 813);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let mut planned = delivery_fixture(
        &link,
        &request,
        &GithubInvitation {
            id: GithubInvitationId::new(),
            delivery_generation: 1,
            invitation_request_id: request.id,
            repo_id: 10,
            github_invitation_id: None,
            state: InvitationState::Sending,
            error_message: None,
            created_at: request.created_at,
            updated_at: request.created_at,
        },
    );
    planned.create.outcome = crate::delivery::CreateOutcome::Planned;
    planned.create.confirmed_at = None;
    s.project_delivery(&planned).await.unwrap();
    s.project_delivery(&planned).await.unwrap();
    let mut orphan = planned.clone();
    orphan.create.command.request_id = RequestId::new();
    orphan.create.command.invitation_id = Some(GithubInvitationId::new());
    assert!(matches!(
        s.project_delivery(&orphan).await,
        Err(Error::ProjectionDependency)
    ));
}

async fn scenario_installation_revision_fencing<S: Storage>(s: S) {
    use crate::account::{App, Installation};
    assert!(s.app_installation(App::Repo, 1).await.unwrap().is_none());
    let initial = Installation::repo(sample_account(1, 9015, "acme15"));
    s.put_app_installation(&initial).await.unwrap();
    let mut changed = initial.clone();
    changed.revision += 1;
    changed.account.selected_repos = SelectedRepos::Subset(vec![10]);
    s.put_app_installation(&changed).await.unwrap();
    s.put_app_installation(&initial).await.unwrap();
    assert_eq!(
        s.app_installation(App::Repo, 1).await.unwrap(),
        Some(changed.clone())
    );
    let mut conflict = changed.clone();
    conflict.account.selected_repos = SelectedRepos::All;
    assert!(matches!(
        s.put_app_installation(&conflict).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    changed.revision += 1;
    changed.account.uninstalled_at = Some(dt("2026-05-04T18:00:00Z"));
    s.put_app_installation(&changed).await.unwrap();
    s.put_app_installation(&changed).await.unwrap();
    conflict = changed.clone();
    conflict.revision += 1;
    conflict.account.uninstalled_at = None;
    assert!(matches!(
        s.put_app_installation(&conflict).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.app_installation(App::Repo, 1).await.unwrap(),
        Some(changed)
    );
}

async fn scenario_console_scope<S: Storage + ProjectionStorage>(s: S) {
    use super::{AuditPosition, console_scope::ConsoleScope};
    use crate::targets::{MemberGrant, OrganizationTarget};
    let account = 9040;
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, account, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(2, account + 1, "other")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(7, "admin")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let visible = sample_link(account, 1, 7);
    let mut hidden = sample_link(account, 1, 7);
    hidden.targets.repos[0].repo_id = 11;
    hidden.targets.repos[0].repo_full_name = "acme/hidden".into();
    let mut mixed = sample_link(account, 1, 7);
    mixed.targets.repos.push(hidden.targets.repos[0].clone());
    s.put_app_installation(&sample_org_installation(1, account, "acme"))
        .await
        .unwrap();
    let mut org = sample_link(account, 1, 7);
    org.organization_installation_id = Some(1);
    org.targets.organization = Some(OrganizationTarget {
        org_id: account,
        org_login: "acme".into(),
        grant: MemberGrant::Member,
    });
    let mut org_only = sample_link(account, 1, 7);
    org_only.targets = org.targets.clone();
    org_only.targets.repos.clear();
    let mut other = sample_link(account + 1, 2, 7);
    other.targets.repos[0].repo_full_name = "other/api".into();
    for link in [&visible, &hidden, &mixed, &org, &org_only, &other] {
        seed(&s, link, &[], 1).await;
    }
    let scope = ConsoleScope::repo_admin(account, [10, 10]).unwrap();
    let owner = ConsoleScope::account(account).unwrap();
    let all_repos = ConsoleScope::repo_admin(account, [10, 11]).unwrap();
    let none = ConsoleScope::repo_admin(account, []).unwrap();
    assert_eq!(
        s.scoped_list_invitation_links_for_account(&scope)
            .await
            .unwrap(),
        vec![visible.clone()]
    );
    assert_eq!(
        s.scoped_list_invitation_links_for_account(&all_repos)
            .await
            .unwrap()
            .len(),
        3,
        "org targets never grant delegated access"
    );
    assert_eq!(
        s.scoped_list_invitation_links_for_account(&owner)
            .await
            .unwrap()
            .len(),
        5
    );
    assert!(
        s.scoped_list_invitation_links_for_account(&none)
            .await
            .unwrap()
            .is_empty()
    );
    let first = s
        .distinct_console_repo_targets(account, None, 1)
        .await
        .unwrap();
    assert_eq!(first.rows[0].id, 10);
    assert_eq!(first.next, Some(10));
    let second = s
        .distinct_console_repo_targets(account, first.next, 1)
        .await
        .unwrap();
    assert_eq!(second.rows[0].id, 11);
    assert_eq!(second.next, None);
    assert_eq!(
        s.distinct_console_repo_targets(account, None, 501)
            .await
            .unwrap()
            .rows
            .len(),
        2
    );
    assert!(
        s.distinct_console_repo_targets(account, None, 0)
            .await
            .is_err()
    );
    assert!(
        s.distinct_console_repo_targets(account, None, 502)
            .await
            .is_err()
    );
    assert!(ConsoleScope::repo_admin(account, [0]).is_err());
    assert!(ConsoleScope::account(super::integer::MAX_EXACT + 1).is_err());
    assert!(ConsoleScope::repo_admin(account, 1..=5001).is_err());
    let mut visible_requests = Vec::new();
    for (link, offset) in [(&visible, 100), (&hidden, 0)] {
        for n in 0..27 {
            let mut request = sample_request(link.id, 8);
            request.created_at += chrono::Duration::seconds(offset + n);
            seed(&s, link, std::slice::from_ref(&request), 1).await;
            s.audit(&AuditEvent {
                id: AuditEventId::new(),
                account_id: account,
                occurred_at: dt("2026-05-04T16:00:00Z")
                    + chrono::Duration::seconds(if offset == 0 { 100 + n } else { n }),
                event_type: EventType::RequestCreated,
                actor_kind: ActorKind::User,
                actor_id: Some(8),
                target_kind: TargetKind::InvitationRequest,
                target_id: request.id.to_string(),
                metadata: serde_json::json!({"link_id":other.id}),
                request_id: None,
            })
            .await
            .unwrap();
            if offset != 0 {
                visible_requests.push(request);
            }
        }
    }
    assert_eq!(
        s.scoped_count_pending_requests_for_account(&scope)
            .await
            .unwrap(),
        27
    );
    assert_eq!(
        s.scoped_count_pending_requests_for_account(&owner)
            .await
            .unwrap(),
        54
    );
    assert_eq!(
        s.scoped_count_pending_requests_for_account(&none)
            .await
            .unwrap(),
        0
    );
    let page = s
        .scoped_list_pending_requests_page(&scope, None)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 25);
    assert!(page.rows.iter().all(|r| r.link_id == Some(visible.id)));
    let last = s
        .scoped_list_pending_requests_page(&scope, page.next)
        .await
        .unwrap();
    assert_eq!(last.rows.len(), 2);
    assert!(last.next.is_none());
    assert!(
        s.scoped_list_pending_requests_page(&none, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    let audit = s
        .scoped_list_audit_events(
            &scope,
            Some(EventType::RequestCreated),
            AuditPosition::Latest,
        )
        .await
        .unwrap();
    assert_eq!(audit.events.len(), 25);
    assert!(!audit.has_newer);
    assert!(audit.has_older);
    assert!(audit.events.iter().all(|e| {
        visible_requests
            .iter()
            .any(|r| r.id.to_string() == e.target_id)
    }));
    let older = s
        .scoped_list_audit_events(
            &scope,
            Some(EventType::RequestCreated),
            AuditPosition::Before(audit.events.last().unwrap().into()),
        )
        .await
        .unwrap();
    assert_eq!(older.events.len(), 2);
    assert!(older.has_newer);
    assert!(!older.has_older);
    let newer = s
        .scoped_list_audit_events(
            &scope,
            Some(EventType::RequestCreated),
            AuditPosition::After((&older.events[0]).into()),
        )
        .await
        .unwrap();
    assert_eq!(newer.events, audit.events);
    assert!(
        s.scoped_list_audit_events(&none, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
    let invitation = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: visible_requests[0].id,
        repo_id: 10,
        delivery_generation: 1,
        github_invitation_id: Some(999),
        state: InvitationState::Sent,
        error_message: None,
        created_at: dt("2026-05-04T18:00:00Z"),
        updated_at: dt("2026-05-04T18:00:00Z"),
    };
    seed_legacy(&s, &visible, &visible_requests[0], &invitation).await;
    for (kind, target) in [
        (TargetKind::InvitationLink, visible.id.to_string()),
        (TargetKind::GithubInvitation, invitation.id.to_string()),
        (
            TargetKind::GithubInvitation,
            format!("{}:10:1", visible_requests[0].id),
        ),
        (TargetKind::Installation, "repo:1".into()),
        (TargetKind::InvitationLink, other.id.to_string()),
    ] {
        s.audit(&AuditEvent {
            id: AuditEventId::new(),
            account_id: account,
            occurred_at: dt("2026-05-04T19:00:00Z"),
            event_type: EventType::InvitationSent,
            actor_kind: ActorKind::System,
            actor_id: None,
            target_kind: kind,
            target_id: target,
            metadata: serde_json::json!({"link_id":visible.id}),
            request_id: None,
        })
        .await
        .unwrap();
    }
    let selected = s
        .scoped_list_audit_events(
            &scope,
            Some(EventType::InvitationSent),
            AuditPosition::Latest,
        )
        .await
        .unwrap();
    assert_eq!(
        selected.events.len(),
        4,
        "canonical confirmation plus three relationally bound historical events; metadata cannot grant access"
    );
    assert!(!selected.has_older);
    assert!(!selected.has_newer);
}

async fn scenario_app_installations_and_delegation<S: Storage>(s: S) {
    use crate::account::{AccountSettings, App, Installation, InstallationPermission};
    let mut repo = Installation::repo(sample_account(501, 9017, "multi-app"));
    repo.permissions
        .insert("administration".into(), InstallationPermission::Write);
    repo.installed_by_user_id = Some(701);
    repo.installed_by_login = Some("installer".into());
    let mut org = repo.clone();
    org.app = App::Org;
    org.permissions.clear();
    org.permissions
        .insert("members".into(), InstallationPermission::Write);
    org.suspended_at = Some(dt("2026-05-05T00:00:00Z"));
    s.put_app_installation(&repo).await.unwrap();
    s.put_app_installation(&org).await.unwrap();
    assert_eq!(
        s.app_installation(App::Repo, 501).await.unwrap(),
        Some(repo.clone())
    );
    assert_eq!(
        s.app_installation(App::Org, 501).await.unwrap(),
        Some(org.clone())
    );
    assert_eq!(
        s.app_installation(crate::account::App::Repo, 501)
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap(),
        Some(repo.account.clone())
    );
    let mut conflict = repo.clone();
    conflict.account.account_id = 9999;
    assert!(matches!(
        s.put_app_installation(&conflict).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    let settings = AccountSettings {
        account_id: 9017,
        revision: 1,
        repo_admin_delegation: true,
        changed_by: 701,
        changed_at: dt("2026-05-05T00:00:00Z"),
    };
    assert!(s.account_settings(9017).await.unwrap().is_none());
    s.put_account_settings(&settings).await.unwrap();
    assert_eq!(s.account_settings(9017).await.unwrap(), Some(settings));
    org.revision += 1;
    org.account.uninstalled_at = Some(dt("2026-05-06T00:00:00Z"));
    s.put_app_installation(&org).await.unwrap();
    assert!(
        s.active_app_installation(App::Org, 9017)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        s.active_app_installation(App::Repo, 9017).await.unwrap(),
        Some(repo)
    );
}

/// Reordered deliveries cannot erase installer evidence, permissions, delegation,
/// or an uninstall tombstone; equal revisions bind the complete snapshot.
async fn scenario_app_installation_projection_order<S: Storage>(s: S) {
    use crate::account::{AccountSettings, App, Installation, InstallationPermission};
    let initial = Installation::repo(sample_account(502, 9018, "ordered-app"));
    s.put_app_installation(&initial).await.unwrap();
    let mut current = initial.clone();
    current.revision = 2;
    current.installed_by_user_id = Some(702);
    current.installed_by_login = Some("verified-installer".into());
    current
        .permissions
        .insert("administration".into(), InstallationPermission::Write);
    s.put_app_installation(&current).await.unwrap();
    s.put_app_installation(&initial).await.unwrap();
    s.put_app_installation(&current).await.unwrap();
    let mut conflict = current.clone();
    conflict.permissions.clear();
    assert!(matches!(
        s.put_app_installation(&conflict).await,
        Err(super::Error::ProjectionInvariant(_))
    ));
    let mut erased = current.clone();
    erased.revision = 3;
    erased.installed_by_user_id = None;
    erased.installed_by_login = None;
    assert!(s.put_app_installation(&erased).await.is_err());
    assert_eq!(
        s.app_installation(App::Repo, 502).await.unwrap(),
        Some(current.clone())
    );
    let mut org = current.clone();
    org.app = App::Org;
    org.revision = 1;
    org.installed_by_user_id = Some(703);
    org.installed_by_login = Some("org-installer".into());
    s.put_app_installation(&org).await.unwrap();
    for (app, expected) in [(App::Repo, &current), (App::Org, &org)] {
        assert_eq!(
            s.active_app_installation_by_login(app, "ordered-app")
                .await
                .unwrap()
                .as_ref(),
            Some(expected)
        );
        assert_eq!(
            s.latest_app_installation_by_login(app, "ordered-app")
                .await
                .unwrap()
                .as_ref(),
            Some(expected)
        );
        assert_eq!(
            s.list_active_app_installations(app).await.unwrap(),
            vec![expected.clone()]
        );
    }
    current.revision = 4;
    current.account.uninstalled_at = Some(dt("2026-05-07T00:00:00Z"));
    s.put_app_installation(&current).await.unwrap();
    s.put_app_installation(&initial).await.unwrap();
    let mut revived = current.clone();
    revived.revision = 5;
    revived.account.uninstalled_at = None;
    assert!(s.put_app_installation(&revived).await.is_err());
    assert!(
        s.active_app_installation_by_login(App::Repo, "ordered-app")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.list_active_app_installations(App::Repo)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.latest_app_installation_by_login(App::Repo, "ordered-app")
            .await
            .unwrap(),
        Some(current)
    );
    assert_eq!(
        s.active_app_installation(App::Org, 9018).await.unwrap(),
        Some(org)
    );
    let first = AccountSettings {
        account_id: 9018,
        revision: 1,
        repo_admin_delegation: true,
        changed_by: 702,
        changed_at: dt("2026-05-06T00:00:00Z"),
    };
    let second = AccountSettings {
        revision: 2,
        repo_admin_delegation: false,
        ..first.clone()
    };
    s.put_account_settings(&first).await.unwrap();
    s.put_account_settings(&second).await.unwrap();
    s.put_account_settings(&first).await.unwrap();
    s.put_account_settings(&second).await.unwrap();
    let conflict = AccountSettings {
        repo_admin_delegation: true,
        ..second.clone()
    };
    assert!(s.put_account_settings(&conflict).await.is_err());
    assert_eq!(s.account_settings(9018).await.unwrap(), Some(second));
}

async fn scenario_link_capabilities<S: Storage + ProjectionStorage>(s: S) {
    use super::link_secrets::LinkSecret;
    use super::projection::AuditIntent;
    use crate::{LinkSecretState, SecretHash};
    let link = sample_link(9020, 1, 701);
    let secret = LinkSecret {
        secret_sha256: link.secret.sha256,
        link_id: link.id,
        secret_generation: 1,
        created_at: dt("2026-05-05T00:00:00Z"),
        retired_at: None,
    };
    // Registration is an insert-only routing index, independent of projection.
    // The authority checks the current hash/generation, never this lookup alone.
    s.register_link_secret(&secret).await.unwrap();
    s.register_link_secret(&secret).await.unwrap();
    assert_eq!(
        s.resolve_link_secret(&secret.secret_sha256).await.unwrap(),
        Some(secret.clone())
    );
    assert!(
        s.get_invitation_link_by_id(link.id)
            .await
            .unwrap()
            .is_none()
    );
    let mut wrong = secret.clone();
    wrong.secret_generation = 2;
    assert!(s.register_link_secret(&wrong).await.is_err());
    wrong = secret.clone();
    wrong.link_id = InvitationLinkId::new();
    assert!(s.register_link_secret(&wrong).await.is_err());
    wrong = secret.clone();
    wrong.secret_sha256 = SecretHash::from_bytes([9; 32]);
    assert!(s.register_link_secret(&wrong).await.is_err()); // generation cannot be rebound
    wrong = secret.clone();
    wrong.secret_generation = 0;
    assert!(s.register_link_secret(&wrong).await.is_err());
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9020, "caps")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "admin")).await.unwrap();
    let initial = fixture::envelope(&link, 1);
    s.apply_transition(&initial).await.unwrap();
    // A different link and capability must not participate in this link's
    // same-generation / newer-revision invariant checks.
    let mut unrelated = sample_link(9020, 1, 701);
    unrelated.secret.sha256 = SecretHash::from_bytes([99; 32]);
    seed(&s, &unrelated, &[], 1).await;
    let mut admission = initial.clone();
    admission.link.revision = 2;
    admission.link.uses = 1;
    s.apply_transition(&admission).await.unwrap();
    let rotation = |generation: u32, revision| {
        let mut envelope = initial.clone();
        envelope.transition_id = format!("link/{}/secret/{generation}", link.id);
        envelope.link.revision = revision;
        envelope.link.secret = LinkSecretState {
            secret_generation: generation,
            sha256: SecretHash::from_bytes([generation as u8; 32]),
        };
        envelope.events = vec![AuditIntent {
            event_id: envelope.transition_id.clone(),
            kind: EventType::InvitationLinkSecretRotated,
            actor_id: Some(701),
            target_id: link.id.to_string(),
            effective_at: dt("2026-05-06T00:00:00Z"),
            evaluated_at: dt("2026-05-06T00:00:00Z"),
        }];
        envelope
    };
    let second = LinkSecret {
        secret_sha256: SecretHash::from_bytes([2; 32]),
        secret_generation: 2,
        ..secret.clone()
    };
    let third = LinkSecret {
        secret_sha256: SecretHash::from_bytes([3; 32]),
        secret_generation: 3,
        ..secret.clone()
    };
    s.register_link_secret(&second).await.unwrap();
    assert_eq!(
        s.resolve_link_secret(&second.secret_sha256).await.unwrap(),
        Some(second.clone())
    );
    let rotated = rotation(2, 3);
    s.apply_transition(&rotated).await.unwrap();
    assert!(
        s.resolve_link_secret(&secret.secret_sha256)
            .await
            .unwrap()
            .is_none()
    );
    s.register_link_secret(&secret).await.unwrap(); // retained receipt never resurrects old route
    assert!(
        s.resolve_link_secret(&secret.secret_sha256)
            .await
            .unwrap()
            .is_none()
    );
    s.register_link_secret(&third).await.unwrap();
    s.apply_transition(&initial).await.unwrap();
    s.apply_transition(&rotated).await.unwrap();
    assert_eq!(
        s.resolve_link_secret(&third.secret_sha256).await.unwrap(),
        Some(third.clone())
    );
    let newest = rotation(3, 4);
    s.apply_transition(&newest).await.unwrap();
    s.apply_transition(&rotated).await.unwrap();
    assert!(
        s.resolve_link_secret(&second.secret_sha256)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        s.resolve_link_secret(&third.secret_sha256).await.unwrap(),
        Some(third.clone())
    );
    assert_eq!(
        s.get_invitation_link_by_id(link.id)
            .await
            .unwrap()
            .unwrap()
            .secret,
        newest.link.secret
    );
    let mut regression = rotated.clone();
    regression.link.revision = 5;
    assert!(s.apply_transition(&regression).await.is_err());
    let mut same_generation = newest.clone();
    same_generation.link.revision = 5;
    same_generation.link.secret.sha256 = SecretHash::from_bytes([4; 32]);
    assert!(s.apply_transition(&same_generation).await.is_err());
    let mut revoked = newest.clone();
    revoked.link.revision = 5;
    revoked.link.revoked_at = Some(dt("2026-05-07T00:00:00Z"));
    revoked.link.revoked_by = Some(701);
    s.apply_transition(&revoked).await.unwrap();
    s.apply_transition(&initial).await.unwrap();
    assert!(
        s.resolve_link_secret(&third.secret_sha256)
            .await
            .unwrap()
            .is_none()
    );
    let audit = s
        .list_audit_events(
            9020,
            Some(EventType::InvitationLinkSecretRotated),
            super::AuditPosition::Latest,
        )
        .await
        .unwrap();
    assert_eq!(audit.events.len(), 2);
    for event in audit.events {
        let metadata = event.metadata;
        assert!(
            metadata == serde_json::json!({"secret_generation":2})
                || metadata == serde_json::json!({"secret_generation":3})
        );
    }
}

async fn scenario_heterogeneous_link_targets<S: Storage + ProjectionStorage>(s: S) {
    use crate::targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget, TeamTarget};
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9022, "acme")).clone(),
    ))
    .await
    .unwrap();
    for (id, login) in [(701, "admin"), (702, "requester")] {
        s.upsert_user(&sample_user(id, login)).await.unwrap();
    }
    s.put_app_installation(&sample_org_installation(1, 9022, "acme"))
        .await
        .unwrap();
    let mut link = sample_link(9022, 1, 701);
    link.organization_installation_id = Some(1);
    link.budget_share_pct = 17;
    link.approval_required = true;
    link.targets.organization = Some(OrganizationTarget {
        org_id: 9022,
        org_login: "acme".into(),
        grant: MemberGrant::Member,
    });
    link.targets.teams = vec![TeamTarget {
        team_id: 31,
        team_slug: "developers".into(),
        grant: MemberGrant::Member,
    }];
    link.targets.repos.push(RepoTarget {
        repo_id: 11,
        repo_full_name: "acme/admin".into(),
        grant: RepoRole::Admin,
    });
    let request = sample_request(link.id, 702);
    seed(&s, &link, &[request], 1).await;
    assert_eq!(s.link_targets(link.id).await.unwrap(), link.targets);
    assert_eq!(
        s.get_invitation_link_by_id(link.id)
            .await
            .unwrap()
            .unwrap()
            .targets,
        link.targets
    );
    assert_eq!(
        s.list_invitation_links_for_account(9022).await.unwrap()[0].targets,
        link.targets
    );
    let pending = s.pending_request_page(9022, None).await.unwrap();
    assert_eq!(pending.rows.len(), 1);
    assert_eq!(pending.rows[0].targets.as_ref(), Some(&link.targets));
}
async fn scenario_foreign_pending_generation<S: Storage + ProjectionStorage>(s: S) {
    use crate::delivery::{CreateOutcome, SettlementReason};
    use crate::targets::RepoRole;
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9021, "foreign")).clone(),
    ))
    .await
    .unwrap();
    for (id, name) in [(701, "admin"), (702, "requester")] {
        s.upsert_user(&sample_user(id, name)).await.unwrap();
    }
    let link = sample_link(9021, 1, 701);
    let mut request = sample_request(link.id, 702);
    request.state = RequestState::Approved;
    seed(&s, &link, &[request.clone()], 1).await;
    let mut snap: crate::delivery::DeliverySnapshot = crate::delivery::CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: crate::delivery::CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "approved".into(),
            account_id: 9021,
            installation_id: 1,
            requester_id: 702,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: dt("2026-05-05T00:00:00Z"),
        },
        outcome: CreateOutcome::AlreadyPending {
            upstream_id: 99,
            permission: RepoRole::Read,
        },
        revision: 1,
        confirmed_at: Some(dt("2026-05-05T00:00:00Z")),
    }
    .into();
    s.project_delivery(&snap).await.unwrap();
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap().len(),
        1
    );
    snap.tracking_stopped = Some(SettlementReason::OwnExpiry);
    snap.revision = 2;
    s.project_delivery(&snap).await.unwrap();
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap(),
        vec![snap.clone()]
    );
    assert_eq!(
        legacy_read(&s, request.id, 10, 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        InvitationState::Sent
    );
    let mut next = snap.clone();
    next.create.command.delivery_generation = 2;
    next.create.command.invitation_id = Some(GithubInvitationId::new());
    next.tracking_stopped = None;
    next.revision = 1;
    s.project_delivery(&next).await.unwrap();
    assert_eq!(
        s.list_delivery_for_request(request.id)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.create.command.delivery_generation == 2)
            .unwrap()
            .create
            .command
            .delivery_generation,
        2
    );
    let mut mutated = snap.clone();
    mutated.create.outcome = CreateOutcome::Created { upstream_id: 99 };
    mutated.tracking_stopped = None;
    mutated.revision = 3;
    assert!(s.project_delivery(&mutated).await.is_err());
    let mut resume = snap;
    resume.tracking_stopped = None;
    resume.revision = 3;
    assert!(s.project_delivery(&resume).await.is_err());
}

async fn scenario_signin_budget<S: Storage>(s: S) {
    use super::signin_budget::SigninBudgetKey;
    use std::task::Poll;
    let key = SigninBudgetKey::from_digest([7; 32]);
    let ids: Vec<_> = (0..20).map(|_| RequestId::new()).collect();
    let now = 1_000_000;
    // Poll all operations concurrently, including D1 cleanup/insert awaits.
    let mut futures: Vec<_> = ids
        .iter()
        .map(|id| Some(s.reserve_signin(key, *id, now)))
        .collect();
    let mut results = vec![None; futures.len()];
    std::future::poll_fn(|cx| {
        for (index, future) in futures.iter_mut().enumerate() {
            if let Some(active) = future
                && let Poll::Ready(result) = active.as_mut().poll(cx)
            {
                results[index] = Some(result.unwrap());
                *future = None;
            }
        }
        if futures.iter().all(Option::is_none) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    assert_eq!(
        results.iter().filter(|value| **value == Some(true)).count(),
        5
    );
    let retained = ids[results
        .iter()
        .position(|value| *value == Some(true))
        .unwrap()];
    assert!(s.reserve_signin(key, retained, now).await.unwrap());
    assert!(!s.reserve_signin(key, retained, now + 1).await.unwrap());
    assert!(
        !s.reserve_signin(key, RequestId::new(), now + 599_999)
            .await
            .unwrap()
    );
    assert!(
        s.reserve_signin(SigninBudgetKey::from_digest([8; 32]), RequestId::new(), now)
            .await
            .unwrap()
    );
    for _ in 0..5 {
        assert!(
            s.reserve_signin(key, RequestId::new(), now + 600_000)
                .await
                .unwrap()
        );
    }
    assert!(
        !s.reserve_signin(key, RequestId::new(), now + 600_000)
            .await
            .unwrap()
    );
    assert!(
        s.reserve_signin(key, RequestId::new(), i64::MAX)
            .await
            .is_err()
    );
}

async fn scenario_invite_budget_projection<
    S: super::invite_budget::InviteBudgetStorage + super::blocked_digest::DailyDigestStorage,
>(
    s: S,
) {
    super::invite_budget::conformance::run(&s).await;
}

/// Shared SQLite/D1 contract: exact ciphertext/context/retention CAS, global
/// mutation epoch and expired rows. No crypto is bypassed by the web caller.
async fn scenario_continuation_rotation<S: Storage>(s: S) {
    let initial = s.continuation_rotation_epoch().await.unwrap();
    s.retain_attempt_continuation("rotation", "one", "binding", "old", 20, 10)
        .await
        .unwrap();
    let row = s
        .continuation_rotation_page(None)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id.as_deref() == Some("one"))
        .unwrap();
    assert_eq!(row.revision, initial + 1);
    assert_eq!(row.expires_at, Some(20));
    // Reads at a later application clock do not hide retained expired rows.
    assert!(
        s.get_attempt_continuation("rotation", "one", 30)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.rewrap_attempt_continuation(&row, "new", row.revision)
            .await
            .unwrap()
    );
    let fresh = s
        .continuation_rotation_page(None)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id.as_deref() == Some("one"))
        .unwrap();
    assert_eq!(fresh.payload.as_deref(), Some("new"));
    assert_eq!(
        (&fresh.scope, &fresh.id, &fresh.binding, fresh.expires_at),
        (&row.scope, &row.id, &row.binding, row.expires_at)
    );
    assert_eq!(fresh.revision, row.revision + 1);
    // A fresh epoch does not authorize stale bytes, context or retention.
    assert!(
        !s.rewrap_attempt_continuation(&row, "stale", fresh.revision)
            .await
            .unwrap()
    );
    for field in 0..4 {
        let mut changed = fresh.clone();
        match field {
            0 => changed.scope = Some("other".into()),
            1 => changed.id = Some("other".into()),
            2 => changed.binding = Some("other".into()),
            _ => changed.expires_at = Some(21),
        };
        assert!(
            !s.rewrap_attempt_continuation(&changed, "stale", fresh.revision)
                .await
                .unwrap()
        );
    }
    assert_eq!(
        s.continuation_rotation_epoch().await.unwrap(),
        fresh.revision
    );
    s.retain_attempt_continuation("rotation", "two", "other", "peer", 40, 10)
        .await
        .unwrap();
    assert!(
        !s.rewrap_attempt_continuation(&fresh, "stale", fresh.revision)
            .await
            .unwrap()
    );
    s.release_attempt_continuation("rotation", "two")
        .await
        .unwrap();
    assert_eq!(
        s.continuation_rotation_epoch().await.unwrap(),
        fresh.revision + 2
    );
    s.release_attempt_continuation("rotation", "one")
        .await
        .unwrap();
    let empty = s.continuation_rotation_page(None).await.unwrap();
    assert_eq!(empty.len(), 1);
    assert!(empty[0].row_id.is_none());
    assert_eq!(empty[0].revision, fresh.revision + 3);
}
