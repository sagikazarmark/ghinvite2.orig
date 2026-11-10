use super::*;
use crate::delivery::{
    BlockedReason, CreateOutcome, CreateReceipt, DeliverySnapshot, SettlementReason,
};
use crate::storage::{Error, operational::RepositoryTelemetry};
use chrono::Duration;

fn receipt(
    link: &InvitationLink,
    request: &InvitationRequest,
    generation: u64,
    outcome: CreateOutcome,
    confirmed_at: Option<DateTime<Utc>>,
) -> CreateReceipt {
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
            approval_id: "telemetry-test".into(),
            account_id: link.account_id,
            installation_id: link.installation_id,
            requester_id: request.requester_id,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: request.created_at,
        },
        outcome,
        revision: 2,
        confirmed_at,
    }
}
fn telemetry(receipt: &CreateReceipt) -> RepositoryTelemetry {
    RepositoryTelemetry {
        request_id: receipt.command.request_id,
        target_id: receipt.command.repo_id,
        delivery_generation: receipt.command.delivery_generation,
        account_id: receipt.command.account_id,
        unknown_since: None,
        last_observed_at: None,
    }
}
pub async fn scenario_repository_telemetry<S: Storage + ProjectionStorage>(s: S) {
    let now = dt("2026-05-04T15:00:00Z");
    assert_eq!(
        s.delivery_signals(now)
            .await
            .unwrap()
            .counts
            .repository_deliveries,
        0
    );
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(7, "owner")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let link = sample_link(100, 1, 7);
    let request = sample_request(link.id, 8);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let unknown = receipt(&link, &request, 1, CreateOutcome::OutcomeUnknown, None);
    let mut first = telemetry(&unknown);
    first.unknown_since = Some(now - Duration::hours(1));
    assert_eq!(first.key(), unknown.command.delivery_key());
    assert!(matches!(
        s.project_repository_telemetry(&first).await,
        Err(Error::ProjectionDependency)
    ));
    s.project_delivery(&unknown.clone().into()).await.unwrap();
    s.project_repository_telemetry(&first).await.unwrap();
    s.project_repository_telemetry(&first).await.unwrap();
    s.project_repository_telemetry(&telemetry(&unknown))
        .await
        .unwrap();
    let before = s.list_delivery_for_request(request.id).await.unwrap();
    for bad in [
        RepositoryTelemetry {
            account_id: 200,
            ..first.clone()
        },
        RepositoryTelemetry {
            unknown_since: Some(request.created_at - Duration::nanoseconds(1)),
            ..first.clone()
        },
        RepositoryTelemetry {
            last_observed_at: Some(request.created_at - Duration::nanoseconds(1)),
            ..first.clone()
        },
        RepositoryTelemetry {
            unknown_since: Some(now),
            ..first.clone()
        },
        RepositoryTelemetry {
            target_id: 0,
            ..first.clone()
        },
        RepositoryTelemetry {
            delivery_generation: 0,
            ..first.clone()
        },
        RepositoryTelemetry {
            account_id: crate::storage::integer::MAX_EXACT + 1,
            ..first.clone()
        },
    ] {
        assert!(matches!(
            s.project_repository_telemetry(&bad).await,
            Err(Error::ProjectionInvariant(_))
        ));
    }
    for bad in [
        RepositoryTelemetry {
            request_id: RequestId::new(),
            ..first.clone()
        },
        RepositoryTelemetry {
            target_id: 11,
            ..first.clone()
        },
        RepositoryTelemetry {
            delivery_generation: 99,
            ..first.clone()
        },
    ] {
        assert!(matches!(
            s.project_repository_telemetry(&bad).await,
            Err(Error::ProjectionDependency)
        ));
    }
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap(),
        before
    );
    let unknown_missing = receipt(&link, &request, 2, CreateOutcome::OutcomeUnknown, None);
    let unknown_future = receipt(&link, &request, 3, CreateOutcome::OutcomeUnknown, None);
    s.project_delivery(&unknown_missing.into()).await.unwrap();
    s.project_delivery(&unknown_future.clone().into())
        .await
        .unwrap();
    s.project_repository_telemetry(&RepositoryTelemetry {
        unknown_since: Some(now + Duration::seconds(1)),
        ..telemetry(&unknown_future)
    })
    .await
    .unwrap();
    let sent = now - Duration::hours(2);
    let pending = receipt(
        &link,
        &request,
        4,
        CreateOutcome::Created { upstream_id: 4 },
        Some(sent),
    );
    let foreign = receipt(
        &link,
        &request,
        5,
        CreateOutcome::AlreadyPending {
            upstream_id: 5,
            permission: crate::targets::RepoRole::Read,
        },
        Some(sent - Duration::nanoseconds(1)),
    );
    let observed = receipt(
        &link,
        &request,
        6,
        CreateOutcome::Created { upstream_id: 6 },
        Some(sent),
    );
    let future = receipt(
        &link,
        &request,
        7,
        CreateOutcome::Created { upstream_id: 7 },
        Some(sent),
    );
    for row in [&pending, &foreign, &observed, &future] {
        s.project_delivery(&row.clone().into()).await.unwrap();
    }
    for observation in [now - Duration::minutes(30), now - Duration::minutes(60)] {
        s.project_repository_telemetry(&RepositoryTelemetry {
            last_observed_at: Some(observation),
            ..telemetry(&observed)
        })
        .await
        .unwrap();
    }
    s.project_repository_telemetry(&telemetry(&observed))
        .await
        .unwrap();
    s.project_repository_telemetry(&RepositoryTelemetry {
        last_observed_at: Some(now + Duration::minutes(1)),
        ..telemetry(&future)
    })
    .await
    .unwrap();
    // Planned work has no sent timestamp and must not masquerade as pending.
    // Historical raw Created+NULL sent_at preservation is tested at migration0023.
    s.project_delivery(&receipt(&link, &request, 8, CreateOutcome::Planned, None).into())
        .await
        .unwrap();
    let stopped = receipt(
        &link,
        &request,
        9,
        CreateOutcome::AlreadyPending {
            upstream_id: 9,
            permission: crate::targets::RepoRole::Read,
        },
        Some(sent),
    );
    let mut stopped = DeliverySnapshot::from(stopped);
    stopped.revision = 3;
    stopped.tracking_stopped = Some(SettlementReason::LinkRevoked);
    s.project_delivery(&stopped).await.unwrap();
    let mut settled = DeliverySnapshot::from(receipt(
        &link,
        &request,
        10,
        CreateOutcome::Created { upstream_id: 10 },
        Some(sent),
    ));
    settled.revision = 3;
    settled.settlement = Some(crate::delivery::Settlement {
        state: InvitationState::Accepted,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: now,
            event_type: EventType::InvitationAccepted,
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: settled
                .create
                .command
                .invitation_id
                .expect("legacy fixture identity")
                .to_string(),
            metadata: serde_json::Value::Null,
            request_id: None,
        },
    });
    s.project_delivery(&settled).await.unwrap();
    for (generation, reason) in [
        (11, BlockedReason::InstallationUnavailable),
        (12, BlockedReason::InstallationUnavailable),
        (13, BlockedReason::InstallationPermissions),
    ] {
        s.project_delivery(
            &receipt(
                &link,
                &request,
                generation,
                CreateOutcome::Blocked { reason },
                None,
            )
            .into(),
        )
        .await
        .unwrap();
    }
    let retained = s.list_delivery_for_request(request.id).await.unwrap();
    s.project_repository_telemetry(&RepositoryTelemetry {
        last_observed_at: Some(now),
        ..telemetry(&settled.create)
    })
    .await
    .unwrap();
    s.project_delivery(&pending.clone().into()).await.unwrap();
    assert_eq!(
        s.list_delivery_for_request(request.id).await.unwrap(),
        retained
    );
    let report = s.delivery_signals(now).await.unwrap();
    assert_eq!(report.counts.repository_deliveries, 13);
    assert_eq!(report.counts.unknown.total, 3);
    assert_eq!(report.counts.unknown.over_threshold, 0);
    assert_eq!(report.counts.unknown.oldest_age_seconds, Some(3600));
    assert_eq!(report.counts.unknown.missing_timestamp, 1);
    assert_eq!(report.counts.unknown.future_timestamp, 1);
    assert_eq!(report.counts.pending.total, 4);
    assert_eq!(report.counts.pending.over_threshold, 1);
    assert_eq!(report.counts.pending.oldest_age_seconds, Some(7200));
    assert_eq!(report.counts.pending.missing_timestamp, 0);
    assert_eq!(report.counts.pending.future_timestamp, 1);
    assert!(report.incomplete());
    assert_eq!(report.by_account.len(), 1);
    assert_eq!(report.by_account[0].account_id, 100);
    assert_eq!(
        report
            .counts
            .blocked
            .iter()
            .find(|r| r.reason == BlockedReason::InstallationUnavailable.into())
            .unwrap()
            .count,
        2
    );
    assert_eq!(
        report
            .counts
            .blocked
            .iter()
            .find(|r| r.reason == BlockedReason::InstallationPermissions.into())
            .unwrap()
            .count,
        1
    );
    let after = s
        .delivery_signals(now + Duration::nanoseconds(1))
        .await
        .unwrap();
    assert_eq!(after.counts.unknown.over_threshold, 1);
    assert_eq!(after.counts.pending.over_threshold, 2);
    // Move all other observations forward, exposing the retained maximum on #6.
    for row in [&pending, &foreign] {
        s.project_repository_telemetry(&RepositoryTelemetry {
            last_observed_at: Some(now),
            ..telemetry(row)
        })
        .await
        .unwrap();
    }
    let report = s.delivery_signals(now).await.unwrap();
    assert_eq!(report.counts.pending.oldest_age_seconds, Some(1800));
    assert_eq!(report.counts.pending.over_threshold, 0);
    // Account groups cannot merge numeric-identical targets across accounts.
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(2, 200, "other")).clone(),
    ))
    .await
    .unwrap();
    let link = sample_link(200, 2, 7);
    let request = sample_request(link.id, 8);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    s.project_delivery(
        &receipt(
            &link,
            &request,
            1,
            CreateOutcome::Blocked {
                reason: BlockedReason::InstallationUnavailable,
            },
            None,
        )
        .into(),
    )
    .await
    .unwrap();
    let report = s.delivery_signals(now).await.unwrap();
    assert_eq!(report.counts.repository_deliveries, 14);
    assert_eq!(
        report
            .by_account
            .iter()
            .map(|a| a.account_id)
            .collect::<Vec<_>>(),
        vec![100, 200]
    );
    assert_eq!(report.by_account[1].counts.blocked[0].count, 1);
}
