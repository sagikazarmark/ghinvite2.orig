//! Request-owned snapshots and immutable audit facts, atomically applied on SQLx/D1.
use super::*;
use crate::storage::{Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn encode(envelope: &RequestProjectionEnvelope) -> Result<String> {
    let invalid = || Error::ProjectionInvariant("invalid request approval proof".into());
    let mut value: serde_json::Value =
        serde_json::from_str(&encode_single(envelope)?).map_err(|_| invalid())?;
    match (&envelope.approval, envelope.request.state) {
        (Some(approval), RequestState::Lapsed) => {
            if approval.request.state != RequestState::Approved
                || approval.request.revision >= envelope.request.revision
            {
                return Err(invalid());
            }
            let mut expected = approval.request.clone();
            expected.state = RequestState::Lapsed;
            expected.revision = envelope.request.revision;
            if expected != envelope.request {
                return Err(invalid());
            }
            if let Some(proof) = &approval.reissue {
                if proof.after != approval.request
                    || proof.approval_events != approval.events
                    || envelope.events.iter().any(|event| {
                        event.kind == EventType::RequestLapsed
                            && event.event_id
                                != format!(
                                    "request.lapsed/{}/{}",
                                    envelope.request.request_id, envelope.request.revision
                                )
                    })
                {
                    return Err(invalid());
                }
                value["reissue_projection"] =
                    serde_json::from_str(&super::reissue_sql::encode(proof)?)
                        .map_err(|_| invalid())?;
            }
            let predecessor = RequestProjectionEnvelope {
                approval: None,
                request: approval.request.clone(),
                events: approval.events.clone(),
            };
            value["approval_projection"] =
                serde_json::from_str(&encode_single(&predecessor)?).map_err(|_| invalid())?;
        }
        (None, state) if state != RequestState::Lapsed => {}
        _ => return Err(invalid()),
    }
    serde_json::to_string(&value).map_err(|_| invalid())
}

/// One atomic batch: reconstruct the retained approval before completion, even
/// when the completion arrives first at the exclusive projection object. A
/// dependency retry here would block the predecessor in that object's inbox.
pub fn statements(envelope: &RequestProjectionEnvelope) -> Vec<String> {
    let mut statements = Vec::new();
    if envelope
        .approval
        .as_ref()
        .is_some_and(|a| a.reissue.is_some())
    {
        statements.extend(
            super::reissue_sql::statements()
                .iter()
                .map(|sql| sql.replace("?1", "json_extract(?1,'$.reissue_projection')")),
        );
    } else if envelope.approval.is_some() {
        statements.extend(
            STATEMENTS
                .iter()
                .map(|sql| sql.replace("?1", "json_extract(?1,'$.approval_projection')")),
        );
    }
    statements.extend(STATEMENTS.iter().map(|sql| (*sql).to_owned()));
    statements
}

pub(crate) fn encode_single(envelope: &RequestProjectionEnvelope) -> Result<String> {
    let request = &envelope.request;
    let invalid = || Error::ProjectionInvariant("invalid request envelope".into());
    crate::bulk::validate_origin(request.link_id, request.batch).map_err(|_| invalid())?;
    if request.batch.is_some()
        && (request.domain_evidence.is_some()
            || !matches!(request.state, RequestState::Approved | RequestState::Lapsed)
            || request.decision_deadline.is_some()
            || request
                .decision
                .as_ref()
                .is_none_or(|d| d.decided_by.is_none()))
    {
        return Err(invalid());
    }
    if request.revision == 0
        || request.account_id == 0
        || request.requester_id == 0
        || (request.state == RequestState::Pending) != request.decision.is_none()
        || (request.state == RequestState::Pending && request.decision_deadline.is_none())
        || (request.state == RequestState::Cancelled)
            != request
                .decision
                .as_ref()
                .is_some_and(|decision| decision.cancellation_reason.is_some())
        || request.decision.as_ref().is_some_and(|decision| {
            decision.cancellation_reason.is_some() && decision.decline_reason.is_some()
        })
        || (request.state == RequestState::Lapsed
            && (request
                .decision
                .as_ref()
                .is_none_or(|decision| decision.decline_reason.is_some())
                || envelope
                    .events
                    .iter()
                    .filter(|event| event.kind == EventType::RequestLapsed)
                    .count()
                    != 1))
        || envelope.events.len() > 3
    {
        return Err(invalid());
    }
    if let Some(message) = request
        .decision
        .as_ref()
        .and_then(|d| d.requester_message.as_ref())
        && (request.state != RequestState::Declined
            || crate::request_lifecycle::normalize_requester_message(Some(message.clone()))
                .ok()
                .flatten()
                .as_ref()
                != Some(message))
    {
        return Err(invalid());
    }
    if let Some(evidence) = &request.domain_evidence {
        crate::storage::invitation_requests::validate_evidence(
            evidence,
            request.link_id,
            request.admitted_at,
        )
        .map_err(|_| invalid())?;
    }
    crate::storage::integer::validate(envelope)?;
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
                    | EventType::RequestCancelled
                    | EventType::RequestLapsed
            )
        {
            return Err(invalid());
        }
        if event.kind == EventType::RequestLapsed {
            let decision = request.decision.as_ref().ok_or_else(invalid)?;
            if request.state != RequestState::Lapsed
                || event.actor_id.is_some()
                || (event.event_id != format!("request.lapsed/{}", request.request_id)
                    && event.event_id
                        != format!("request.lapsed/{}/{}", request.request_id, request.revision))
                || event.effective_at < decision.effective_at
                || event.evaluated_at < event.effective_at
            {
                return Err(invalid());
            }
        }
        if event.kind == EventType::RequestCancelled {
            if request.state != RequestState::Cancelled {
                return Err(invalid());
            }
            let decision = request.decision.as_ref().ok_or_else(invalid)?;
            let reason = decision.cancellation_reason.ok_or_else(invalid)?;
            if event.actor_id.is_some()
                || decision.decided_by.is_some()
                || event.event_id != decision.decision_id
                || event.effective_at != decision.effective_at
                || event.evaluated_at != decision.evaluated_at
            {
                return Err(invalid());
            }
            encoded["metadata"] = json!({"reason":reason});
        }
        if let Some(batch) = request.batch {
            if matches!(
                event.kind,
                EventType::RequestCreated | EventType::RequestApproved
            ) && event.actor_id != request.decision.as_ref().and_then(|d| d.decided_by)
            {
                return Err(invalid());
            }
            encoded["metadata"] =
                json!({"source":"batch","batch_id":batch.batch_id,"row":batch.row});
        }
        let digest = Sha256::digest(format!("ghinvite/projection/audit/{}", event.event_id));
        let bytes: [u8; 16] = digest[..16].try_into().unwrap();
        encoded["id"] = json!(ulid::Ulid::from_bytes(bytes).to_string());
        encoded["content"] = if event.kind == EventType::RequestCancelled || request.batch.is_some()
        {
            json!([request.account_id, event, encoded["metadata"]])
        } else {
            json!([request.account_id, event])
        };
    }
    serde_json::to_string(&value).map_err(|_| invalid())
}

pub const STATEMENTS: &[&str] = &[
    r#"INSERT INTO projection_assertions(dependency, invariant) SELECT
      (EXISTS(SELECT 1 FROM invitation_links WHERE id=json_extract(?1,'$.request.link_id'))
       OR EXISTS(SELECT 1 FROM bulk_batches b WHERE b.id=json_extract(?1,'$.request.batch.batch_id')
         AND json_extract(b.snapshot,'$.submitted') IS NOT NULL))
      AND (json_extract(?1,'$.request.state') != 'lapsed' OR EXISTS(
        SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.request.request_id')
        AND state IN ('approved','lapsed')))
      AND NOT EXISTS(SELECT 1 FROM (
        SELECT json_extract(?1,'$.request.requester_id') AS id
        UNION SELECT json_extract(?1,'$.request.decision.decided_by')
        UNION SELECT json_extract(value,'$.actor_id') FROM json_each(?1,'$.events')
      ) WHERE id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM users WHERE user_id=id)),
      NOT EXISTS(SELECT 1 FROM invitation_links WHERE id=json_extract(?1,'$.request.link_id')
        AND account_id != json_extract(?1,'$.request.account_id'))"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT json_extract(?1,'$.request.batch') IS NULL OR EXISTS(
      SELECT 1 FROM bulk_batch_requests r JOIN bulk_batches b ON b.id=r.batch_id
      WHERE r.batch_id=json_extract(?1,'$.request.batch.batch_id')
      AND r.row_number=json_extract(?1,'$.request.batch.row')
      AND r.request_id=json_extract(?1,'$.request.request_id')
      AND r.requester_id=json_extract(?1,'$.request.requester_id')
      AND b.account_id=json_extract(?1,'$.request.account_id')
      AND json_extract(b.snapshot,'$.submitted.submitted_by')=json_extract(?1,'$.request.decision.decided_by')
      AND json_extract(b.snapshot,'$.submitted.submitted_at')=json_extract(?1,'$.request.admitted_at')
      AND json_extract(b.snapshot,'$.submitted.submitted_at')=json_extract(?1,'$.request.decision.effective_at')
      AND json_extract(b.snapshot,'$.submitted.submitted_at')=json_extract(?1,'$.request.decision.evaluated_at')
      AND json_extract(?1,'$.request.decision.decision_id')='request.approved/'||r.request_id
      AND json_extract(?1,'$.request.justification') IS NULL)"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(
      SELECT 1 FROM invitation_links l WHERE l.id=json_extract(?1,'$.request.link_id') AND (
        (json_array_length(COALESCE(l.email_domains_json,'[]'))=0) IS NOT
          (json_extract(?1,'$.request.domain_evidence') IS NULL)
        OR EXISTS(SELECT 1 FROM json_each(?1,'$.request.domain_evidence.matched_domains') d
          WHERE NOT EXISTS(SELECT 1 FROM json_each(COALESCE(l.email_domains_json,'[]')) r
            WHERE d.value=json_extract(r.value,'$.exact') OR
              (length(d.value)>length(json_extract(r.value,'$.subdomains'))+1
               AND substr(d.value,-length(json_extract(r.value,'$.subdomains'))-1)='.'||json_extract(r.value,'$.subdomains'))))))"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT
      NOT EXISTS(SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.request.request_id') AND (
        invitation_link_id IS NOT json_extract(?1,'$.request.link_id') OR
        batch_id IS NOT json_extract(?1,'$.request.batch.batch_id') OR
        batch_row IS NOT json_extract(?1,'$.request.batch.row') OR
        requester_id IS NOT json_extract(?1,'$.request.requester_id') OR
        justification IS NOT json_extract(?1,'$.request.justification') OR
        domain_evidence_json IS NOT json_extract(?1,'$.request.domain_evidence') OR
        created_at IS NOT json_extract(?1,'$.request.admitted_at') OR
        decision_deadline IS NOT json_extract(?1,'$.request.decision_deadline') OR
        (projection_revision=json_extract(?1,'$.request.revision') AND (
          (projection_content IS NOT NULL AND projection_content IS NOT json_extract(?1,'$.content')) OR
          state IS NOT json_extract(?1,'$.request.state') OR
          decided_by IS NOT json_extract(?1,'$.request.decision.decided_by') OR
          decided_at IS NOT json_extract(?1,'$.request.decision.effective_at') OR
          decline_reason IS NOT json_extract(?1,'$.request.decision.decline_reason') OR
          requester_message IS NOT json_extract(?1,'$.request.decision.requester_message') OR
          cancellation_reason IS NOT json_extract(?1,'$.request.decision.cancellation_reason')))))
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
           a.metadata IS NOT json_extract(j.value,'$.metadata') OR a.request_id IS NOT NULL)"#,
    r#"INSERT INTO projection_assertions(invariant) SELECT NOT EXISTS(
      SELECT 1 FROM invitation_requests WHERE id=json_extract(?1,'$.request.request_id')
      AND state != 'pending' AND projection_revision <= json_extract(?1,'$.request.revision')
      AND ((state IS NOT json_extract(?1,'$.request.state') AND NOT (
          state='approved' AND json_extract(?1,'$.request.state')='lapsed'
          AND projection_revision < json_extract(?1,'$.request.revision')
          OR state='lapsed' AND json_extract(?1,'$.request.state')='approved' AND EXISTS(
            SELECT 1 FROM request_reissues p WHERE p.request_id=invitation_requests.id
            AND p.after_revision=json_extract(?1,'$.request.revision')
            AND p.before_revision>=invitation_requests.projection_revision
            AND p.after_content IS json_extract(?1,'$.content')
            AND (p.before_revision>invitation_requests.projection_revision OR p.before_content IS invitation_requests.projection_content))))
        OR decided_by IS NOT json_extract(?1,'$.request.decision.decided_by')
        OR decided_at IS NOT json_extract(?1,'$.request.decision.effective_at')
        OR decline_reason IS NOT json_extract(?1,'$.request.decision.decline_reason')
        OR requester_message IS NOT json_extract(?1,'$.request.decision.requester_message')
        OR cancellation_reason IS NOT json_extract(?1,'$.request.decision.cancellation_reason')
        OR (projection_content IS NOT NULL AND json_extract(projection_content,'$.decision') IS NOT json_extract(?1,'$.request.decision'))))"#,
    r#"INSERT INTO invitation_requests(id, invitation_link_id, batch_id, batch_row, source, requester_id, justification, state, created_at,
      decision_deadline, projection_revision, decided_by, decided_at, decline_reason, requester_message, cancellation_reason, domain_evidence_json, projection_content)
      SELECT json_extract(?1,'$.request.request_id'),json_extract(?1,'$.request.link_id'),
        json_extract(?1,'$.request.batch.batch_id'),json_extract(?1,'$.request.batch.row'),
        CASE WHEN json_extract(?1,'$.request.batch') IS NULL THEN 'link' ELSE 'batch' END,
        json_extract(?1,'$.request.requester_id'),json_extract(?1,'$.request.justification'),
        json_extract(?1,'$.request.state'),json_extract(?1,'$.request.admitted_at'),
        json_extract(?1,'$.request.decision_deadline'),json_extract(?1,'$.request.revision'),
        json_extract(?1,'$.request.decision.decided_by'),json_extract(?1,'$.request.decision.effective_at'),
        json_extract(?1,'$.request.decision.decline_reason'),json_extract(?1,'$.request.decision.requester_message'),json_extract(?1,'$.request.decision.cancellation_reason'),json_extract(?1,'$.request.domain_evidence'),json_extract(?1,'$.content') WHERE true
      ON CONFLICT(id) DO UPDATE SET state=excluded.state,projection_revision=excluded.projection_revision,
        decided_by=excluded.decided_by,decided_at=excluded.decided_at,decline_reason=excluded.decline_reason,requester_message=excluded.requester_message,
        cancellation_reason=excluded.cancellation_reason,projection_content=excluded.projection_content
      WHERE excluded.projection_revision > invitation_requests.projection_revision"#,
    r#"INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,actor_id,target_kind,target_id,
      metadata,projection_event_id,projection_content,evaluated_at)
      SELECT json_extract(value,'$.id'),json_extract(?1,'$.request.account_id'),json_extract(value,'$.effective_at'),
        json_extract(value,'$.kind'),CASE WHEN json_extract(value,'$.actor_id') IS NULL THEN 'system' ELSE 'user' END,
        json_extract(value,'$.actor_id'),'invitation_request',json_extract(value,'$.target_id'),json_extract(value,'$.metadata'),
        json_extract(value,'$.event_id'),json_extract(value,'$.content'),json_extract(value,'$.evaluated_at')
      FROM json_each(?1,'$.events') WHERE true ON CONFLICT(id) DO NOTHING"#,
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];
