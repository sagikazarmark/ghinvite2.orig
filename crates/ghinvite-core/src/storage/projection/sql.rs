//! The same fixed batch runs in SQLx and D1. One JSON parameter avoids lossy
//! JavaScript number bindings. All validation and writes share one transaction.
use super::*;
use crate::invitation_link::{DESCRIPTION_MAX_CHARS, INTERNAL_NOTE_MAX_BYTES};
use crate::storage::{Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};

fn invalid() -> Error {
    Error::ProjectionInvariant("invalid envelope".into())
}

pub fn encode(envelope: &ProjectionEnvelope) -> Result<String> {
    let link = &envelope.link;
    let creation = &link.creation;
    let valid_id = |id: u64| id > 0 && id <= i64::MAX as u64;
    if envelope.transition_id.is_empty()
        || envelope.transition_id.len() > 256
        || link.link_id != creation.link_id
        || creation.admin.account_id != creation.account_id
        || !valid_id(creation.account_id)
        || !valid_id(creation.installation_id)
        || !valid_id(creation.admin.user_id)
        || !valid_id(link.revision)
        || link.uses > u32::MAX as u64
        || link.revoked_by.is_some_and(|id| !valid_id(id))
        || link.revoked_at.is_some() != link.revoked_by.is_some()
        || crate::Slug::from_string(link.invitation_code.clone()).is_err()
        || creation.description.chars().count() > DESCRIPTION_MAX_CHARS
        || crate::Description::parse(link.description()).is_err()
        || link
            .internal_note()
            .is_some_and(|s| s.len() > INTERNAL_NOTE_MAX_BYTES)
        || creation
            .internal_note
            .as_ref()
            .is_some_and(|s| s.len() > INTERNAL_NOTE_MAX_BYTES)
        || crate::RepositoryScope::parse(creation.repos.clone()).is_err()
        || creation.repos.iter().any(|repo| !valid_id(repo.repo_id))
        || envelope.requests.len() > 2
        || envelope.events.len() > 8
    {
        return Err(invalid());
    }
    let mut value = serde_json::to_value(envelope).map_err(|_| invalid())?;
    value["link"]["current_description"] = json!(link.description());
    value["link"]["current_internal_note"] = json!(link.internal_note());
    let mut requests = std::collections::BTreeSet::new();
    for request in &envelope.requests {
        if request.link_id != link.link_id
            || request.account_id != creation.account_id
            || !valid_id(request.requester_id)
            || !valid_id(request.revision)
            || !requests.insert(request.request_id.to_string())
            || request
                .justification
                .as_ref()
                .is_some_and(|s| s.len() > crate::admission::MAX_JUSTIFICATION_BYTES)
            || (request.state == RequestState::Pending && request.decision_deadline.is_none())
            || request.decision.as_ref().is_some_and(|d| {
                d.decided_by.is_some_and(|id| !valid_id(id))
                    || d.decline_reason.as_ref().is_some_and(|s| s.len() > 16_384)
            })
        {
            return Err(invalid());
        }
    }
    let mut events = std::collections::BTreeMap::new();
    for (event, encoded) in envelope
        .events
        .iter()
        .zip(value["events"].as_array_mut().unwrap())
    {
        if events
            .insert(&event.event_id, event)
            .is_some_and(|previous| previous != event)
        {
            return Err(invalid());
        }
        let target_kind = match event.kind {
            EventType::InvitationLinkCreated
            | EventType::InvitationLinkMetadataUpdated
            | EventType::InvitationLinkRevoked
            | EventType::InvitationLinkExpired
            | EventType::InvitationLinkExhausted
                if event.target_id == link.link_id.to_string() =>
            {
                "invitation_link"
            }
            EventType::RequestCreated
            | EventType::RequestApproved
            | EventType::RequestDeclined
            | EventType::RequestExpired
                if envelope
                    .requests
                    .iter()
                    .any(|r| r.request_id.to_string() == event.target_id) =>
            {
                "invitation_request"
            }
            _ => return Err(invalid()),
        };
        if event.event_id.is_empty()
            || event.event_id.len() > 256
            || event.actor_id.is_some_and(|id| !valid_id(id))
        {
            return Err(invalid());
        }
        // Preserve the existing ULID audit-read/cursor contract. The logical ID
        // is retained separately and content checked; 128-bit hash collisions
        // fail the immutable-content assertion rather than deduplicating silently.
        let digest = Sha256::digest(format!("ghinvite/projection/audit/{}", event.event_id));
        let bytes: [u8; 16] = digest[..16].try_into().unwrap();
        encoded["id"] = json!(ulid::Ulid::from_bytes(bytes).to_string());
        encoded["target_kind"] = json!(target_kind);
        encoded["content"] = json!([creation.account_id, event]);
    }
    let encoded = serde_json::to_string(&value).map_err(|_| invalid())?;
    // Includes up to three copies of bounded input and worst-case JSON escaping
    // (six bytes per control character), including two touched requests.
    if encoded.len() > 2 * 1024 * 1024 {
        return Err(invalid());
    }
    Ok(encoded)
}

pub fn classify(message: String) -> Error {
    if message.contains("projection_dependency") {
        Error::ProjectionDependency
    } else if message.contains("projection_invariant")
        || message.contains("UNIQUE constraint failed")
    {
        Error::ProjectionInvariant("immutable identity conflict".into())
    } else {
        Error::Database(message)
    }
}

pub const STATEMENTS: &[&str] = &[
    // Missing owned parents are retryable; account mismatches are invariants.
    r#"INSERT INTO projection_assertions(dependency, invariant)
    SELECT
      EXISTS(SELECT 1 FROM installations WHERE installation_id = json_extract(?1,'$.link.creation.installation_id'))
      AND NOT EXISTS(SELECT 1 FROM (
        SELECT json_extract(?1,'$.link.creation.admin.user_id') AS id
        UNION SELECT json_extract(?1,'$.link.revoked_by')
        UNION SELECT json_extract(value,'$.requester_id') FROM json_each(?1,'$.requests')
        UNION SELECT json_extract(value,'$.decision.decided_by') FROM json_each(?1,'$.requests')
        UNION SELECT json_extract(value,'$.actor_id') FROM json_each(?1,'$.events')
      ) WHERE id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM users WHERE user_id = id)),
      NOT EXISTS(SELECT 1 FROM installations WHERE installation_id = json_extract(?1,'$.link.creation.installation_id')
        AND account_id != json_extract(?1,'$.link.creation.account_id'))"#,
    r#"INSERT INTO projection_assertions(invariant)
    SELECT NOT EXISTS(SELECT 1 FROM invitation_links WHERE id = json_extract(?1,'$.link.link_id') AND (
      slug IS NOT json_extract(?1,'$.link.invitation_code') OR
      installation_id IS NOT json_extract(?1,'$.link.creation.installation_id') OR
      account_id IS NOT json_extract(?1,'$.link.creation.account_id') OR
      created_by IS NOT json_extract(?1,'$.link.creation.admin.user_id') OR
      created_at IS NOT json_extract(?1,'$.link.created_at') OR
      expires_at IS NOT json_extract(?1,'$.link.creation.expires_at') OR
      max_uses IS NOT json_extract(?1,'$.link.creation.max_uses') OR
      permission IS NOT json_extract(?1,'$.link.creation.permission') OR
      approval_required IS NOT json_extract(?1,'$.link.creation.approval_required')))
    AND NOT EXISTS(SELECT 1 FROM invitation_link_repos r WHERE r.invitation_link_id = json_extract(?1,'$.link.link_id')
      AND NOT EXISTS(SELECT 1 FROM json_each(?1,'$.link.creation.repos') j
        WHERE r.repo_id = json_extract(j.value,'$.repo_id') AND r.repo_full_name = json_extract(j.value,'$.repo_full_name')))
    AND NOT EXISTS(SELECT 1 FROM invitation_requests r JOIN json_each(?1,'$.requests') j ON r.id = json_extract(j.value,'$.request_id') WHERE
      r.invitation_link_id IS NOT json_extract(j.value,'$.link_id') OR
      r.requester_id IS NOT json_extract(j.value,'$.requester_id') OR
      r.justification IS NOT json_extract(j.value,'$.justification') OR
      r.created_at IS NOT json_extract(j.value,'$.admitted_at') OR
      r.decision_deadline IS NOT json_extract(j.value,'$.decision_deadline'))
    AND NOT EXISTS(SELECT 1 FROM audit_events a JOIN json_each(?1,'$.events') j
      ON a.id = json_extract(j.value,'$.id') OR a.projection_event_id = json_extract(j.value,'$.event_id')
      WHERE a.projection_content IS NOT json_extract(j.value,'$.content') OR
        a.account_id IS NOT json_extract(?1,'$.link.creation.account_id') OR
        a.projection_event_id IS NOT json_extract(j.value,'$.event_id') OR
        a.id IS NOT json_extract(j.value,'$.id') OR
        a.event_type IS NOT json_extract(j.value,'$.kind') OR
        a.actor_id IS NOT json_extract(j.value,'$.actor_id') OR
        a.actor_kind IS NOT CASE WHEN json_extract(j.value,'$.actor_id') IS NULL THEN 'system' ELSE 'user' END OR
        a.target_kind IS NOT json_extract(j.value,'$.target_kind') OR
        a.target_id IS NOT json_extract(j.value,'$.target_id') OR
        a.occurred_at IS NOT json_extract(j.value,'$.effective_at') OR
        a.evaluated_at IS NOT json_extract(j.value,'$.evaluated_at') OR
        a.metadata IS NOT NULL OR a.request_id IS NOT NULL)"#,
    r#"INSERT INTO invitation_links(id, slug, installation_id, account_id, created_by, created_at,
      expires_at, max_uses, uses_count, permission, approval_required, description, internal_note,
      revoked_at, revoked_by, projection_revision)
    SELECT json_extract(?1,'$.link.link_id'), json_extract(?1,'$.link.invitation_code'),
      json_extract(?1,'$.link.creation.installation_id'), json_extract(?1,'$.link.creation.account_id'),
      json_extract(?1,'$.link.creation.admin.user_id'), json_extract(?1,'$.link.created_at'),
      json_extract(?1,'$.link.creation.expires_at'), json_extract(?1,'$.link.creation.max_uses'),
      json_extract(?1,'$.link.uses'), json_extract(?1,'$.link.creation.permission'),
      json_extract(?1,'$.link.creation.approval_required'), json_extract(?1,'$.link.current_description'),
      json_extract(?1,'$.link.current_internal_note'), json_extract(?1,'$.link.revoked_at'),
      json_extract(?1,'$.link.revoked_by'), json_extract(?1,'$.link.revision') WHERE true
    ON CONFLICT(id) DO UPDATE SET uses_count=excluded.uses_count, revoked_at=excluded.revoked_at,
      revoked_by=excluded.revoked_by, projection_revision=excluded.projection_revision,
      description=excluded.description, internal_note=excluded.internal_note
    WHERE excluded.projection_revision > invitation_links.projection_revision"#,
    r#"INSERT INTO invitation_link_repos(invitation_link_id, repo_id, repo_full_name)
    SELECT json_extract(?1,'$.link.link_id'), json_extract(value,'$.repo_id'), json_extract(value,'$.repo_full_name')
    FROM json_each(?1,'$.link.creation.repos') WHERE true
    ON CONFLICT(invitation_link_id, repo_id) DO NOTHING"#,
    r#"INSERT INTO invitation_requests(id, invitation_link_id, requester_id, justification, state, created_at,
      decision_deadline, projection_revision, decided_by, decided_at, decline_reason)
    SELECT json_extract(value,'$.request_id'), json_extract(value,'$.link_id'), json_extract(value,'$.requester_id'),
      json_extract(value,'$.justification'), json_extract(value,'$.state'), json_extract(value,'$.admitted_at'),
      json_extract(value,'$.decision_deadline'), json_extract(value,'$.revision'),
      json_extract(value,'$.decision.decided_by'), json_extract(value,'$.decision.effective_at'),
      json_extract(value,'$.decision.decline_reason') FROM json_each(?1,'$.requests') WHERE true
    ON CONFLICT(id) DO UPDATE SET state=excluded.state, projection_revision=excluded.projection_revision,
      decided_by=excluded.decided_by,
      decided_at=excluded.decided_at, decline_reason=excluded.decline_reason
    WHERE excluded.projection_revision > invitation_requests.projection_revision"#,
    r#"INSERT INTO audit_events(id, account_id, occurred_at, event_type, actor_kind, actor_id,
      target_kind, target_id, metadata, projection_event_id, projection_content, evaluated_at)
    SELECT json_extract(value,'$.id'), json_extract(?1,'$.link.creation.account_id'),
      json_extract(value,'$.effective_at'), json_extract(value,'$.kind'),
      CASE WHEN json_extract(value,'$.actor_id') IS NULL THEN 'system' ELSE 'user' END,
      json_extract(value,'$.actor_id'), json_extract(value,'$.target_kind'), json_extract(value,'$.target_id'),
      NULL, json_extract(value,'$.event_id'), json_extract(value,'$.content'), json_extract(value,'$.evaluated_at')
    FROM json_each(?1,'$.events') WHERE true ON CONFLICT(id) DO NOTHING"#,
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];
