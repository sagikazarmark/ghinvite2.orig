//! Independent email-addressed receipts and a full-command non-idempotent fence.
use super::{Error, Result};
use crate::{
    bulk::BulkBatchId,
    email_delivery::{EmailAttempt, EmailDeliverySnapshot},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
mod audit;
/// Retained transition facts contain no address, address digest, or vault reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmailAuditKind {
    Blocked {
        reason: crate::email_delivery::EmailBlockedReason,
    },
    OutcomeUnknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailAuditFact {
    pub transition_revision: u64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: EmailAuditKind,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EmailDeliveryProjection {
    #[serde(flatten)]
    pub snapshot: EmailDeliverySnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_events: Vec<EmailAuditFact>,
}
impl EmailDeliveryProjection {
    pub fn validate(&self) -> Result<()> {
        self.snapshot.validate().map_err(|_| invalid())?;
        super::integer::validate(self)?;
        if self.audit_events.len() > 64
            || self.audit_events.iter().any(|fact| {
                fact.transition_revision == 0
                    || fact.transition_revision > self.snapshot.revision
                    || fact.at < self.snapshot.command.approved_at
                    || (matches!(fact.kind, EmailAuditKind::OutcomeUnknown)
                        && self.snapshot.unknown_since != Some(fact.at))
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[async_trait]
pub trait EmailDeliveryStorage: Send + Sync + 'static {
    async fn claim_email_attempt(&self, attempt: &EmailAttempt) -> Result<Option<u64>>;
    async fn email_attempt_exists(&self, attempt: &EmailAttempt) -> Result<bool>;
    /// Only positive rejection before a possible write may release this exact generation.
    async fn reject_email_attempt(&self, attempt: &EmailAttempt, generation: u64) -> Result<()>;
    async fn project_email_delivery(&self, snapshot: &EmailDeliverySnapshot) -> Result<()> {
        self.project_email_delivery_with_events(&EmailDeliveryProjection {
            snapshot: snapshot.clone(),
            audit_events: vec![],
        })
        .await
    }
    async fn project_email_delivery_with_events(
        &self,
        projection: &EmailDeliveryProjection,
    ) -> Result<()>;
    async fn email_delivery(&self, id: &str) -> Result<Option<EmailDeliverySnapshot>>;
    async fn batch_email_deliveries(
        &self,
        account_id: u64,
        batch_id: BulkBatchId,
    ) -> Result<Vec<EmailDeliverySnapshot>>;
}
pub fn invalid() -> Error {
    Error::ProjectionInvariant("email delivery binding conflict".into())
}
pub fn unavailable() -> Error {
    Error::Database("email delivery storage unavailable".into())
}
pub fn encode_attempt(attempt: &EmailAttempt) -> Result<String> {
    attempt.create.validate().map_err(|_| invalid())?;
    super::integer::validate(attempt)?;
    serde_json::to_string(&attempt.create).map_err(|_| invalid())
}
#[derive(Deserialize)]
pub struct AttemptRow {
    pub command: String,
    pub retryable: i64,
}
pub fn check_attempt(row: Option<AttemptRow>, command: &str) -> Result<Option<bool>> {
    row.map(|r| {
        if r.command == command {
            Ok(r.retryable == 0)
        } else {
            Err(invalid())
        }
    })
    .transpose()
}
/// A row can never be rebound to another organization or mailbox reference.
/// A staged/expired vault row is not authority to issue an invitation.
pub const CLAIM: &str = r#"INSERT INTO email_delivery_attempts(id,batch_id,row_number,command)
SELECT ?1,json_extract(?2,'$.batch.batch_id'),json_extract(?2,'$.batch.row'),?2
WHERE EXISTS(SELECT 1 FROM email_vault v WHERE v.email_ref=json_extract(?2,'$.email_ref') AND v.batch_id=json_extract(?2,'$.batch.batch_id') AND v.row_number=json_extract(?2,'$.batch.row') AND v.account_id=json_extract(?2,'$.account_id') AND v.installation_id=json_extract(?2,'$.installation_id') AND v.activated_at IS NOT NULL AND v.retired_at IS NULL AND v.ciphertext IS NOT NULL)
ON CONFLICT(id) DO UPDATE SET generation=generation+1,retryable=0 WHERE retryable=1 AND command=excluded.command AND generation<9007199254740991 RETURNING generation"#;
pub const ATTEMPT: &str = "SELECT command,retryable FROM email_delivery_attempts WHERE id=?1 OR (batch_id=json_extract(?2,'$.batch.batch_id') AND row_number=json_extract(?2,'$.batch.row'))";
pub const REJECT: &str =
    "UPDATE email_delivery_attempts SET retryable=1 WHERE id=?1 AND command=?2 AND generation=?3";
#[derive(Deserialize)]
pub struct SnapshotRow {
    pub id: String,
    pub receipt: String,
}
impl SnapshotRow {
    pub fn decode(&self) -> Result<EmailDeliverySnapshot> {
        let s: EmailDeliverySnapshot =
            serde_json::from_str(&self.receipt).map_err(|_| invalid())?;
        super::integer::validate(&s)?;
        s.validate().map_err(|_| invalid())?;
        if s.command.storage_id() != self.id {
            return Err(invalid());
        }
        Ok(s)
    }
}
pub const GET: &str = "SELECT id,receipt FROM email_deliveries WHERE id=?1";
pub const LIST: &str = "SELECT id,receipt FROM email_deliveries WHERE account_id=?1 AND batch_id=?2 ORDER BY row_number LIMIT 201";
pub fn encode_projection(
    snapshot: &EmailDeliverySnapshot,
    previous: Option<&SnapshotRow>,
) -> Result<Option<String>> {
    encode_projection_with_events(
        &EmailDeliveryProjection {
            snapshot: snapshot.clone(),
            audit_events: vec![],
        },
        previous,
    )
}
pub fn encode_projection_with_events(
    projection: &EmailDeliveryProjection,
    previous: Option<&SnapshotRow>,
) -> Result<Option<String>> {
    projection.validate()?;
    let snapshot = &projection.snapshot;
    let retained = previous.map(SnapshotRow::decode).transpose()?;
    let advances = retained
        .as_ref()
        .map(|old| snapshot.advances(old).map_err(|_| invalid()))
        .transpose()?
        .unwrap_or(true);
    if !advances && projection.audit_events.is_empty() {
        return Ok(None);
    }
    // A delayed history append must not replace a newer authoritative receipt.
    let effective = if advances {
        snapshot
    } else {
        retained.as_ref().expect("retained predecessor")
    };
    let mut events = audit::events(snapshot);
    events.extend(audit::transitions(snapshot, &projection.audit_events));
    Ok(Some(serde_json::json!({"id":snapshot.command.storage_id(),"expected":previous.map(|r|r.receipt.as_str()),"snapshot":effective,"events":events}).to_string()))
}

pub const PROJECT: &str = r#"INSERT INTO email_deliveries(id,batch_id,row_number,account_id,receipt)
VALUES(json_extract(?1,'$.id'),json_extract(?1,'$.snapshot.command.batch.batch_id'),json_extract(?1,'$.snapshot.command.batch.row'),json_extract(?1,'$.snapshot.command.account_id'),json_extract(?1,'$.snapshot'))
ON CONFLICT(id) DO UPDATE SET receipt=excluded.receipt WHERE receipt=json_extract(?1,'$.expected') AND json_extract(excluded.receipt,'$.revision')>json_extract(email_deliveries.receipt,'$.revision') RETURNING id"#;

/// A compare-and-swap guard and audit appends share one transaction with the
/// receipt. A concurrent projection cannot publish an event for rejected state.
pub fn statements() -> Vec<String> {
    vec![
        "INSERT INTO projection_assertions(unchanged) SELECT (SELECT receipt FROM email_deliveries WHERE id=json_extract(?1,'$.id')) IS json_extract(?1,'$.expected')".into(),
        PROJECT.trim_end_matches(" RETURNING id").into(),
        super::audit_write::retained_events(),
        "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL".into(),
    ]
}

pub fn classify(message: String) -> Error {
    if message.contains("projection_changed") {
        Error::Database("email projection changed; retry".into())
    } else {
        super::delivery_projection::classify(message)
    }
}
