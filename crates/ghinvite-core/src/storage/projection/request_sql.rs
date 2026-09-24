//! Request-owned snapshots and immutable audit facts, atomically applied on SQLx/D1.
use super::*;
use crate::storage::{Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn encode(envelope: &RequestProjectionEnvelope) -> Result<String> {
    let request = &envelope.request;
    let invalid = || Error::ProjectionInvariant("invalid request envelope".into());
    if request.revision == 0
        || request.account_id == 0
        || request.requester_id == 0
        || (request.state == RequestState::Pending) != request.decision.is_none()
        || (request.state == RequestState::Pending && request.decision_deadline.is_none())
        || envelope.events.len() > 3
    {
        return Err(invalid());
    }
    let mut value = serde_json::to_value(envelope).map_err(|_| invalid())?;
    value["content"] = json!(request);
    let mut identities = std::collections::BTreeMap::new();
    for (event, encoded) in envelope
        .events
        .iter()
        .zip(value["events"].as_array_mut().unwrap())
    {
        if identities
            .insert(&event.event_id, event)
            .is_some_and(|old| old != event)
        {
            return Err(invalid());
        }
        if event.target_id != request.request_id.to_string()
            || event.event_id.is_empty()
            || !matches!(
                event.kind,
                EventType::RequestCreated
                    | EventType::RequestApproved
                    | EventType::RequestDeclined
                    | EventType::RequestExpired
            )
        {
            return Err(invalid());
        }
        let digest = Sha256::digest(format!("ghinvite/projection/audit/{}", event.event_id));
        let bytes: [u8; 16] = digest[..16].try_into().unwrap();
        encoded["id"] = json!(ulid::Ulid::from_bytes(bytes).to_string());
        encoded["content"] = json!([request.account_id, event]);
    }
    serde_json::to_string(&value).map_err(|_| invalid())
}

pub const STATEMENTS: &[&str] = &[
    r#"INSERT INTO projection_assertions(dependency, invariant) SELECT
      EXISTS(SELECT 1 FROM invitation_links WHERE id=json_extract(?1,'$.request.link_id'))
      AND NOT EXISTS(SELECT 1 FROM (
        SELECT json_extract(?1,'$.request.requester_id') AS id
        UNION SELECT json_extract(?1,'$.request.decision.decided_by')
        UNION SELECT json_extract(value,'$.actor_id') FROM json_each(?1,'$.events')
      ) WHERE id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM users WHERE user_id=id)),
      NOT EXISTS(SELECT 1 FROM invitation_links WHERE id=json_extract(?1,'$.request.link_id')
        AND account_id != json_extract(?1,'$.request.account_id'))"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT
      NOT EXISTS(SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.request.request_id') AND (
        invitation_link_id IS NOT json_extract(?1,'$.request.link_id') OR
        requester_id IS NOT json_extract(?1,'$.request.requester_id') OR
        justification IS NOT json_extract(?1,'$.request.justification') OR
        created_at IS NOT json_extract(?1,'$.request.admitted_at') OR
        decision_deadline IS NOT json_extract(?1,'$.request.decision_deadline') OR
        (projection_revision=json_extract(?1,'$.request.revision') AND (
          (projection_content IS NOT NULL AND projection_content IS NOT json_extract(?1,'$.content')) OR
          state IS NOT json_extract(?1,'$.request.state') OR
          decided_by IS NOT json_extract(?1,'$.request.decision.decided_by') OR
          decided_at IS NOT json_extract(?1,'$.request.decision.effective_at') OR
          decline_reason IS NOT json_extract(?1,'$.request.decision.decline_reason')))))
      AND NOT EXISTS(SELECT 1 FROM audit_events a JOIN json_each(?1,'$.events') j
        ON a.id=json_extract(j.value,'$.id') OR a.projection_event_id=json_extract(j.value,'$.event_id')
         WHERE a.projection_content IS NOT json_extract(j.value,'$.content') OR
           a.account_id IS NOT json_extract(?1,'$.request.account_id') OR
           a.projection_event_id IS NOT json_extract(j.value,'$.event_id') OR
           a.id IS NOT json_extract(j.value,'$.id') OR
           a.event_type IS NOT json_extract(j.value,'$.kind') OR
           a.actor_id IS NOT json_extract(j.value,'$.actor_id') OR
           a.actor_kind IS NOT CASE WHEN json_extract(j.value,'$.actor_id') IS NULL THEN 'system' ELSE 'user' END OR
           a.target_kind IS NOT 'invitation_request' OR
           a.target_id IS NOT json_extract(j.value,'$.target_id') OR
           a.occurred_at IS NOT json_extract(j.value,'$.effective_at') OR
           a.evaluated_at IS NOT json_extract(j.value,'$.evaluated_at') OR
           a.metadata IS NOT NULL OR a.request_id IS NOT NULL)"#,
    r#"INSERT INTO invitation_requests(id, invitation_link_id, requester_id, justification, state, created_at,
      decision_deadline, projection_revision, decided_by, decided_at, decline_reason, projection_content)
      SELECT json_extract(?1,'$.request.request_id'),json_extract(?1,'$.request.link_id'),
        json_extract(?1,'$.request.requester_id'),json_extract(?1,'$.request.justification'),
        json_extract(?1,'$.request.state'),json_extract(?1,'$.request.admitted_at'),
        json_extract(?1,'$.request.decision_deadline'),json_extract(?1,'$.request.revision'),
        json_extract(?1,'$.request.decision.decided_by'),json_extract(?1,'$.request.decision.effective_at'),
        json_extract(?1,'$.request.decision.decline_reason'),json_extract(?1,'$.content') WHERE true
      ON CONFLICT(id) DO UPDATE SET state=excluded.state,projection_revision=excluded.projection_revision,
        decided_by=excluded.decided_by,decided_at=excluded.decided_at,decline_reason=excluded.decline_reason,
        projection_content=excluded.projection_content
      WHERE excluded.projection_revision > invitation_requests.projection_revision"#,
    r#"INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,actor_id,target_kind,target_id,
      metadata,projection_event_id,projection_content,evaluated_at)
      SELECT json_extract(value,'$.id'),json_extract(?1,'$.request.account_id'),json_extract(value,'$.effective_at'),
        json_extract(value,'$.kind'),CASE WHEN json_extract(value,'$.actor_id') IS NULL THEN 'system' ELSE 'user' END,
        json_extract(value,'$.actor_id'),'invitation_request',json_extract(value,'$.target_id'),NULL,
        json_extract(value,'$.event_id'),json_extract(value,'$.content'),json_extract(value,'$.evaluated_at')
      FROM json_each(?1,'$.events') WHERE true ON CONFLICT(id) DO NOTHING"#,
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];
