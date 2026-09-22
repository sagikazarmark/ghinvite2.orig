//! One atomic receipt/history projection for SQLx and D1. Audit insertion is
//! independent of receipt freshness and of the current invitation lifecycle.
use crate::{
    audit::{ActorKind, EventType},
    delivery::{CreateOutcome, CreateReceipt},
};
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn encode(receipt: &CreateReceipt) -> super::Result<String> {
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
    Ok(format!("{{\"receipt\":{receipt},\"event\":{event}}}"))
}

pub const RECEIPT: &str = r#"INSERT INTO delivery_outcomes(invitation_id, request_id, receipt)
    VALUES (json_extract(?1,'$.receipt.command.invitation_id'), json_extract(?1,'$.receipt.command.request_id'), json_extract(?1,'$.receipt'))
    ON CONFLICT(invitation_id) DO UPDATE SET receipt = excluded.receipt
    WHERE json_extract(excluded.receipt,'$.revision') >= json_extract(delivery_outcomes.receipt,'$.revision')"#;

pub const IDENTITY: &str = r#"INSERT INTO projection_assertions(invariant)
    SELECT NOT EXISTS(SELECT 1 FROM delivery_outcomes WHERE invitation_id = json_extract(?1,'$.receipt.command.invitation_id')
    AND (json_extract(receipt,'$.command') != json_extract(?1,'$.receipt.command')
    OR (json_extract(receipt,'$.revision') = json_extract(?1,'$.receipt.revision') AND receipt != json_extract(?1,'$.receipt'))))"#;

pub fn statements() -> Vec<String> {
    vec![
        IDENTITY.into(),
        RECEIPT.into(),
        audit_statement(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}

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
    pub fn decode(self) -> super::Result<CreateReceipt> {
        serde_json::from_str(&self.receipt).map_err(|e| super::Error::Corrupt(e.to_string()))
    }
}
