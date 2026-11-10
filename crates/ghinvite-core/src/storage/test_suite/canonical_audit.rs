use super::*;
use crate::{
    delivery::*,
    delivery_audit::*,
    org_delivery::*,
    storage::{AuditPosition, Error},
};
use serde_json::json;

pub async fn scenario_canonical_delivery_audit<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&sample_org_installation(1, 9001, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    let mut link = sample_link(9001, 1, 701);
    link.organization_installation_id = Some(1);
    link.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 9001,
        org_login: "acme".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    link.targets.teams = vec![crate::targets::TeamTarget {
        team_id: 5,
        team_slug: "api".into(),
        grant: crate::targets::MemberGrant::Member,
    }];
    let at = dt("2026-05-04T13:00:00Z");
    for version in [
        DeliveryAuditVersion::Legacy,
        DeliveryAuditVersion::CanonicalV1,
        DeliveryAuditVersion::CanonicalV2,
    ] {
        let request = sample_request(link.id, 701);
        seed(&s, &link, std::slice::from_ref(&request), 1).await;
        let repo = CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            invitation_id: (version != DeliveryAuditVersion::CanonicalV2)
                .then(GithubInvitationId::new),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 9001,
            installation_id: 1,
            requester_id: 701,
            repo_id: link.targets.repos[0].repo_id,
            repo_full_name: link.targets.repos[0].repo_full_name.clone(),
            permission: crate::Permission::Pull,
            delivery_generation: 1,
            approved_at: at,
            budget_share_pct: 50,
        };
        let initial: DeliverySnapshot = CreateReceipt {
            audit_version: version,
            command: repo.clone(),
            outcome: CreateOutcome::OutcomeUnknown,
            revision: 1,
            confirmed_at: None,
        }
        .into();
        let wire = serde_json::to_value(&initial).unwrap();
        assert_eq!(
            wire["create"]["command"].get("invitation_id").is_none(),
            version == DeliveryAuditVersion::CanonicalV2
        );
        assert_eq!(
            serde_json::from_value::<DeliverySnapshot>(wire).unwrap(),
            initial
        );
        let mut bad_pair = initial.clone();
        bad_pair.create.command.invitation_id = if version == DeliveryAuditVersion::CanonicalV2 {
            Some(GithubInvitationId::new())
        } else {
            None
        };
        assert!(matches!(
            s.project_delivery(&bad_pair).await,
            Err(Error::ProjectionInvariant(_))
        ));
        s.project_delivery(&initial).await.unwrap();
        let opposite = if version.is_legacy() {
            DeliveryAuditVersion::CanonicalV1
        } else {
            DeliveryAuditVersion::Legacy
        };
        let mut conflict = initial.clone();
        conflict.create.audit_version = opposite;
        conflict.revision = 2;
        conflict.create.revision = 2;
        assert!(matches!(
            s.project_delivery(&conflict).await,
            Err(Error::ProjectionInvariant(_))
        ));
        let mut confirmed = initial.clone();
        confirmed.revision = 2;
        confirmed.create.revision = 2;
        confirmed.create.outcome = CreateOutcome::Aborted {
            reason: AbortReason::LinkRevoked,
        };
        confirmed.create.confirmed_at = Some(at);
        s.project_delivery(&confirmed).await.unwrap();
        s.project_delivery(&initial).await.unwrap();
        conflict.revision = 1;
        conflict.create.revision = 1;
        assert!(
            matches!(
                s.project_delivery(&conflict).await,
                Err(Error::ProjectionInvariant(_))
            ),
            "stale version rewrite must not invent legacy events"
        );
        assert_eq!(
            s.list_delivery_for_request(request.id).await.unwrap(),
            vec![confirmed]
        );
        let rows = s
            .list_audit_events(
                9001,
                Some(EventType::DeliveryAborted),
                AuditPosition::Latest,
            )
            .await
            .unwrap()
            .events;
        let row = rows
            .iter()
            .find(|event| {
                event.target_id
                    == repo
                        .invitation_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| repo.delivery_key())
            })
            .unwrap();
        assert_eq!(
            row.metadata.get("reason").cloned(),
            if version.is_legacy() {
                None
            } else {
                Some(json!("link_revoked"))
            }
        );

        let command = OrganizationCreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            invitation_id: (version != DeliveryAuditVersion::CanonicalV2)
                .then(GithubInvitationId::new),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 9001,
            installation_id: 1,
            requester_id: 701,
            organization: link.targets.organization.clone().unwrap(),
            teams: link.targets.teams.clone(),
            delivery_generation: 1,
            approved_at: at,
            budget_share_pct: 50,
        };
        let mut planned = OrganizationDeliverySnapshot::planned(command.clone()).unwrap();
        planned.create.audit_version = version;
        s.project_organization_delivery(&planned).await.unwrap();
        let mut conflict = planned.clone();
        conflict.create.audit_version = opposite;
        conflict.revision = 2;
        conflict.create.revision = 2;
        assert!(matches!(
            s.project_organization_delivery(&conflict).await,
            Err(Error::ProjectionInvariant(_))
        ));
        let mut created = planned.clone();
        created.revision = 2;
        created.create.revision = 2;
        created.create.outcome = OrganizationCreateOutcome::Created {
            upstream_id: 99,
            created_at: at,
        };
        created.create.confirmed_at = Some(at);
        let team = created.teams.get_mut(&5).unwrap();
        team.revision = 2;
        team.create.revision = 2;
        team.create.outcome = TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: 99 },
        };
        team.create.confirmed_at = Some(at);
        s.project_organization_delivery(&created).await.unwrap();
        let mut settled = created.clone();
        settled.revision = 3;
        settle(
            &mut settled,
            InvitationState::Cancelled,
            Some(SettlementReason::OwnExpiry),
        );
        s.project_organization_delivery(&settled).await.unwrap();
        let original = s
            .list_audit_events(
                9001,
                Some(EventType::InvitationCancelled),
                AuditPosition::Latest,
            )
            .await
            .unwrap()
            .events;
        for snapshot in [&settled, &created, &planned] {
            s.project_organization_delivery(snapshot).await.unwrap();
        }
        assert_eq!(
            s.get_organization_delivery(&command.storage_id())
                .await
                .unwrap(),
            Some(settled.clone())
        );
        assert_eq!(
            s.list_audit_events(
                9001,
                Some(EventType::InvitationCancelled),
                AuditPosition::Latest
            )
            .await
            .unwrap()
            .events,
            original
        );
        let team_event = original
            .iter()
            .find(|event| event.target_id == format!("{}/team:5", command.storage_id()))
            .unwrap();
        assert_eq!(
            team_event.metadata.get("reason").cloned(),
            if version.is_legacy() {
                None
            } else {
                Some(json!("own_expiry"))
            }
        );
        conflict.revision = 1;
        conflict.create.revision = 1;
        assert!(matches!(
            s.project_organization_delivery(&conflict).await,
            Err(Error::ProjectionInvariant(_))
        ));
        let mut reason_rewrite = settled.clone();
        reason_rewrite.revision = 4;
        reason_rewrite.teams.get_mut(&5).unwrap().revision = 4;
        reason_rewrite
            .teams
            .get_mut(&5)
            .unwrap()
            .settlement
            .as_mut()
            .unwrap()
            .reason = Some(SettlementReason::LinkRevoked);
        assert!(matches!(
            s.project_organization_delivery(&reason_rewrite).await,
            Err(Error::ProjectionInvariant(_))
        ));
        assert_eq!(
            s.get_organization_delivery(&command.storage_id())
                .await
                .unwrap(),
            Some(settled)
        );

        // A final snapshot can arrive before its create projection. It inserts
        // both retained facts; later old envelopes cannot reopen its state.
        let mut accepted = created.clone();
        accepted.create.command.delivery_generation = 2;
        accepted.create.command.invitation_id =
            (version != DeliveryAuditVersion::CanonicalV2).then(GithubInvitationId::new);
        accepted.revision = 3;
        settle(&mut accepted, InvitationState::Accepted, None);
        s.project_organization_delivery(&accepted).await.unwrap();
        let mut before = accepted.clone();
        before.revision = 2;
        before.settlement = None;
        before.teams.get_mut(&5).unwrap().settlement = None;
        before.teams.get_mut(&5).unwrap().revision = 2;
        s.project_organization_delivery(&before).await.unwrap();
        s.project_organization_delivery(&accepted).await.unwrap();
        let expected = if version.is_legacy() {
            EventType::InvitationAccepted
        } else {
            EventType::OrgInvitationAccepted
        };
        let rows = s
            .list_audit_events(9001, Some(expected), AuditPosition::Latest)
            .await
            .unwrap()
            .events;
        assert_eq!(
            rows.iter()
                .filter(|event| event.target_id == accepted.create.command.storage_id())
                .count(),
            1
        );
        if !version.is_legacy() {
            let rows = s
                .list_audit_events(
                    9001,
                    Some(EventType::TeamMembershipAdded),
                    AuditPosition::Latest,
                )
                .await
                .unwrap()
                .events;
            assert_eq!(
                rows.iter()
                    .filter(|event| event.target_id
                        == format!("{}/team:5", accepted.create.command.storage_id())
                        && event.metadata["membership_state"] == "active")
                    .count(),
                1
            );
        }
    }
    assert!(
        s.list_audit_events(9002, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
}
fn settle(
    snapshot: &mut OrganizationDeliverySnapshot,
    state: InvitationState,
    reason: Option<SettlementReason>,
) {
    let command = &snapshot.create.command;
    let at = command.approved_at + chrono::Duration::seconds(1);
    snapshot.settlement = Some(OrganizationSettlement {
        state,
        event: AuditEvent {
            id: organization_settlement_id(snapshot.create.audit_version, command),
            account_id: command.account_id,
            occurred_at: at,
            event_type: organization_settlement_kind(snapshot.create.audit_version, state).unwrap(),
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::OrganizationDelivery,
            target_id: command.storage_id(),
            metadata: json!({"reason":reason}),
            request_id: Some(command.request_id.to_string()),
        },
    });
    for team in snapshot.teams.values_mut() {
        team.revision = snapshot.revision;
        team.settlement = Some(TeamSettlement {
            state,
            observed_at: at,
            reason: if snapshot.create.audit_version.is_legacy() {
                None
            } else {
                reason
            },
        });
    }
}
