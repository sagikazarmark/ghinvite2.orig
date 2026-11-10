use crate::{
    audit::{ActorKind, EventType, TargetKind},
    email_delivery::{EmailCreateOutcome, EmailDeliverySnapshot, EmailSettlement},
};
use sha2::{Digest, Sha256};
/// Derive only retained facts with trustworthy clocks. No mailbox reference or
/// address digest is part of event identity or metadata.
pub(super) fn events(snapshot: &EmailDeliverySnapshot) -> Vec<serde_json::Value> {
    let command = &snapshot.command;
    let mut events = Vec::new();
    let mut push = |kind: EventType,
                    at: chrono::DateTime<chrono::Utc>,
                    suffix: &str,
                    detail: serde_json::Value| {
        let target = command.storage_id();
        let digest = Sha256::digest(format!(
            "ghinvite/email-delivery/audit/v1/{target}/{suffix}"
        ));
        let id = ulid::Ulid::from_bytes(digest[..16].try_into().expect("digest length"));
        let metadata = serde_json::json!({"source":{"kind":"batch","batch_id":command.batch.batch_id,"row":command.batch.row},"org_id":command.organization.org_id,"team_ids":command.teams.iter().map(|t|t.team_id).collect::<Vec<_>>(),"detail":detail});
        events.push(serde_json::json!({"id":id.to_string(),"account_id":command.account_id,"occurred_at":at,"event_type":kind,"actor_kind":ActorKind::System,"actor_id":null,"target_kind":TargetKind::OrganizationDelivery,"target_id":target,"metadata":metadata,"request_id":null}));
    };
    match (
        &snapshot.outcome,
        snapshot.confirmed_at,
        snapshot.completed_at,
    ) {
        (EmailCreateOutcome::Created { upstream_id, .. }, Some(at), _) => push(
            EventType::OrgInvitationSent,
            at,
            "create",
            serde_json::json!({"upstream_id":upstream_id}),
        ),
        (EmailCreateOutcome::Failed { reason }, _, Some(at)) => push(
            EventType::InvitationSendFailed,
            at,
            "create",
            serde_json::json!({"reason":reason}),
        ),
        (EmailCreateOutcome::Aborted { reason }, _, Some(at)) => push(
            EventType::DeliveryAborted,
            at,
            "create",
            serde_json::json!({"reason":reason}),
        ),
        _ => {}
    }
    if let (EmailCreateOutcome::Created { upstream_id, .. }, Some(settlement), Some(at)) =
        (&snapshot.outcome, &snapshot.settlement, snapshot.settled_at)
    {
        let kind = match settlement {
            EmailSettlement::NoLongerPending => EventType::EmailInvitationNoLongerPending,
            EmailSettlement::FailedInvitation { .. } => EventType::EmailInvitationFailed,
            EmailSettlement::Cancelled => EventType::InvitationCancelled,
        };
        push(
            kind,
            at,
            "settlement",
            serde_json::json!({"upstream_id":upstream_id,"settlement":settlement}),
        );
    }
    events
}

/// Separate transition namespace preserves every existing terminal event byte.
pub(super) fn transitions(
    snapshot: &EmailDeliverySnapshot,
    facts: &[super::EmailAuditFact],
) -> Vec<serde_json::Value> {
    facts.iter().map(|fact| {
        let (event_type, phase, detail) = match &fact.kind {
            super::EmailAuditKind::Blocked { reason } => (EventType::DeliveryBlocked, "blocked", serde_json::json!({"reason":reason})),
            super::EmailAuditKind::OutcomeUnknown => (EventType::DeliveryOutcomeUnknown, "unknown", serde_json::json!({})),
        };
        let command = &snapshot.command;
        let target = command.storage_id();
        let digest = Sha256::digest(format!("ghinvite/email-delivery/transition/v1/{target}/{}/{phase}", fact.transition_revision));
        let id = ulid::Ulid::from_bytes(digest[..16].try_into().expect("digest length"));
        serde_json::json!({"id":id.to_string(),"account_id":command.account_id,"occurred_at":fact.at,
            "event_type":event_type,"actor_kind":ActorKind::System,"actor_id":null,
            "target_kind":TargetKind::OrganizationDelivery,"target_id":target,"request_id":null,
            "metadata":{"source":{"kind":"batch","batch_id":command.batch.batch_id,"row":command.batch.row},
                "org_id":command.organization.org_id,"team_ids":command.teams.iter().map(|t|t.team_id).collect::<Vec<_>>(),
                "transition_revision":fact.transition_revision,"detail":detail}})
    }).collect()
}
