//! The same fixed batch runs in SQLx and D1. One JSON parameter avoids lossy
//! JavaScript number bindings. All validation and writes share one transaction.
use super::*;
use crate::invitation_link::LinkMetadata;
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
        // The authority only writes values its own rules leave unchanged.
        || creation.clone().normalized().as_ref() != Ok(creation)
        || link.metadata.as_ref().is_some_and(|metadata| {
            LinkMetadata::parse(&metadata.description, metadata.internal_note.as_deref()).as_ref()
                != Ok(metadata)
        })
        || creation.repos.iter().any(|repo| !valid_id(repo.repo_id))
        || envelope.events.len() > 8
    {
        return Err(invalid());
    }
    let mut value = serde_json::to_value(envelope).map_err(|_| invalid())?;
    value["content"] = json!(link);
    value["link"]["current_description"] = json!(link.description());
    value["link"]["current_internal_note"] = json!(link.internal_note());
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
    // Bound worst-case JSON escaping of link metadata and repository scope.
    if encoded.len() > 2 * 1024 * 1024 {
        return Err(invalid());
    }
    Ok(encoded)
}

pub fn classify(message: String) -> Error {
    if message.contains("projection_dependency") {
        Error::ProjectionDependency
    } else if message.contains("projection_invariant") || crate::storage::unique_violation(&message)
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
        UNION SELECT json_extract(value,'$.actor_id') FROM json_each(?1,'$.events')
      ) WHERE id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM users WHERE user_id = id)),
      NOT EXISTS(SELECT 1 FROM installations WHERE installation_id = json_extract(?1,'$.link.creation.installation_id')
        AND account_id != json_extract(?1,'$.link.creation.account_id'))"#,
    r#"INSERT INTO projection_assertions(invariant)
    SELECT NOT EXISTS(SELECT 1 FROM invitation_links WHERE id = json_extract(?1,'$.link.link_id') AND (
      installation_id IS NOT json_extract(?1,'$.link.creation.installation_id') OR
      account_id IS NOT json_extract(?1,'$.link.creation.account_id') OR
      created_by IS NOT json_extract(?1,'$.link.creation.admin.user_id') OR
      created_at IS NOT json_extract(?1,'$.link.created_at') OR
      expires_at IS NOT json_extract(?1,'$.link.creation.expires_at') OR
      max_uses IS NOT json_extract(?1,'$.link.creation.max_uses') OR
      permission IS NOT json_extract(?1,'$.link.creation.permission') OR
      approval_required IS NOT json_extract(?1,'$.link.creation.approval_required') OR
      json_extract(projection_content,'$.creation') IS NOT json_extract(?1,'$.link.creation') OR
      (projection_revision = json_extract(?1,'$.link.revision') AND (
        projection_content IS NOT json_extract(?1,'$.content') OR
        uses_count IS NOT json_extract(?1,'$.link.uses') OR
        description IS NOT json_extract(?1,'$.link.current_description') OR
        internal_note IS NOT json_extract(?1,'$.link.current_internal_note') OR
        revoked_at IS NOT json_extract(?1,'$.link.revoked_at') OR
        revoked_by IS NOT json_extract(?1,'$.link.revoked_by')))))
    AND NOT EXISTS(SELECT 1 FROM invitation_link_repos r WHERE r.invitation_link_id = json_extract(?1,'$.link.link_id')
      AND NOT EXISTS(SELECT 1 FROM json_each(?1,'$.link.creation.repos') j
        WHERE r.repo_id = json_extract(j.value,'$.repo_id') AND r.repo_full_name = json_extract(j.value,'$.repo_full_name')))
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
    r#"INSERT INTO invitation_links(id, installation_id, account_id, created_by, created_at,
      expires_at, max_uses, uses_count, permission, approval_required, description, internal_note,
      revoked_at, revoked_by, projection_revision, projection_content)
    SELECT json_extract(?1,'$.link.link_id'),
      json_extract(?1,'$.link.creation.installation_id'), json_extract(?1,'$.link.creation.account_id'),
      json_extract(?1,'$.link.creation.admin.user_id'), json_extract(?1,'$.link.created_at'),
      json_extract(?1,'$.link.creation.expires_at'), json_extract(?1,'$.link.creation.max_uses'),
      json_extract(?1,'$.link.uses'), json_extract(?1,'$.link.creation.permission'),
      json_extract(?1,'$.link.creation.approval_required'), json_extract(?1,'$.link.current_description'),
      json_extract(?1,'$.link.current_internal_note'), json_extract(?1,'$.link.revoked_at'),
      json_extract(?1,'$.link.revoked_by'), json_extract(?1,'$.link.revision'), json_extract(?1,'$.content') WHERE true
    ON CONFLICT(id) DO UPDATE SET uses_count=excluded.uses_count, revoked_at=excluded.revoked_at,
      revoked_by=excluded.revoked_by, projection_revision=excluded.projection_revision,
      description=excluded.description, internal_note=excluded.internal_note,
      projection_content=excluded.projection_content
    WHERE excluded.projection_revision > invitation_links.projection_revision"#,
    // The retained creation content proves exact scope at every revision, so
    // replay can repair missing child rows without admitting an expanded scope.
    r#"INSERT INTO invitation_link_repos(invitation_link_id, repo_id, repo_full_name)
    SELECT json_extract(?1,'$.link.link_id'), json_extract(value,'$.repo_id'), json_extract(value,'$.repo_full_name')
    FROM json_each(?1,'$.link.creation.repos') WHERE true
    ON CONFLICT(invitation_link_id, repo_id) DO NOTHING"#,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> ProjectionEnvelope {
        serde_json::from_value(json!({
            "transition_id": "link/01ARZ3NDEKTSV4RRFFQ69G5FAV/1",
            "link": {
                "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "revision": 1, "uses": 0,
                "created_at": "2026-09-14T00:00:00Z",
                "revoked_at": null, "revoked_by": null,
                "creation": {
                    "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "admin": {"account_id": 100, "user_id": 7}, "account_id": 100,
                    "installation_id": 1, "description": "Workshop", "internal_note": "Cohort",
                    "expires_at": null, "max_uses": 1, "permission": "pull",
                    "approval_required": true,
                    "repos": [
                        {"repo_id": 10, "repo_full_name": "acme/api"},
                        {"repo_id": 11, "repo_full_name": "acme/web"}
                    ]
                }
            },
            "events": []
        }))
        .unwrap()
    }

    fn with_metadata(description: &str, internal_note: Option<&str>) -> ProjectionEnvelope {
        let mut input = envelope();
        input.link.metadata = Some(LinkMetadata {
            description: description.into(),
            internal_note: internal_note.map(Into::into),
        });
        input
    }

    #[test]
    fn values_the_authority_writes_are_encoded() {
        assert!(encode(&envelope()).is_ok());
        assert!(encode(&with_metadata("Renamed", None)).is_ok());
    }

    /// The gate applies the authority's own rule: a value it would normalise
    /// differently, or refuse, never came from it.
    #[test]
    fn values_the_authority_would_never_write_are_refused() {
        let mut refused = Vec::new();
        for note in [" ", " Cohort", "Cohort\n"] {
            let mut input = envelope();
            input.link.creation.internal_note = Some(note.into());
            refused.push((format!("creation note {note:?}"), input));
            refused.push((
                format!("current note {note:?}"),
                with_metadata("Workshop", Some(note)),
            ));
        }
        for description in [" Workshop", "Workshop "] {
            let mut input = envelope();
            input.link.creation.description = description.into();
            refused.push((format!("creation description {description:?}"), input));
            refused.push((
                format!("current description {description:?}"),
                with_metadata(description, None),
            ));
        }
        let mut input = envelope();
        input.link.creation.max_uses = Some(0);
        refused.push(("max uses 0".into(), input));
        let mut input = envelope();
        input.link.creation.repos.reverse();
        refused.push(("unordered repository scope".into(), input));
        for (case, input) in refused {
            assert!(
                matches!(encode(&input), Err(Error::ProjectionInvariant(_))),
                "{case}"
            );
        }
    }
}
