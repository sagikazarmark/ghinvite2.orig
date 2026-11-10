use super::*;
use crate::{
    org_delivery::*,
    storage::{
        Error,
        blocked_digest::DailyDigestStorage,
        console_scope::ConsoleScope,
        operational::{OperationalBlockedReason, SignalKind},
        organization_telemetry::{OrganizationStepTelemetry, OrganizationTelemetryStep as Step},
    },
};
use chrono::Duration;

pub async fn scenario_organization_telemetry<
    S: Storage + ProjectionStorage + DailyDigestStorage,
>(
    s: S,
) {
    let now = dt("2026-05-04T16:00:00Z");
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&sample_org_installation(1, 100, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(7, "owner")).await.unwrap();
    let mut link = sample_link(100, 1, 7);
    link.organization_installation_id = Some(1);
    link.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 100,
        org_login: "acme".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    link.targets.teams = [5, 6, 7]
        .into_iter()
        .map(|team_id| crate::targets::TeamTarget {
            team_id,
            team_slug: format!("team-{team_id}"),
            grant: crate::targets::MemberGrant::Member,
        })
        .collect();
    let request = sample_request(link.id, 7);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let command = OrganizationCreateCommand {
        budget_attribution: Default::default(),
        batch: None,
        access_lifetime_seconds: None,
        invitation_id: Some(GithubInvitationId::new()),
        link_id: Some(link.id),
        request_id: request.id,
        approval_id: "telemetry".into(),
        account_id: 100,
        installation_id: 1,
        requester_id: 7,
        organization: link.targets.organization.clone().unwrap(),
        teams: link.targets.teams.clone(),
        delivery_generation: 1,
        approved_at: request.created_at,
        budget_share_pct: 50,
    };
    let telemetry = OrganizationStepTelemetry {
        request_id: request.id,
        org_id: 100,
        delivery_generation: 1,
        account_id: 100,
        step: Step::Organization,
        unknown_since: Some(now - Duration::hours(1)),
        last_observed_at: None,
    };
    assert!(matches!(
        s.project_organization_telemetry(&telemetry).await,
        Err(Error::ProjectionDependency)
    ));
    let mut snapshot = OrganizationDeliverySnapshot::planned(command.clone()).unwrap();
    snapshot.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
    snapshot.create.revision = 2;
    snapshot.revision = 2;
    for (id, team) in &mut snapshot.teams {
        team.revision = 2;
        team.create.revision = 2;
        team.create.outcome = match id {
            5 => TeamCreateOutcome::OutcomeUnknown,
            6 => TeamCreateOutcome::Created {
                via: TeamCreation::PendingMembership,
            },
            _ => TeamCreateOutcome::Blocked {
                reason: OrganizationBlockedReason::IdpSyncedTeam,
            },
        };
        if *id == 6 {
            team.create.confirmed_at = Some(now - Duration::hours(2));
        }
    }
    s.project_organization_delivery(&snapshot).await.unwrap();
    let missing = s.delivery_signals(now).await.unwrap();
    assert_eq!(missing.counts.organization_deliveries, 1);
    assert_eq!(missing.counts.team_steps, 3);
    assert_eq!(missing.counts.unknown.missing_timestamp, 2);
    assert!(missing.incomplete());
    assert_eq!(
        missing
            .steps
            .iter()
            .map(|r| r.step.as_str())
            .collect::<Vec<_>>(),
        vec!["org", "team:5", "team:6", "team:7"]
    );
    s.project_organization_telemetry(&telemetry).await.unwrap();
    s.project_organization_telemetry(&telemetry).await.unwrap();
    let team = OrganizationStepTelemetry {
        step: Step::Team { team_id: 5 },
        unknown_since: Some(now - Duration::hours(1) - Duration::nanoseconds(1)),
        ..telemetry.clone()
    };
    s.project_organization_telemetry(&team).await.unwrap();
    let pending = OrganizationStepTelemetry {
        step: Step::Team { team_id: 6 },
        unknown_since: None,
        last_observed_at: Some(now - Duration::hours(2)),
        ..telemetry.clone()
    };
    s.project_organization_telemetry(&pending).await.unwrap();
    // An organization-only read must not make a separate pending team fresh.
    s.project_organization_telemetry(&OrganizationStepTelemetry {
        unknown_since: None,
        last_observed_at: Some(now),
        ..telemetry.clone()
    })
    .await
    .unwrap();
    let report = s.delivery_signals(now).await.unwrap();
    assert_eq!(report.counts.unknown.over_threshold, 1);
    assert_eq!(report.counts.pending.oldest_age_seconds, Some(7200));
    assert_eq!(report.counts.pending.over_threshold, 0);
    assert!(!report.incomplete());
    let later = s
        .delivery_signals(now + Duration::nanoseconds(1))
        .await
        .unwrap();
    assert_eq!(later.counts.unknown.over_threshold, 2);
    assert_eq!(later.counts.pending.over_threshold, 1);
    for bad in [
        OrganizationStepTelemetry {
            step: Step::Team { team_id: 99 },
            ..telemetry.clone()
        },
        OrganizationStepTelemetry {
            account_id: 200,
            ..telemetry.clone()
        },
        OrganizationStepTelemetry {
            org_id: 0,
            ..telemetry.clone()
        },
        OrganizationStepTelemetry {
            delivery_generation: super::super::integer::MAX_EXACT + 1,
            ..telemetry.clone()
        },
        OrganizationStepTelemetry {
            unknown_since: Some(now),
            ..telemetry.clone()
        },
        OrganizationStepTelemetry {
            last_observed_at: Some(command.approved_at - Duration::nanoseconds(1)),
            ..telemetry.clone()
        },
    ] {
        assert!(matches!(
            s.project_organization_telemetry(&bad).await,
            Err(Error::ProjectionInvariant(_))
        ));
    }
    // Per-step replay is monotonic and never rewrites immutable outcome JSON.
    s.project_organization_telemetry(&OrganizationStepTelemetry {
        last_observed_at: Some(now - Duration::minutes(1)),
        ..pending.clone()
    })
    .await
    .unwrap();
    s.project_organization_telemetry(&pending).await.unwrap();
    assert_eq!(
        s.delivery_signals(now)
            .await
            .unwrap()
            .counts
            .pending
            .oldest_age_seconds,
        Some(60)
    );
    s.project_organization_telemetry(&OrganizationStepTelemetry {
        last_observed_at: Some(now + Duration::seconds(1)),
        ..pending.clone()
    })
    .await
    .unwrap();
    assert_eq!(
        s.delivery_signals(now)
            .await
            .unwrap()
            .counts
            .pending
            .future_timestamp,
        1
    );
    assert_eq!(
        s.get_organization_delivery(&command.storage_id())
            .await
            .unwrap(),
        Some(snapshot.clone())
    );
    // Typed blocked counts include step kind; a delegated repository reader sees none of the mixed link.
    s.record_blocked_digest(now).await.unwrap();
    let full = s
        .scoped_blocked_digest(&ConsoleScope::account(100).unwrap(), now.date_naive())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(full.counts.len(), 1);
    assert_eq!(full.counts[0].kind, SignalKind::Team);
    assert_eq!(
        full.counts[0].reason,
        OperationalBlockedReason::IdpSyncedTeam
    );
    let delegated = s
        .scoped_blocked_digest(
            &ConsoleScope::repo_admin(100, [10]).unwrap(),
            now.date_naive(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(delegated.counts.is_empty());
    let other = s
        .scoped_blocked_digest(&ConsoleScope::account(200).unwrap(), now.date_naive())
        .await
        .unwrap()
        .unwrap();
    assert!(other.counts.is_empty());
    // Later changes cannot alter the first completed daily snapshot.
    snapshot.revision = 3;
    snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::Blocked {
        reason: OrganizationBlockedReason::ManagedEnterprise,
    };
    snapshot.teams.get_mut(&7).unwrap().create.revision = 3;
    snapshot.teams.get_mut(&7).unwrap().revision = 3;
    s.project_organization_delivery(&snapshot).await.unwrap();
    s.record_blocked_digest(now + Duration::minutes(1))
        .await
        .unwrap();
    assert_eq!(
        s.scoped_blocked_digest(&ConsoleScope::account(100).unwrap(), now.date_naive())
            .await
            .unwrap(),
        Some(full)
    );
}
