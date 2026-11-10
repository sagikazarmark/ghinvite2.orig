//! One atomic receipt/history projection for SQLx and D1. Audit insertion is
//! independent of receipt freshness and of the current invitation lifecycle.
use crate::{
    audit::{ActorKind, EventType},
    delivery::{CreateOutcome, DeliverySnapshot},
};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn encode(snapshot: &DeliverySnapshot) -> super::Result<String> {
    encode_with_events(&snapshot.clone().into())
}

pub fn encode_with_events(
    projection: &crate::delivery_audit::RepositoryDeliveryProjection,
) -> super::Result<String> {
    let snapshot = &projection.snapshot;
    let events = projection
        .events()
        .map_err(|message| super::Error::ProjectionInvariant(message.into()))?;
    let events =
        serde_json::to_string(&events).map_err(|error| super::Error::Corrupt(error.to_string()))?;
    super::integer::validate(snapshot)?;
    if let Some(timing) = &snapshot.invitation_timing {
        let valid = snapshot.timing_binding().is_some_and(|(id, owned)| {
            id == timing.upstream_id
                && crate::delivery::InvitationTiming::observed(
                    id,
                    timing.created_at,
                    timing.observed_at,
                    snapshot.create.command.approved_at,
                    owned,
                )
                .is_some()
        });
        if !valid
            || timing
                .owned_lifetime_seconds
                .is_some_and(|s| !(3600..=604800).contains(&s))
        {
            return Err(super::Error::ProjectionInvariant(
                "invalid invitation timing".into(),
            ));
        }
    }
    let receipt = &snapshot.create;
    let (target_kind, target_id) = crate::delivery_audit::repository_target(receipt)
        .map_err(|e| super::Error::ProjectionInvariant(e.into()))?;
    if snapshot.tracking_stopped.is_some()
        && (!receipt.outcome.tracked_foreign() || snapshot.settlement.is_some())
    {
        return Err(super::Error::ProjectionInvariant(
            "invalid foreign tracking stop".into(),
        ));
    }
    if snapshot.revision < receipt.revision {
        return Err(super::Error::ProjectionInvariant(
            "invalid delivery revision".into(),
        ));
    }
    if let Some(settlement) = &snapshot.settlement
        && (!receipt.outcome.needs_settlement()
            || snapshot.revision <= receipt.revision
            || settlement.event.event_type != super::settlement::event_type(settlement.state)?
            || settlement.event.account_id != receipt.command.account_id
            || settlement.event.target_id != target_id
            || settlement.event.target_kind != target_kind
            || (receipt.audit_version == crate::delivery_audit::DeliveryAuditVersion::CanonicalV2
                && settlement.event.id
                    != crate::delivery_audit::repository_settlement_id(receipt)
                        .map_err(|e| super::Error::ProjectionInvariant(e.into()))?))
    {
        return Err(super::Error::ProjectionInvariant(
            "invalid retained settlement".into(),
        ));
    }
    if receipt.command.delivery_generation == 0
        || receipt.revision == 0
        || receipt.outcome.confirmed() != receipt.confirmed_at.is_some()
    {
        return Err(super::Error::ProjectionInvariant(
            "invalid delivery receipt".into(),
        ));
    }
    let command = &receipt.command;
    crate::bulk::validate_origin(command.link_id, command.batch)
        .map_err(|message| super::Error::ProjectionInvariant(message.into()))?;
    let event = if let Some(at) = receipt.confirmed_at {
        let mut metadata =
            json!({"repo_full_name":command.repo_full_name,"requester_id":command.requester_id});
        let (kind, actor) = match &receipt.outcome {
            CreateOutcome::Created { upstream_id } => {
                metadata["github_invitation_id"] = json!(upstream_id);
                (EventType::InvitationSent, ActorKind::System)
            }
            CreateOutcome::AlreadySatisfied { .. } => {
                metadata["reason"] = json!("already_collaborator");
                (EventType::DeliveryAlreadySatisfied, ActorKind::Github)
            }
            CreateOutcome::AlreadyPending { upstream_id, .. } => {
                metadata["github_invitation_id"] = json!(upstream_id);
                (EventType::DeliveryAlreadyPending, ActorKind::System)
            }
            CreateOutcome::Upgraded { .. } => (EventType::DeliveryUpgraded, ActorKind::System),
            CreateOutcome::SkippedCustomRole { .. } => {
                (EventType::DeliverySkippedCustomRole, ActorKind::System)
            }
            CreateOutcome::Aborted { reason } => {
                if !receipt.audit_version.is_legacy() {
                    metadata["reason"] = json!(reason);
                }
                (EventType::DeliveryAborted, ActorKind::System)
            }
            CreateOutcome::Failed { .. } => {
                metadata["reason"] = json!("github_rejected");
                (EventType::InvitationSendFailed, ActorKind::System)
            }
            _ => {
                return Err(super::Error::ProjectionInvariant(
                    "unconfirmed delivery has confirmation time".into(),
                ));
            }
        };
        let identity = command
            .invitation_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| command.delivery_key());
        let phase =
            if receipt.audit_version == crate::delivery_audit::DeliveryAuditVersion::CanonicalV2 {
                "/confirmation"
            } else {
                ""
            };
        let namespace = match receipt.audit_version {
            crate::delivery_audit::DeliveryAuditVersion::Legacy => "ghinvite/delivery/audit",
            crate::delivery_audit::DeliveryAuditVersion::CanonicalV1 => {
                "ghinvite/delivery/audit/v1"
            }
            crate::delivery_audit::DeliveryAuditVersion::CanonicalV2 => {
                "ghinvite/delivery/audit/v2"
            }
        };
        let digest = Sha256::digest(format!("{namespace}/{identity}{phase}"));
        let bytes: [u8; 16] = digest[..16].try_into().unwrap();
        json!({"id":ulid::Ulid::from_bytes(bytes).to_string(),"account_id":command.account_id,
            "occurred_at":at,"event_type":kind,"actor_kind":actor,"target_kind":target_kind,"target_id":target_id,
            "metadata":metadata})
    } else {
        serde_json::Value::Null
    };
    // Always use the typed receipt encoding (struct field order): a replayed
    // receipt must match the stored text, because SQLite's immutable JSON
    // comparison is textual.
    let key = serde_json::to_string(&command.delivery_key()).expect("string serializes");
    let final_result = serde_json::to_string(
        &crate::request_completion::repository(snapshot).map(|result| result.result),
    )
    .expect("enum serializes");
    let receipt =
        serde_json::to_string(receipt).map_err(|e| super::Error::Corrupt(e.to_string()))?;
    let snapshot =
        serde_json::to_string(snapshot).map_err(|e| super::Error::Corrupt(e.to_string()))?;
    Ok(format!(
        "{{\"delivery_key\":{key},\"final_result\":{final_result},\"receipt\":{receipt},\"snapshot\":{snapshot},\"event\":{event},\"events\":{events}}}"
    ))
}

pub const RECEIPT: &str = r#"INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES(json_extract(?1,'$.delivery_key'),json_extract(?1,'$.receipt.command.request_id'),json_extract(?1,'$.snapshot')) ON CONFLICT(delivery_id) DO UPDATE SET receipt=excluded.receipt WHERE json_extract(excluded.receipt,'$.revision')>=json_extract(delivery_outcomes.receipt,'$.revision')"#;
pub const IDENTITY: &str = r#"INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(SELECT 1 FROM delivery_outcomes WHERE delivery_id=json_extract(?1,'$.delivery_key') AND (json_extract(receipt,'$.create.command') != json_extract(?1,'$.receipt.command') OR COALESCE(json_extract(receipt,'$.create.audit_version'),'legacy') != COALESCE(json_extract(?1,'$.receipt.audit_version'),'legacy') OR (json_extract(receipt,'$.revision')=json_extract(?1,'$.snapshot.revision') AND receipt != json_extract(?1,'$.snapshot'))))"#;
const PARENTS: &str = r#"INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.receipt.command.request_id'))"#;
fn invitation_identity() -> String {
    let batch = super::bulk::delivery_identity("$.receipt.command", false);
    format!(
        r#"INSERT INTO projection_assertions(invariant)
SELECT NOT EXISTS(SELECT 1 FROM deliveries WHERE id=json_extract(?1,'$.delivery_key') AND (kind != 'repo' OR id != json_extract(?1,'$.delivery_key') OR request_id != json_extract(?1,'$.receipt.command.request_id') OR target_id != json_extract(?1,'$.receipt.command.repo_id')))
AND ((json_extract(?1,'$.receipt.command.batch') IS NULL AND EXISTS(SELECT 1 FROM invitation_requests r JOIN invitation_links l ON l.id=r.invitation_link_id WHERE r.id=json_extract(?1,'$.receipt.command.request_id') AND r.invitation_link_id=json_extract(?1,'$.receipt.command.link_id') AND r.requester_id=json_extract(?1,'$.receipt.command.requester_id') AND l.account_id=json_extract(?1,'$.receipt.command.account_id'))) OR {batch})"#
    )
}
const INITIAL: &str = r#"INSERT INTO deliveries(id,request_id,target_id,delivery_generation,created_at,updated_at) VALUES(json_extract(?1,'$.delivery_key'),json_extract(?1,'$.receipt.command.request_id'),json_extract(?1,'$.receipt.command.repo_id'),json_extract(?1,'$.receipt.command.delivery_generation'),json_extract(?1,'$.receipt.command.approved_at'),json_extract(?1,'$.receipt.command.approved_at')) ON CONFLICT(id) DO NOTHING"#;
const CREATE: &str = r#"UPDATE deliveries SET
create_outcome=json_extract(?1,'$.receipt.outcome.kind'),
outcome_reason=json_extract(?1,'$.receipt.outcome.reason'),
effective_role_before=json_extract(?1,'$.receipt.outcome.from'),
upstream_invitation_id=json_extract(?1,'$.receipt.outcome.upstream_id'),
tracked_foreign=(json_extract(?1,'$.receipt.outcome.kind')='already_pending'),
tracking_stopped_reason=json_extract(?1,'$.snapshot.tracking_stopped'),
held=(json_extract(?1,'$.receipt.outcome.kind')='held'),
sent_at=CASE WHEN json_extract(?1,'$.receipt.outcome.kind') IN ('created','already_pending') THEN json_extract(?1,'$.receipt.confirmed_at') ELSE NULL END,
final_result=json_extract(?1,'$.final_result'),
updated_at=COALESCE(json_extract(?1,'$.receipt.confirmed_at'),json_extract(?1,'$.receipt.command.approved_at')),
projection_revision=json_extract(?1,'$.snapshot.revision')
WHERE id=json_extract(?1,'$.delivery_key') AND (settlement IS NULL OR settlement='sent') AND projection_revision<=json_extract(?1,'$.snapshot.revision')
AND EXISTS(SELECT 1 FROM delivery_outcomes WHERE delivery_id=deliveries.id AND json_extract(receipt,'$.revision')=json_extract(?1,'$.snapshot.revision'))"#;

/// Both adapters classify the named atomic assertions identically.
pub fn classify(message: String) -> super::Error {
    if message.contains("projection_dependency") {
        super::Error::ProjectionDependency
    } else if message.contains("projection_invariant")
        || message.contains("audit_events.account_id")
        || message.contains("delivery outcome conflict")
        || message.contains("delivery_legacy_invitation_identity")
        || message.contains("foreign tracking stop conflict")
        || message.contains("settled invitation writer conflict")
    {
        super::Error::ProjectionInvariant(message)
    } else {
        super::Error::Database(message)
    }
}

pub fn statements() -> Vec<String> {
    vec![
        PARENTS.into(),
        invitation_identity(),
        IDENTITY.into(),
        INITIAL.into(),
        RECEIPT.into(),
        CREATE.into(),
        audit_statement(),
        settlement_audit_statement(),
        super::audit_write::retained_events(),
        SETTLED.into(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}

fn settlement_audit_statement() -> String {
    format!(
        r#"INSERT INTO audit_events(id, account_id, occurred_at, event_type, actor_kind, actor_id, target_kind, target_id, metadata, request_id)
    SELECT json_extract(?1,'$.snapshot.settlement.event.id'), json_extract(?1,'$.snapshot.settlement.event.account_id'),
      json_extract(?1,'$.snapshot.settlement.event.occurred_at'), json_extract(?1,'$.snapshot.settlement.event.event_type'),
      json_extract(?1,'$.snapshot.settlement.event.actor_kind'), json_extract(?1,'$.snapshot.settlement.event.actor_id'),
      json_extract(?1,'$.snapshot.settlement.event.target_kind'), json_extract(?1,'$.snapshot.settlement.event.target_id'),
      json_extract(?1,'$.snapshot.settlement.event.metadata'), json_extract(?1,'$.snapshot.settlement.event.request_id')
    WHERE json_type(?1,'$.snapshot.settlement') = 'object' {}"#,
        super::audit_write::INSTALLATION_REPLAY
    )
}

const SETTLED: &str = r#"UPDATE deliveries SET settlement=json_extract(?1,'$.snapshot.settlement.state'),
updated_at=json_extract(?1,'$.snapshot.settlement.event.occurred_at'),outcome_reason=NULL,
settlement_reason=json_extract(?1,'$.snapshot.settlement.event.metadata.reason'),
final_result=json_extract(?1,'$.final_result'),
projection_revision=json_extract(?1,'$.snapshot.revision')
WHERE id=json_extract(?1,'$.delivery_key') AND json_type(?1,'$.snapshot.settlement')='object' AND EXISTS(SELECT 1 FROM delivery_outcomes WHERE delivery_id=deliveries.id AND json_extract(receipt,'$.revision')=json_extract(?1,'$.snapshot.revision'))"#;

pub fn audit_statement() -> String {
    format!(
        r#"INSERT INTO audit_events(id, account_id, occurred_at, event_type, actor_kind, actor_id, target_kind, target_id, metadata, request_id)
        SELECT json_extract(?1,'$.event.id'), json_extract(?1,'$.event.account_id'), json_extract(?1,'$.event.occurred_at'),
        json_extract(?1,'$.event.event_type'), json_extract(?1,'$.event.actor_kind'), NULL, json_extract(?1,'$.event.target_kind'),
        json_extract(?1,'$.event.target_id'), json_extract(?1,'$.event.metadata'), NULL
        WHERE json_type(?1,'$.event') != 'null' {}"#,
        super::audit_write::INSTALLATION_REPLAY
    )
}

/// Projected receipts of one request, in invitation order.
pub const GET: &str = "SELECT d.id,o.receipt FROM delivery_outcomes o JOIN deliveries d ON d.id=o.delivery_id WHERE d.kind='repo' AND d.id=?1";
pub const FOR_REQUEST: &str = "SELECT d.id,o.receipt FROM delivery_outcomes o JOIN deliveries d ON d.id=o.delivery_id WHERE d.kind='repo' AND o.request_id = ?1 ORDER BY o.delivery_id";

#[derive(Debug, serde::Deserialize)]
pub struct ReceiptRow {
    pub id: String,
    pub receipt: String,
}

impl ReceiptRow {
    pub fn decode(self) -> super::Result<DeliverySnapshot> {
        let snapshot: DeliverySnapshot = serde_json::from_str(&self.receipt)
            .map_err(|e| super::Error::Corrupt(e.to_string()))?;
        if snapshot.create.command.delivery_key() != self.id {
            return Err(super::Error::Corrupt(
                "repository receipt key mismatch".into(),
            ));
        }
        crate::delivery_audit::repository_target(&snapshot.create)
            .map_err(|e| super::Error::Corrupt(e.into()))?;
        Ok(snapshot)
    }
}
