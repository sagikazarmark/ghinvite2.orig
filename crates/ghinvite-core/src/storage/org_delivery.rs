//! Organization attempt fences and atomic projection SQL, shared by both drivers.
use super::{Error, Result};
use crate::org_delivery::{OrganizationAttempt, OrganizationDeliverySnapshot};
use async_trait::async_trait;
use serde::Deserialize;

#[async_trait]
pub trait OrgDeliveryStorage: Send + Sync + 'static {
    /// Independent per-step telemetry cannot change receipts or lifecycle.
    async fn project_organization_telemetry(
        &self,
        telemetry: &super::organization_telemetry::OrganizationStepTelemetry,
    ) -> Result<()>;
    async fn claim_organization_attempt(
        &self,
        attempt: &OrganizationAttempt,
    ) -> Result<Option<u64>>;
    async fn organization_attempt_exists(&self, attempt: &OrganizationAttempt) -> Result<bool>;
    async fn reject_organization_attempt(
        &self,
        attempt: &OrganizationAttempt,
        generation: u64,
    ) -> Result<()>;
    async fn project_organization_delivery(
        &self,
        snapshot: &OrganizationDeliverySnapshot,
    ) -> Result<()> {
        self.project_organization_delivery_with_events(&snapshot.clone().into())
            .await
    }
    async fn project_organization_delivery_with_events(
        &self,
        projection: &crate::delivery_audit::OrganizationDeliveryProjection,
    ) -> Result<()>;
    async fn get_organization_delivery(
        &self,
        storage_id: &str,
    ) -> Result<Option<OrganizationDeliverySnapshot>>;
}

pub fn encode_attempt(attempt: &OrganizationAttempt) -> Result<String> {
    super::integer::validate(attempt)?;
    attempt.validate().map_err(invariant)?;
    serde_json::to_string(attempt).map_err(|e| Error::Corrupt(e.to_string()))
}
fn invariant(message: &str) -> Error {
    Error::ProjectionInvariant(message.into())
}

// Physical fence identity includes the delivery kind. The public object key
// deliberately does not: RepositoryDelivery and OrganizationDelivery are distinct services.
pub const CLAIM: &str = r#"INSERT INTO delivery_attempts(delivery_key,step,command)
SELECT ?1,?2,?3 WHERE NOT EXISTS(SELECT 1 FROM delivery_attempts WHERE step=?2 AND json_extract(command,'$.create.invitation_id')=json_extract(?3,'$.create.invitation_id') AND command!=?3)
ON CONFLICT(delivery_key,step) DO UPDATE SET attempt_generation=attempt_generation+1,retryable=0
WHERE retryable=1 AND command=excluded.command RETURNING attempt_generation AS generation"#;
pub const ATTEMPT: &str = r#"SELECT command,retryable FROM delivery_attempts WHERE step=?2 AND (delivery_key=?1 OR json_extract(command,'$.create.invitation_id')=json_extract(?3,'$.create.invitation_id'))"#;
pub const REJECT: &str = "UPDATE delivery_attempts SET retryable=1 WHERE delivery_key=?1 AND step=?2 AND command=?3 AND attempt_generation=?4";
#[derive(Deserialize)]
pub struct AttemptRow {
    pub command: String,
    pub retryable: i64,
}
pub fn check_attempt(row: Option<AttemptRow>, encoded: &str) -> Result<Option<bool>> {
    row.map(|row| {
        if row.command == encoded {
            Ok(row.retryable == 0)
        } else {
            Err(invariant("organization attempt identity conflict"))
        }
    })
    .transpose()
}

pub const GET: &str = "SELECT d.id,o.receipt FROM delivery_outcomes o JOIN deliveries d ON d.id=o.delivery_id WHERE d.kind='org' AND d.id=?1";
#[derive(Deserialize)]
pub struct SnapshotRow {
    pub id: String,
    pub receipt: String,
}
impl SnapshotRow {
    pub fn decode(&self) -> Result<OrganizationDeliverySnapshot> {
        let snapshot: OrganizationDeliverySnapshot =
            serde_json::from_str(&self.receipt).map_err(|e| Error::Corrupt(e.to_string()))?;
        super::integer::validate(&snapshot)?;
        snapshot.validate().map_err(|e| Error::Corrupt(e.into()))?;
        if snapshot.create.command.storage_id() != self.id {
            return Err(Error::Corrupt("organization receipt key mismatch".into()));
        }
        Ok(snapshot)
    }
}
/// Compare-and-swap protects successor validation. Snapshot freshness controls
/// only row updates: retained events are inserted for every validated envelope.
pub fn encode_projection(
    snapshot: &OrganizationDeliverySnapshot,
    previous: Option<&SnapshotRow>,
) -> Result<Option<String>> {
    encode_projection_with_events(&snapshot.clone().into(), previous).map(Some)
}

pub fn encode_projection_with_events(
    projection: &crate::delivery_audit::OrganizationDeliveryProjection,
    previous: Option<&SnapshotRow>,
) -> Result<String> {
    let snapshot = &projection.snapshot;
    super::integer::validate(snapshot)?;
    snapshot.validate().map_err(invariant)?;
    let advance = match previous {
        Some(previous) => snapshot
            .validate_successor(&previous.decode()?)
            .map_err(invariant)?,
        None => true,
    };
    let mut retained = events(snapshot);
    retained.extend(
        projection
            .events()
            .map_err(invariant)?
            .into_iter()
            .map(|event| serde_json::to_value(event).expect("audit event serializes")),
    );
    Ok(serde_json::json!({
        "id": snapshot.create.command.storage_id(),
        "expected": previous.map(|p| p.receipt.as_str()),
        "advance": advance,
        "snapshot": snapshot,
        "events": retained,
        "final_result": crate::request_completion::organization(snapshot).into_iter()
            .find(|result| result.target.kind == crate::request_completion::TargetKind::Organization)
            .map(|result| result.result),
    }).to_string())
}

const PARENTS: &str = r#"INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.snapshot.create.command.request_id'))"#;
fn identity() -> String {
    let batch = super::bulk::delivery_identity("$.snapshot.create.command", true);
    format!(
        r#"INSERT INTO projection_assertions(invariant)
SELECT NOT EXISTS(SELECT 1 FROM deliveries WHERE id=json_extract(?1,'$.id') AND (kind!='org' OR id!=json_extract(?1,'$.id') OR request_id!=json_extract(?1,'$.snapshot.create.command.request_id') OR target_id!=json_extract(?1,'$.snapshot.create.command.organization.org_id')))
AND ((json_extract(?1,'$.snapshot.create.command.batch') IS NULL AND EXISTS(SELECT 1 FROM invitation_requests r JOIN invitation_links l ON l.id=r.invitation_link_id WHERE r.id=json_extract(?1,'$.snapshot.create.command.request_id') AND r.invitation_link_id=json_extract(?1,'$.snapshot.create.command.link_id') AND r.requester_id=json_extract(?1,'$.snapshot.create.command.requester_id') AND l.account_id=json_extract(?1,'$.snapshot.create.command.account_id'))
AND EXISTS(SELECT 1 FROM invitation_link_targets t WHERE t.link_id=json_extract(?1,'$.snapshot.create.command.link_id') AND t.kind='org' AND t.target_id=json_extract(?1,'$.snapshot.create.command.organization.org_id') AND t.grant=json_extract(?1,'$.snapshot.create.command.organization.grant'))
AND NOT EXISTS(SELECT 1 FROM json_each(?1,'$.snapshot.create.command.teams') supplied WHERE NOT EXISTS(SELECT 1 FROM invitation_link_targets t WHERE t.link_id=json_extract(?1,'$.snapshot.create.command.link_id') AND t.kind='team' AND t.target_id=json_extract(supplied.value,'$.team_id') AND t.grant=json_extract(supplied.value,'$.grant')))) OR {batch})"#
    )
}
const CAS: &str = r#"INSERT INTO projection_assertions(unchanged) SELECT
COALESCE((SELECT receipt FROM delivery_outcomes WHERE delivery_id=json_extract(?1,'$.id')),'')=COALESCE(json_extract(?1,'$.expected'),'')"#;
const INITIAL: &str = r#"INSERT INTO deliveries(id,request_id,kind,target_id,delivery_generation,created_at,updated_at)
VALUES(json_extract(?1,'$.id'),json_extract(?1,'$.snapshot.create.command.request_id'),'org',json_extract(?1,'$.snapshot.create.command.organization.org_id'),json_extract(?1,'$.snapshot.create.command.delivery_generation'),json_extract(?1,'$.snapshot.create.command.approved_at'),json_extract(?1,'$.snapshot.create.command.approved_at')) ON CONFLICT(id) DO NOTHING"#;
const RECEIPT: &str = r#"INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES(json_extract(?1,'$.id'),json_extract(?1,'$.snapshot.create.command.request_id'),json_extract(?1,'$.snapshot')) ON CONFLICT(delivery_id) DO UPDATE SET receipt=excluded.receipt WHERE json_extract(?1,'$.advance')=1"#;
const DELIVERY: &str = r#"UPDATE deliveries SET
create_outcome=json_extract(?1,'$.snapshot.create.outcome.kind'),outcome_reason=json_extract(?1,'$.snapshot.create.outcome.reason'),
upstream_invitation_id=json_extract(?1,'$.snapshot.create.outcome.upstream_id'),tracked_foreign=(json_extract(?1,'$.snapshot.create.outcome.kind')='already_pending'),
tracking_stopped_reason=json_extract(?1,'$.snapshot.tracking_stopped'),held=(json_extract(?1,'$.snapshot.create.outcome.kind')='held'),
sent_at=CASE WHEN json_extract(?1,'$.snapshot.create.outcome.kind') IN ('created','already_pending') THEN json_extract(?1,'$.snapshot.create.confirmed_at') ELSE NULL END,
settlement=json_extract(?1,'$.snapshot.settlement.state'),settlement_reason=json_extract(?1,'$.snapshot.settlement.event.metadata.reason'),
final_result=json_extract(?1,'$.final_result'),
updated_at=COALESCE(json_extract(?1,'$.snapshot.settlement.event.occurred_at'),json_extract(?1,'$.snapshot.create.confirmed_at'),json_extract(?1,'$.snapshot.create.command.approved_at')),
projection_revision=json_extract(?1,'$.snapshot.revision') WHERE id=json_extract(?1,'$.id') AND kind='org' AND json_extract(?1,'$.advance')=1"#;
const TEAMS: &str = r#"INSERT INTO delivery_steps(delivery_id,step,receipt,outcome,reason,updated_at,projection_revision)
SELECT json_extract(?1,'$.id'),'team:'||key,value,json_extract(value,'$.create.outcome.kind'),json_extract(value,'$.create.outcome.reason'),COALESCE(json_extract(value,'$.settlement.observed_at'),json_extract(value,'$.create.confirmed_at'),json_extract(?1,'$.snapshot.create.command.approved_at')),json_extract(value,'$.revision')
FROM json_each(?1,'$.snapshot.teams') WHERE json_extract(?1,'$.advance')=1
ON CONFLICT(delivery_id,step) DO UPDATE SET receipt=excluded.receipt,outcome=excluded.outcome,reason=excluded.reason,updated_at=excluded.updated_at,projection_revision=excluded.projection_revision"#;
pub fn statements() -> Vec<String> {
    vec![
        PARENTS.into(),
        identity(),
        CAS.into(),
        INITIAL.into(),
        RECEIPT.into(),
        DELIVERY.into(),
        TEAMS.into(),
        super::audit_write::retained_events(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}
pub fn classify(message: String) -> Error {
    // A racing valid projector must re-read and revalidate, never be mistaken
    // for an immutable-domain conflict.
    if message.contains("projection_changed") {
        Error::Database("organization projection changed; retry".into())
    } else if message.contains("organization step") {
        invariant(&message)
    } else {
        super::delivery_projection::classify(message)
    }
}

fn events(snapshot: &OrganizationDeliverySnapshot) -> Vec<serde_json::Value> {
    use crate::{
        audit::{ActorKind, EventType, TargetKind},
        org_delivery::{OrganizationCreateOutcome as Org, TeamCreateOutcome as Team, TeamCreation},
    };
    use sha2::{Digest, Sha256};
    let command = &snapshot.create.command;
    let canonical = !snapshot.create.audit_version.is_legacy();
    let mut events = vec![];
    let mut push = |target: String,
                    target_kind: TargetKind,
                    kind: EventType,
                    actor: ActorKind,
                    at: chrono::DateTime<chrono::Utc>,
                    metadata: serde_json::Value,
                    suffix: &str| {
        let namespace = if snapshot.create.audit_version
            == crate::delivery_audit::DeliveryAuditVersion::CanonicalV2
        {
            "ghinvite/org-delivery/audit/v2"
        } else if canonical {
            "ghinvite/org-delivery/audit/v1"
        } else {
            "ghinvite/org-delivery/audit"
        };
        let digest = Sha256::digest(format!("{namespace}/{target}/{suffix}"));
        let id = ulid::Ulid::from_bytes(digest[..16].try_into().expect("digest length"));
        events.push(serde_json::json!({"id":id.to_string(),"account_id":command.account_id,"occurred_at":at,"event_type":kind,"actor_kind":actor,"actor_id":null,"target_kind":target_kind,"target_id":target,"metadata":metadata,"request_id":command.request_id}));
    };
    if let Some(at) = snapshot.create.confirmed_at {
        let (kind, actor) = match snapshot.create.outcome {
            Org::Created { .. } => (
                if canonical {
                    EventType::OrgInvitationSent
                } else {
                    EventType::InvitationSent
                },
                ActorKind::System,
            ),
            Org::AlreadySatisfied { .. } => {
                (EventType::DeliveryAlreadySatisfied, ActorKind::Github)
            }
            Org::AlreadyPending { .. } => (EventType::DeliveryAlreadyPending, ActorKind::System),
            Org::Aborted { .. } => (EventType::DeliveryAborted, ActorKind::System),
            Org::Failed { .. } => (EventType::InvitationSendFailed, ActorKind::System),
            _ => unreachable!("validated confirmation"),
        };
        let mut metadata = serde_json::json!({"outcome":snapshot.create.outcome});
        if canonical && let Org::Aborted { reason } = snapshot.create.outcome {
            metadata["reason"] = serde_json::json!(reason);
        }
        push(
            command.storage_id(),
            TargetKind::OrganizationDelivery,
            kind,
            actor,
            at,
            metadata,
            "create",
        );
    }
    for (team_id, team) in &snapshot.teams {
        let target = format!("{}/team:{team_id}", command.storage_id());
        if let Some(at) = team.create.confirmed_at {
            let kind = match team.create.outcome {
                Team::Created {
                    via: TeamCreation::PendingMembership,
                } if canonical => EventType::TeamMembershipAdded,
                Team::Created { .. } => EventType::InvitationSent,
                Team::AlreadySatisfied { .. } => EventType::DeliveryAlreadySatisfied,
                Team::Upgraded if canonical => EventType::TeamMembershipAdded,
                Team::Upgraded => EventType::DeliveryUpgraded,
                Team::Aborted { .. } => EventType::DeliveryAborted,
                Team::Failed { .. } => EventType::InvitationSendFailed,
                _ => unreachable!("validated confirmation"),
            };
            let mut metadata = serde_json::json!({"team_id":team_id,"outcome":team.create.outcome});
            if canonical && let Team::Aborted { reason } = team.create.outcome {
                metadata["reason"] = serde_json::json!(reason);
            }
            if canonical && kind == EventType::TeamMembershipAdded {
                metadata["membership_state"] =
                    serde_json::json!(if matches!(team.create.outcome, Team::Upgraded) {
                        "active"
                    } else {
                        "pending"
                    });
            }
            push(
                target.clone(),
                TargetKind::TeamDelivery,
                kind,
                ActorKind::System,
                at,
                metadata,
                "create",
            );
        }
        if let Some(settlement) = &team.settlement {
            let kind = if canonical
                && settlement.state == crate::InvitationState::Accepted
                && matches!(
                    team.create.outcome,
                    Team::Created {
                        via: TeamCreation::OrganizationInvitation { .. }
                    }
                ) {
                EventType::TeamMembershipAdded
            } else {
                super::settlement::event_type(settlement.state).expect("validated terminal state")
            };
            let mut metadata = serde_json::json!({"team_id":team_id});
            if canonical {
                if let Some(reason) = settlement.reason {
                    metadata["reason"] = serde_json::json!(reason);
                }
                if kind == EventType::TeamMembershipAdded {
                    metadata["membership_state"] = serde_json::json!("active");
                }
            }
            push(
                target,
                TargetKind::TeamDelivery,
                kind,
                ActorKind::Github,
                settlement.observed_at,
                metadata,
                "settlement",
            );
        }
    }
    if let Some(settlement) = &snapshot.settlement {
        events.push(serde_json::to_value(&settlement.event).expect("audit event serializes"));
    }
    events
}
