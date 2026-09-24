//! One atomic receipt/history projection for SQLx and D1. Audit insertion is
//! independent of receipt freshness and of the current invitation lifecycle.
use crate::{
    audit::{ActorKind, EventType},
    delivery::{CreateOutcome, DeliverySnapshot},
};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn encode(snapshot: &DeliverySnapshot) -> super::Result<String> {
    let receipt = &snapshot.create;
    if snapshot.revision < receipt.revision {
        return Err(super::Error::ProjectionInvariant(
            "invalid delivery revision".into(),
        ));
    }
    if let Some(settlement) = &snapshot.settlement
        && (!matches!(receipt.outcome, CreateOutcome::Created { .. })
            || snapshot.revision <= receipt.revision
            || settlement.event.event_type != super::settlement::event_type(settlement.state)?
            || settlement.event.account_id != receipt.command.account_id
            || settlement.event.target_id != receipt.command.invitation_id.to_string()
            || settlement.event.target_kind != crate::audit::TargetKind::GithubInvitation)
    {
        return Err(super::Error::ProjectionInvariant(
            "invalid retained settlement".into(),
        ));
    }
    if receipt.revision == 0 || receipt.outcome.confirmed() != receipt.confirmed_at.is_some() {
        return Err(super::Error::ProjectionInvariant(
            "invalid delivery receipt".into(),
        ));
    }
    let command = &receipt.command;
    let event = if let Some(at) = receipt.confirmed_at {
        let mut metadata =
            json!({"repo_full_name":command.repo_full_name,"requester_id":command.requester_id});
        let (kind, actor) = match receipt.outcome {
            CreateOutcome::Created { upstream_id } => {
                metadata["github_invitation_id"] = json!(upstream_id);
                (EventType::InvitationSent, ActorKind::System)
            }
            CreateOutcome::AlreadyCollaborator => {
                metadata["reason"] = json!("already_collaborator");
                (EventType::InvitationAccepted, ActorKind::Github)
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
        let digest = Sha256::digest(format!("ghinvite/delivery/audit/{}", command.invitation_id));
        let bytes: [u8; 16] = digest[..16].try_into().unwrap();
        json!({"id":ulid::Ulid::from_bytes(bytes).to_string(),"account_id":command.account_id,
            "occurred_at":at,"event_type":kind,"actor_kind":actor,"target_id":command.invitation_id,
            "metadata":metadata})
    } else {
        serde_json::Value::Null
    };
    // Always use the typed receipt encoding (struct field order): a replayed
    // receipt must match the stored text, because SQLite's immutable JSON
    // comparison is textual.
    let receipt =
        serde_json::to_string(receipt).map_err(|e| super::Error::Corrupt(e.to_string()))?;
    let snapshot =
        serde_json::to_string(snapshot).map_err(|e| super::Error::Corrupt(e.to_string()))?;
    Ok(format!(
        "{{\"receipt\":{receipt},\"snapshot\":{snapshot},\"event\":{event}}}"
    ))
}

pub const RECEIPT: &str = r#"INSERT INTO delivery_outcomes(invitation_id, request_id, receipt)
    VALUES (json_extract(?1,'$.receipt.command.invitation_id'), json_extract(?1,'$.receipt.command.request_id'), json_extract(?1,'$.snapshot'))
    ON CONFLICT(invitation_id) DO UPDATE SET receipt = excluded.receipt
    WHERE json_extract(excluded.receipt,'$.revision') >= json_extract(delivery_outcomes.receipt,'$.revision')"#;

pub const IDENTITY: &str = r#"INSERT INTO projection_assertions(invariant)
    SELECT NOT EXISTS(SELECT 1 FROM delivery_outcomes WHERE invitation_id = json_extract(?1,'$.receipt.command.invitation_id')
    AND (json_extract(receipt,'$.create.command') != json_extract(?1,'$.receipt.command')
    OR (json_extract(receipt,'$.revision') = json_extract(?1,'$.snapshot.revision') AND receipt != json_extract(?1,'$.snapshot'))))"#;

const PARENTS: &str = r#"INSERT INTO projection_assertions(dependency)
    SELECT EXISTS(SELECT 1 FROM invitation_requests WHERE id = json_extract(?1,'$.receipt.command.request_id'))"#;

const INVITATION_IDENTITY: &str = r#"INSERT INTO projection_assertions(invariant)
    SELECT NOT EXISTS(SELECT 1 FROM github_invitations WHERE id = json_extract(?1,'$.receipt.command.invitation_id')
        AND (invitation_request_id != json_extract(?1,'$.receipt.command.request_id')
            OR repo_id != json_extract(?1,'$.receipt.command.repo_id')))
    AND EXISTS(SELECT 1 FROM invitation_requests r JOIN invitation_links l ON l.id = r.invitation_link_id
        WHERE r.id = json_extract(?1,'$.receipt.command.request_id')
        AND r.invitation_link_id = json_extract(?1,'$.receipt.command.link_id')
        AND r.requester_id = json_extract(?1,'$.receipt.command.requester_id')
        AND l.account_id = json_extract(?1,'$.receipt.command.account_id'))"#;

const INITIAL: &str = r#"INSERT INTO github_invitations(id, invitation_request_id, repo_id, state, created_at, updated_at)
    VALUES(json_extract(?1,'$.receipt.command.invitation_id'), json_extract(?1,'$.receipt.command.request_id'),
        json_extract(?1,'$.receipt.command.repo_id'), 'sending', json_extract(?1,'$.receipt.command.approved_at'),
        json_extract(?1,'$.receipt.command.approved_at')) ON CONFLICT(id) DO NOTHING"#;

/// Both adapters classify the named atomic assertions identically.
pub fn classify(message: String) -> super::Error {
    if message.contains("projection_dependency") {
        super::Error::ProjectionDependency
    } else if message.contains("projection_invariant")
        || message.contains("audit_events.account_id")
    {
        super::Error::ProjectionInvariant(message)
    } else {
        super::Error::Database(message)
    }
}

pub fn statements() -> Vec<String> {
    vec![
        PARENTS.into(),
        INVITATION_IDENTITY.into(),
        IDENTITY.into(),
        INITIAL.into(),
        RECEIPT.into(),
        audit_statement(),
        settlement_audit_statement(),
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

const SETTLED: &str = r#"UPDATE github_invitations SET
    state = json_extract(?1,'$.snapshot.settlement.state'),
    updated_at = json_extract(?1,'$.snapshot.settlement.event.occurred_at'), error_message = NULL
    WHERE id = json_extract(?1,'$.receipt.command.invitation_id')
    AND json_type(?1,'$.snapshot.settlement') = 'object'
    AND EXISTS(SELECT 1 FROM delivery_outcomes WHERE invitation_id = github_invitations.id
      AND json_extract(receipt,'$.revision') = json_extract(?1,'$.snapshot.revision'))"#;

pub fn audit_statement() -> String {
    format!(
        r#"INSERT INTO audit_events(id, account_id, occurred_at, event_type, actor_kind, actor_id, target_kind, target_id, metadata, request_id)
        SELECT json_extract(?1,'$.event.id'), json_extract(?1,'$.event.account_id'), json_extract(?1,'$.event.occurred_at'),
        json_extract(?1,'$.event.event_type'), json_extract(?1,'$.event.actor_kind'), NULL, 'github_invitation',
        json_extract(?1,'$.event.target_id'), json_extract(?1,'$.event.metadata'), NULL
        WHERE json_type(?1,'$.event') != 'null' {}"#,
        super::audit_write::INSTALLATION_REPLAY
    )
}

/// Projected receipts of one request, in invitation order.
pub const FOR_REQUEST: &str =
    "SELECT receipt FROM delivery_outcomes WHERE request_id = ?1 ORDER BY invitation_id";

#[derive(Debug, serde::Deserialize)]
pub struct ReceiptRow {
    pub receipt: String,
}

impl ReceiptRow {
    pub fn decode(self) -> super::Result<DeliverySnapshot> {
        serde_json::from_str(&self.receipt).map_err(|e| super::Error::Corrupt(e.to_string()))
    }
}
