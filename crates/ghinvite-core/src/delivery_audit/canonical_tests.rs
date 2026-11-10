use super::*;
use crate::{InvitationState, delivery::*, org_delivery::*};
use serde_json::{Value, json};

fn org() -> OrganizationDeliverySnapshot {
    OrganizationDeliverySnapshot::planned(serde_json::from_value(json!({
        "invitation_id":"00000000000000000000000001","link_id":"00000000000000000000000002","request_id":"00000000000000000000000003",
        "approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,
        "organization":{"org_id":9001,"org_login":"acme","grant":"member"},
        "teams":[{"team_id":5,"team_slug":"api","grant":"member"}],
        "approved_at":"2026-05-04T13:00:00Z"
    })).unwrap()).unwrap()
}
fn encoded(snapshot: &OrganizationDeliverySnapshot) -> Value {
    serde_json::from_str(
        &crate::storage::org_delivery::encode_projection(snapshot, None)
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
fn created(version: DeliveryAuditVersion) -> OrganizationDeliverySnapshot {
    let mut snapshot = org();
    snapshot.create.audit_version = version;
    snapshot.create.outcome = OrganizationCreateOutcome::Created {
        upstream_id: 99,
        created_at: snapshot.create.command.approved_at,
    };
    snapshot.create.confirmed_at = Some(snapshot.create.command.approved_at);
    let team = snapshot.teams.get_mut(&5).unwrap();
    team.create.outcome = TeamCreateOutcome::Created {
        via: TeamCreation::OrganizationInvitation { upstream_id: 99 },
    };
    team.create.confirmed_at = snapshot.create.confirmed_at;
    snapshot
}
fn settle(
    snapshot: &mut OrganizationDeliverySnapshot,
    state: InvitationState,
    reason: Option<SettlementReason>,
) {
    let at = snapshot.create.command.approved_at + chrono::Duration::seconds(1);
    snapshot.revision = 2;
    snapshot.settlement = Some(OrganizationSettlement {
        state,
        event: AuditEvent {
            id: organization_settlement_id(snapshot.create.audit_version, &snapshot.create.command),
            account_id: 9001,
            occurred_at: at,
            event_type: organization_settlement_kind(snapshot.create.audit_version, state).unwrap(),
            actor_kind: ActorKind::Github,
            actor_id: None,
            target_kind: TargetKind::OrganizationDelivery,
            target_id: snapshot.create.command.storage_id(),
            metadata: json!({"reason":reason}),
            request_id: Some(snapshot.create.command.request_id.to_string()),
        },
    });
    let team = snapshot.teams.get_mut(&5).unwrap();
    team.revision = 2;
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
#[test]
fn omitted_version_preserves_legacy_wire_ids_and_payloads() {
    let mut legacy = created(DeliveryAuditVersion::Legacy);
    let wire = serde_json::to_string(&legacy).unwrap();
    assert!(!wire.contains("audit_version"));
    assert_eq!(
        serde_json::to_string(
            &serde_json::from_str::<OrganizationDeliverySnapshot>(&wire).unwrap()
        )
        .unwrap(),
        wire
    );
    let original = encoded(&legacy);
    assert_eq!(original["events"][0]["event_type"], "invitation.sent");
    assert_eq!(
        original["events"][0]["metadata"],
        json!({"outcome":{"kind":"created","upstream_id":99,"created_at":"2026-05-04T13:00:00Z"}})
    );
    assert_eq!(original["events"][1]["event_type"], "invitation.sent");
    assert_eq!(
        original["events"][1]["metadata"],
        json!({"team_id":5,"outcome":{"kind":"created","via":{"kind":"organization_invitation","upstream_id":99}}})
    );
    for (index, target) in [
        (0, legacy.create.command.storage_id()),
        (1, format!("{}/team:5", legacy.create.command.storage_id())),
    ] {
        let digest = Sha256::digest(format!("ghinvite/org-delivery/audit/{target}/create"));
        assert_eq!(
            original["events"][index]["id"],
            ulid::Ulid::from_bytes(digest[..16].try_into().unwrap()).to_string()
        );
    }
    settle(
        &mut legacy,
        InvitationState::Cancelled,
        Some(SettlementReason::OwnExpiry),
    );
    assert_eq!(
        legacy.settlement.as_ref().unwrap().event.id.to_string(),
        "00000000000000000000000001"
    );
    assert_eq!(
        encoded(&legacy)["events"][2]["metadata"],
        json!({"team_id":5})
    );
    assert!(
        !serde_json::to_string(&legacy.teams[&5].settlement)
            .unwrap()
            .contains("reason")
    );
    let mut wire = serde_json::to_value(&legacy).unwrap();
    for version in [json!("future"), Value::Null, json!(3)] {
        wire["create"]["audit_version"] = version;
        assert!(serde_json::from_value::<OrganizationDeliverySnapshot>(wire.clone()).is_err());
    }
}
#[test]
fn canonical_attached_membership_waits_for_positive_org_acceptance() {
    let mut snapshot = created(DeliveryAuditVersion::CanonicalV1);
    let before = encoded(&snapshot);
    assert_eq!(before["events"][0]["event_type"], "org_invitation.sent");
    assert_eq!(before["events"][1]["event_type"], "invitation.sent");
    assert_ne!(
        before["events"][0]["id"],
        encoded(&created(DeliveryAuditVersion::Legacy))["events"][0]["id"]
    );
    settle(&mut snapshot, InvitationState::Accepted, None);
    let after = encoded(&snapshot);
    assert_eq!(after["events"][2]["event_type"], "team_membership.added");
    assert_eq!(
        after["events"][2]["metadata"],
        json!({"team_id":5,"membership_state":"active"})
    );
    assert_eq!(after["events"][3]["event_type"], "org_invitation.accepted");
    assert_ne!(after["events"][3]["id"], "00000000000000000000000001");
    assert_eq!(after, encoded(&snapshot));
    snapshot.settlement.as_mut().unwrap().event.event_type = EventType::InvitationAccepted;
    assert!(snapshot.validate().is_err());
}
#[test]
fn direct_team_membership_facts_distinguish_active_and_pending() {
    for (outcome, state) in [
        (TeamCreateOutcome::Upgraded, "active"),
        (
            TeamCreateOutcome::Created {
                via: TeamCreation::PendingMembership,
            },
            "pending",
        ),
    ] {
        let mut snapshot = org();
        snapshot.create.audit_version = DeliveryAuditVersion::CanonicalV1;
        let team = snapshot.teams.get_mut(&5).unwrap();
        team.create.outcome = outcome;
        team.create.confirmed_at = Some(snapshot.create.command.approved_at);
        let encoded = encoded(&snapshot);
        assert_eq!(encoded["events"][0]["event_type"], "team_membership.added");
        assert_eq!(encoded["events"][0]["metadata"]["membership_state"], state);
    }
}
#[test]
fn canonical_cancelled_team_requires_exact_owned_cancellation_reason() {
    let mut snapshot = created(DeliveryAuditVersion::CanonicalV1);
    settle(
        &mut snapshot,
        InvitationState::Cancelled,
        Some(SettlementReason::OwnExpiry),
    );
    assert_eq!(
        encoded(&snapshot)["events"][2]["metadata"]["reason"],
        "own_expiry"
    );
    for reason in [None, Some(SettlementReason::LinkRevoked)] {
        snapshot
            .teams
            .get_mut(&5)
            .unwrap()
            .settlement
            .as_mut()
            .unwrap()
            .reason = reason;
        assert!(snapshot.validate().is_err());
    }
    snapshot
        .teams
        .get_mut(&5)
        .unwrap()
        .settlement
        .as_mut()
        .unwrap()
        .reason = Some(SettlementReason::OwnExpiry);
    snapshot.settlement.as_mut().unwrap().event.metadata = json!({"reason":"upstream prose"});
    assert!(snapshot.validate().is_err());
}
#[test]
fn version_is_immutable_before_confirmation_and_on_stale_snapshots() {
    let old = org();
    let mut next = old.clone();
    next.create.audit_version = DeliveryAuditVersion::CanonicalV1;
    for revision in [1, 2] {
        next.revision = revision;
        next.create.revision = revision;
        assert!(next.validate_successor(&old).is_err());
        assert!(old.validate_successor(&next).is_err());
    }
}
#[test]
fn repository_legacy_abort_payload_stays_exact_and_canonical_has_reason() {
    let mut projection = super::tests::repository();
    projection.snapshot.create.command.invitation_id =
        Some("00000000000000000000000001".parse().unwrap());
    projection.snapshot.create.outcome = CreateOutcome::Aborted {
        reason: AbortReason::LinkRevoked,
    };
    projection.snapshot.create.confirmed_at = Some(projection.snapshot.create.command.approved_at);
    let legacy: Value = serde_json::from_str(
        &crate::storage::delivery_projection::encode(&projection.snapshot).unwrap(),
    )
    .unwrap();
    assert_eq!(
        legacy["event"]["metadata"],
        json!({"repo_full_name":"acme/api","requester_id":701})
    );
    let digest = Sha256::digest("ghinvite/delivery/audit/00000000000000000000000001");
    assert_eq!(
        legacy["event"]["id"],
        ulid::Ulid::from_bytes(digest[..16].try_into().unwrap()).to_string()
    );
    projection.audit_events.push(super::tests::fact());
    let facts = projection.events().unwrap();
    projection.snapshot.create.audit_version = DeliveryAuditVersion::CanonicalV1;
    let canonical: Value = serde_json::from_str(
        &crate::storage::delivery_projection::encode(&projection.snapshot).unwrap(),
    )
    .unwrap();
    assert_eq!(canonical["event"]["metadata"]["reason"], "link_revoked");
    assert_ne!(legacy["event"]["id"], canonical["event"]["id"]);
    assert_eq!(
        facts,
        projection.events().unwrap(),
        "transient fact IDs and payloads never change with confirmed-event version"
    );
}
