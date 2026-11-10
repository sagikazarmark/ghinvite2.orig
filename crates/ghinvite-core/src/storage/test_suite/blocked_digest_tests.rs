use super::*;
use crate::delivery::{
    BlockedReason, CreateOutcome, CreateReceipt, DeliverySnapshot, SettlementReason,
};
use crate::storage::{blocked_digest::DailyDigestStorage, console_scope::ConsoleScope};
use chrono::Duration;

fn blocked(
    link: &InvitationLink,
    request: &InvitationRequest,
    generation: u64,
    reason: BlockedReason,
) -> DeliverySnapshot {
    CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: crate::delivery::CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: generation,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "digest-test".into(),
            account_id: link.account_id,
            installation_id: link.installation_id,
            requester_id: request.requester_id,
            repo_id: link.targets.repos[0].repo_id,
            repo_full_name: link.targets.repos[0].repo_full_name.clone(),
            permission: Permission::Pull,
            approved_at: request.created_at,
        },
        outcome: CreateOutcome::Blocked { reason },
        revision: 2,
        confirmed_at: None,
    }
    .into()
}
pub async fn scenario_blocked_digest<S: Storage + ProjectionStorage + DailyDigestStorage>(s: S) {
    let now = dt("2026-05-04T15:00:00Z");
    let day = now.date_naive();
    let owner = ConsoleScope::account(100).unwrap();
    assert!(
        s.scoped_blocked_digest(&owner, day)
            .await
            .unwrap()
            .is_none()
    );
    for (id, account, name) in [(1, 100, "acme"), (2, 200, "other")] {
        s.put_app_installation(&crate::account::Installation::repo(
            (sample_account(id, account, name)).clone(),
        ))
        .await
        .unwrap();
    }
    s.upsert_user(&sample_user(7, "owner")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let mut visible = sample_link(100, 1, 7);
    visible.revoked_at = Some(now);
    visible.revoked_by = Some(7);
    let mut hidden = sample_link(100, 1, 7);
    hidden.targets.repos[0].repo_id = 11;
    hidden.targets.repos[0].repo_full_name = "acme/hidden".into();
    let mut mixed = sample_link(100, 1, 7);
    mixed.targets.repos.push(hidden.targets.repos[0].clone());
    s.put_app_installation(&sample_org_installation(1, 100, "acme"))
        .await
        .unwrap();
    let mut org = sample_link(100, 1, 7);
    org.organization_installation_id = Some(1);
    org.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 100,
        org_login: "acme".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    let other = sample_link(200, 2, 7);
    let mut originals = Vec::new();
    for (i, link) in [&visible, &hidden, &mixed, &org, &other]
        .into_iter()
        .enumerate()
    {
        let request = sample_request(link.id, 8);
        seed(&s, link, std::slice::from_ref(&request), 1).await;
        let reason = if i == 0 {
            BlockedReason::InstallationUnavailable
        } else {
            BlockedReason::TargetUnavailable
        };
        let row = blocked(link, &request, 1, reason);
        s.project_delivery(&row).await.unwrap();
        originals.push((request, row));
    }
    // Stopped tracking and terminal settlement are not currently blocked work.
    let mut stopped = blocked(&visible, &originals[0].0, 2, BlockedReason::MintForbidden);
    stopped.create.outcome = CreateOutcome::AlreadyPending {
        upstream_id: 2,
        permission: crate::targets::RepoRole::Read,
    };
    stopped.create.confirmed_at = Some(now);
    stopped.revision = 3;
    stopped.tracking_stopped = Some(SettlementReason::LinkRevoked);
    s.project_delivery(&stopped).await.unwrap();
    let mut terminal = blocked(&visible, &originals[0].0, 3, BlockedReason::MintForbidden);
    terminal.create.outcome = CreateOutcome::Created { upstream_id: 3 };
    terminal.create.confirmed_at = Some(now);
    terminal.revision = 3;
    terminal.settlement = Some(crate::delivery::Settlement {
        state: InvitationState::Cancelled,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: now,
            event_type: EventType::InvitationCancelled,
            actor_kind: ActorKind::System,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: terminal
                .create
                .command
                .invitation_id
                .expect("legacy fixture identity")
                .to_string(),
            metadata: serde_json::Value::Null,
            request_id: None,
        },
    });
    s.project_delivery(&terminal).await.unwrap();
    let retained = s
        .list_delivery_for_request(originals[0].0.id)
        .await
        .unwrap();
    s.record_blocked_digest(now).await.unwrap();
    let first = s.scoped_blocked_digest(&owner, day).await.unwrap().unwrap();
    assert_eq!(first.observed_at, now);
    assert!(!first.stale_for(day));
    assert_eq!(first.counts.iter().map(|c| c.count).sum::<u64>(), 4);
    assert_eq!(first.counts.len(), 2);
    let delegate = ConsoleScope::repo_admin(100, [10]).unwrap();
    let visible_digest = s
        .scoped_blocked_digest(&delegate, day)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        visible_digest.counts,
        vec![crate::storage::operational::BlockedCount {
            kind: crate::storage::operational::SignalKind::Repo,
            reason: BlockedReason::InstallationUnavailable.into(),
            count: 1
        }],
        "revoked link remains visible, mixed/hidden/org links do not"
    );
    let all = ConsoleScope::repo_admin(100, [10, 11]).unwrap();
    assert_eq!(
        s.scoped_blocked_digest(&all, day)
            .await
            .unwrap()
            .unwrap()
            .counts
            .iter()
            .map(|c| c.count)
            .sum::<u64>(),
        3
    );
    let empty = ConsoleScope::repo_admin(100, []).unwrap();
    assert!(
        s.scoped_blocked_digest(&empty, day)
            .await
            .unwrap()
            .unwrap()
            .counts
            .is_empty()
    );
    let absent = ConsoleScope::account(300).unwrap();
    assert!(
        s.scoped_blocked_digest(&absent, day)
            .await
            .unwrap()
            .unwrap()
            .counts
            .is_empty()
    );
    assert_eq!(
        s.scoped_blocked_digest(&ConsoleScope::account(200).unwrap(), day)
            .await
            .unwrap()
            .unwrap()
            .counts
            .iter()
            .map(|c| c.count)
            .sum::<u64>(),
        1
    );
    // Facts added after the first observation cannot mutate the frozen day.
    let extra = blocked(&visible, &originals[0].0, 4, BlockedReason::IpAllowList);
    s.project_delivery(&extra).await.unwrap();
    s.record_blocked_digest(now + Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(
        s.scoped_blocked_digest(&owner, day).await.unwrap().unwrap(),
        first
    );
    let tomorrow = day.succ_opt().unwrap();
    let stale = s
        .scoped_blocked_digest(&owner, tomorrow)
        .await
        .unwrap()
        .unwrap();
    assert!(stale.stale_for(tomorrow));
    assert_eq!(stale, first);
    s.record_blocked_digest(now + Duration::days(1))
        .await
        .unwrap();
    assert_eq!(
        s.scoped_blocked_digest(&owner, tomorrow)
            .await
            .unwrap()
            .unwrap()
            .counts
            .iter()
            .map(|c| c.count)
            .sum::<u64>(),
        5
    );
    // Older days cannot be backfilled; latest retention is not clock-sensitive.
    s.record_blocked_digest(now - Duration::days(1))
        .await
        .unwrap();
    assert!(
        s.scoped_blocked_digest(&owner, day.pred_opt().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    for n in 2..=7 {
        s.record_blocked_digest(now + Duration::days(n))
            .await
            .unwrap();
    }
    assert!(
        s.scoped_blocked_digest(&owner, day)
            .await
            .unwrap()
            .is_none(),
        "eighth observation expires oldest day"
    );
    assert!(
        s.scoped_blocked_digest(&owner, tomorrow)
            .await
            .unwrap()
            .is_some(),
        "retains exactly seven UTC days"
    );
    s.record_blocked_digest(now).await.unwrap();
    assert!(
        s.scoped_blocked_digest(&owner, day)
            .await
            .unwrap()
            .is_none(),
        "clock rollback cannot recreate expired snapshot"
    );
    let mut expected = retained;
    expected.push(extra);
    let actual = s
        .list_delivery_for_request(originals[0].0.id)
        .await
        .unwrap();
    assert_eq!(actual.len(), expected.len());
    for row in expected {
        assert!(
            actual.contains(&row),
            "digest must never mutate retained receipts"
        );
    }
}
