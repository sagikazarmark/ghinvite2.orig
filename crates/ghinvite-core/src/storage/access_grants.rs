//! Access removal fences and independently retried read projection.
use super::{Error, Result};
use crate::access_grant::{AccessGrantSnapshot, RemovalCommand};
use async_trait::async_trait;
use serde::Deserialize;
#[async_trait]
pub trait AccessGrantStorage: Send + Sync + 'static {
    async fn claim_removal_attempt(&self, input: &RemovalCommand) -> Result<Option<u64>>;
    async fn removal_attempt_exists(&self, input: &RemovalCommand) -> Result<bool>;
    async fn reject_removal_attempt(&self, input: &RemovalCommand, generation: u64) -> Result<()>;
    async fn project_access_grant(&self, snapshot: &AccessGrantSnapshot) -> Result<()>;
    async fn get_access_grant(&self, storage_id: &str) -> Result<Option<AccessGrantSnapshot>>;
}
fn invariant(message: &str) -> Error {
    Error::ProjectionInvariant(message.into())
}
pub fn encode_attempt(input: &RemovalCommand) -> Result<String> {
    input.grant.validate().map_err(invariant)?;
    serde_json::to_string(input).map_err(|e| Error::Corrupt(e.to_string()))
}
pub const CLAIM: &str = "INSERT INTO delivery_attempts(delivery_key,step,command) VALUES(?1,?2,?3) ON CONFLICT(delivery_key,step) DO UPDATE SET attempt_generation=attempt_generation+1,retryable=0 WHERE retryable=1 AND command=excluded.command RETURNING attempt_generation AS generation";
pub const ATTEMPT: &str =
    "SELECT command,retryable FROM delivery_attempts WHERE delivery_key=?1 AND step=?2";
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
            Err(invariant("access removal attempt conflict"))
        }
    })
    .transpose()
}
pub const GET: &str = "SELECT id,receipt FROM access_grants WHERE id=?1";
#[derive(Deserialize)]
pub struct SnapshotRow {
    pub id: String,
    pub receipt: String,
}
impl SnapshotRow {
    pub fn decode(&self) -> Result<AccessGrantSnapshot> {
        let snapshot: AccessGrantSnapshot =
            serde_json::from_str(&self.receipt).map_err(|e| Error::Corrupt(e.to_string()))?;
        snapshot.validate().map_err(invariant)?;
        if snapshot.command.storage_id() != self.id {
            return Err(invariant("access grant storage identity"));
        }
        Ok(snapshot)
    }
}
pub const SOURCE: &str = "SELECT receipt FROM delivery_outcomes WHERE delivery_id=?1";
#[derive(Deserialize)]
pub struct SourceRow {
    pub receipt: String,
}
pub fn source_id(snapshot: &AccessGrantSnapshot) -> String {
    let command = &snapshot.command;
    let key = crate::delivery::delivery_key(
        command.request_id,
        command.target.delivery_target(),
        command.delivery_generation,
    );
    if command.target.app() == crate::account::App::Org {
        format!("org:{key}")
    } else {
        key
    }
}
pub fn encode_projection(
    snapshot: &AccessGrantSnapshot,
    previous: Option<&SnapshotRow>,
    source: Option<&SourceRow>,
) -> Result<String> {
    snapshot.validate().map_err(invariant)?;
    let source = source.ok_or(Error::ProjectionDependency)?;
    let grants = if snapshot.command.target.app() == crate::account::App::Org {
        let source: crate::org_delivery::OrganizationDeliverySnapshot =
            serde_json::from_str(&source.receipt).map_err(|e| Error::Corrupt(e.to_string()))?;
        crate::access_grant::accepted_organization(&source).map_err(invariant)?
    } else {
        let source: crate::delivery::DeliverySnapshot =
            serde_json::from_str(&source.receipt).map_err(|e| Error::Corrupt(e.to_string()))?;
        super::delivery_projection::encode(&source)?;
        crate::access_grant::accepted_repository(&source)
            .map_err(invariant)?
            .into_iter()
            .collect()
    };
    if grants.is_empty() {
        return Err(Error::ProjectionDependency);
    }
    if !grants.contains(&snapshot.command) {
        return Err(invariant("access grant source receipt mismatch"));
    }
    let previous_snapshot = previous.map(SnapshotRow::decode).transpose()?;
    let apply = if let Some(old) = &previous_snapshot {
        if old.command != snapshot.command {
            return Err(invariant("access grant command changed"));
        }
        if snapshot.revision < old.revision {
            false
        } else {
            old.validate_successor(snapshot).map_err(invariant)?;
            true
        }
    } else {
        true
    };
    let source_key = source_id(snapshot);
    let team_path = if matches!(
        snapshot.command.target,
        crate::access_grant::GrantTarget::Team { .. }
    ) {
        Some(format!("$.teams.\"{}\"", snapshot.command.target.id()))
    } else {
        None
    };
    let event = terminal_audit(snapshot)?;
    Ok(serde_json::json!({
        "event":event,
        "id":snapshot.command.storage_id(),"source_key":source_key,"source_receipt":source.receipt,"team_path":team_path,"request_id":snapshot.command.request_id,
        "target_kind":snapshot.command.target.kind(),"target_id":snapshot.command.target.id(),
        "generation":snapshot.command.delivery_generation,"account_id":snapshot.command.account_id,
        "link_id":snapshot.command.link_id,"remove_at":snapshot.command.remove_at().map_err(invariant)?,
        "revision":snapshot.revision,"receipt":serde_json::to_string(snapshot).map_err(|e| Error::Corrupt(e.to_string()))?,
        "previous":previous.map(|p| &p.receipt),"apply":apply
    }).to_string())
}
fn terminal_audit(snapshot: &AccessGrantSnapshot) -> Result<Option<serde_json::Value>> {
    use crate::{
        access_grant::RemovalOutcome,
        audit::{ActorKind, AuditEvent, EventType, TargetKind},
    };
    use sha2::{Digest, Sha256};
    let (at, kind) = match snapshot.outcome {
        RemovalOutcome::Removed { at, .. } => (at, EventType::AccessRemoved),
        RemovalOutcome::RemovalSkipped { at, .. } => (at, EventType::AccessRemovalSkipped),
        _ => return Ok(None),
    };
    let logical_id = format!("access/{}/terminal", snapshot.command.storage_id());
    let digest = Sha256::digest(format!("ghinvite/access-audit/v1/{logical_id}"));
    let id = crate::AuditEventId::from_ulid(ulid::Ulid::from_bytes(
        digest[..16].try_into().expect("digest"),
    ));
    let event = AuditEvent {
        id,
        account_id: snapshot.command.account_id,
        occurred_at: at,
        event_type: kind,
        actor_kind: ActorKind::System,
        actor_id: None,
        target_kind: TargetKind::AccessGrant,
        target_id: snapshot.command.storage_id(),
        metadata: serde_json::to_value(&snapshot.outcome)
            .map_err(|e| Error::Corrupt(e.to_string()))?,
        request_id: Some(snapshot.command.request_id.to_string()),
    };
    Ok(Some(
        serde_json::json!({"id":event.id,"logical_id":logical_id,"kind":kind.as_str(),"at":at,"metadata":event.metadata.to_string(),"content":serde_json::to_string(&event).map_err(|e|Error::Corrupt(e.to_string()))?}),
    ))
}
pub fn statements() -> Vec<String> {
    vec![
        "INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.request_id'))".into(),
        "INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM delivery_outcomes WHERE delivery_id=json_extract(?1,'$.source_key'))".into(),
        "INSERT INTO projection_assertions(unchanged) SELECT COALESCE((SELECT receipt FROM delivery_outcomes WHERE delivery_id=json_extract(?1,'$.source_key')),'')=json_extract(?1,'$.source_receipt')".into(),
        // Acceptance may be queued ahead of its SQL projection. Only the actual
        // source receipt can make this projection visible; dependency retries
        // cannot authorize an HTTP write.
        "INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM delivery_outcomes o WHERE o.delivery_id=json_extract(?1,'$.source_key') AND CASE WHEN json_extract(?1,'$.team_path') IS NULL THEN json_extract(o.receipt,'$.create.outcome.kind')='created' AND json_extract(o.receipt,'$.settlement.state')='accepted' ELSE json_extract(o.receipt,json_extract(?1,'$.team_path')||'.create.outcome.kind')='created' AND json_extract(o.receipt,json_extract(?1,'$.team_path')||'.settlement.state')='accepted' END)".into(),
        // Full request/account/origin binding; linkless bulk requests use their
        // retained queue account, never an invented link.
        "INSERT INTO projection_assertions(invariant) SELECT EXISTS(SELECT 1 FROM invitation_requests r WHERE r.id=json_extract(?1,'$.request_id') AND r.requester_id=json_extract(json_extract(?1,'$.receipt'),'$.command.requester_id') AND r.invitation_link_id IS json_extract(?1,'$.link_id') AND COALESCE(r.queue_account_id,(SELECT account_id FROM invitation_links WHERE id=r.invitation_link_id))=json_extract(?1,'$.account_id'))".into(),
        "INSERT INTO projection_assertions(unchanged) SELECT COALESCE((SELECT receipt FROM access_grants WHERE id=json_extract(?1,'$.id')),'')=COALESCE(json_extract(?1,'$.previous'),'')".into(),
        "INSERT INTO access_grants(id,request_id,target_kind,target_id,delivery_generation,account_id,link_id,remove_at,revision,receipt) SELECT json_extract(?1,'$.id'),json_extract(?1,'$.request_id'),json_extract(?1,'$.target_kind'),json_extract(?1,'$.target_id'),json_extract(?1,'$.generation'),json_extract(?1,'$.account_id'),json_extract(?1,'$.link_id'),json_extract(?1,'$.remove_at'),json_extract(?1,'$.revision'),json_extract(?1,'$.receipt') WHERE json_extract(?1,'$.apply')=1 ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,receipt=excluded.receipt WHERE excluded.revision>access_grants.revision".into(),
        "INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(SELECT 1 FROM audit_events a WHERE (a.id=json_extract(?1,'$.event.id') OR a.projection_event_id=json_extract(?1,'$.event.logical_id')) AND (a.id IS NOT json_extract(?1,'$.event.id') OR a.projection_event_id IS NOT json_extract(?1,'$.event.logical_id') OR a.projection_content IS NOT json_extract(?1,'$.event.content')))".into(),
        "INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,request_id,projection_event_id,projection_content) SELECT json_extract(?1,'$.event.id'),json_extract(?1,'$.account_id'),json_extract(?1,'$.event.at'),json_extract(?1,'$.event.kind'),'system','access_grant',json_extract(?1,'$.id'),json_extract(?1,'$.event.metadata'),json_extract(?1,'$.request_id'),json_extract(?1,'$.event.logical_id'),json_extract(?1,'$.event.content') WHERE json_extract(?1,'$.event') IS NOT NULL ON CONFLICT(id) DO NOTHING".into(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}
pub fn classify(message: String) -> Error {
    super::projection::sql::classify(message)
}
