use super::*;
use ghinvite_core::{
    audit::TargetKind,
    bulk::BatchRef,
    delivery_audit::{DeliveryAuditVersion, repository_target},
};

fn command(legacy_id: bool) -> CreateCommand {
    let request_id = ghinvite_core::RequestId::new();
    CreateCommand {
        invitation_id: legacy_id.then(ghinvite_core::GithubInvitationId::new),
        delivery_generation: 1,
        link_id: None,
        batch: Some(BatchRef {
            batch_id: ghinvite_core::bulk::BulkBatchId::new(),
            row: 1,
        }),
        budget_attribution: Default::default(),
        access_lifetime_seconds: None,
        budget_share_pct: 50,
        request_id,
        approval_id: format!("request.approved/{request_id}"),
        account_id: 100,
        installation_id: 1,
        requester_id: 8,
        repo_id: 10,
        repo_full_name: "acme/api".into(),
        permission: ghinvite_core::Permission::Push,
        approved_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
    }
}
fn aborted() -> CreateOutcome {
    CreateOutcome::Aborted {
        reason: AbortReason::BatchCancelled,
    }
}

#[test]
fn new_cancellation_receipts_bind_the_supplied_identity_without_allocating_a_bridge() {
    for (legacy_id, expected) in [
        (false, DeliveryAuditVersion::CanonicalV2),
        (true, DeliveryAuditVersion::CanonicalV1),
    ] {
        let command = command(legacy_id);
        let now = command.approved_at + chrono::Duration::seconds(1);
        let snapshot = cancellation_snapshot(&command, None, aborted(), now).unwrap();
        crate::delivery::validate_notice_snapshot(&command.delivery_key(), &snapshot).unwrap();
        assert_eq!(snapshot.create.command, command);
        assert_eq!(snapshot.create.audit_version, expected);
        assert_eq!(snapshot.create.revision, 1);
        let expected_target = match command.invitation_id {
            Some(id) => (TargetKind::GithubInvitation, id.to_string()),
            None => (TargetKind::RepositoryDelivery, command.delivery_key()),
        };
        assert_eq!(
            repository_target(&snapshot.create).unwrap(),
            expected_target
        );
    }
}

#[test]
fn retained_legacy_v1_and_v2_cancellation_preserve_identity_and_audit_contract() {
    for version in [
        DeliveryAuditVersion::Legacy,
        DeliveryAuditVersion::CanonicalV1,
        DeliveryAuditVersion::CanonicalV2,
    ] {
        let command = command(version != DeliveryAuditVersion::CanonicalV2);
        let previous = DeliverySnapshot::from(CreateReceipt {
            audit_version: version,
            command: command.clone(),
            outcome: CreateOutcome::OutcomeUnknown,
            confirmed_at: None,
            revision: 5,
        });
        let original_bytes = serde_json::to_vec(&previous).unwrap();
        let now = command.approved_at + chrono::Duration::seconds(1);
        let next = cancellation_snapshot(&command, Some(&previous), aborted(), now).unwrap();
        crate::delivery::validate_notice_snapshot(&command.delivery_key(), &next).unwrap();
        assert_eq!(next.create.audit_version, version);
        assert_eq!(next.create.command, previous.create.command);
        assert_eq!(next.revision, 6);
        assert_eq!(next.create.confirmed_at, Some(now));
        assert_eq!(
            repository_target(&next.create).unwrap(),
            repository_target(&previous.create).unwrap()
        );
        assert_eq!(serde_json::to_vec(&previous).unwrap(), original_bytes);
    }
}
