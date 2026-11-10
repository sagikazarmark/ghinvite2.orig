//! Monotonic retained batch reviews and the exact issued request identity set.
use super::{Error, Result};
use crate::bulk::{BulkBatchId, Review};
use async_trait::async_trait;

#[async_trait]
pub trait BulkStorage: Send + Sync + 'static {
    async fn project_bulk_review(&self, review: &Review) -> Result<()> {
        self.project_bulk_projection(&Projection {
            review: review.clone(),
            events: vec![],
        })
        .await
    }
    async fn project_bulk_projection(&self, projection: &Projection) -> Result<()>;
    async fn bulk_review(&self, account_id: u64, batch_id: BulkBatchId) -> Result<Option<Review>>;
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct BulkAudit {
    pub operation_id: crate::request_lifecycle::LifecycleOperationId,
    pub actor: crate::invitation_link::AccountAdmin,
    pub at: chrono::DateTime<chrono::Utc>,
    pub revision: u64,
    #[schemars(with = "String")]
    pub kind: crate::audit::EventType,
    pub removed_rows: std::collections::BTreeSet<u16>,
}
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Projection {
    pub review: Review,
    pub events: Vec<BulkAudit>,
}
pub fn encode_projection(
    projection: &Projection,
    previous: Option<&Row>,
) -> Result<Option<String>> {
    use crate::audit::{ActorKind, AuditEvent, EventType, TargetKind};
    use sha2::{Digest, Sha256};
    let review = &projection.review;
    let encoded = encode(review, previous)?;
    if encoded.is_none() && projection.events.is_empty() {
        return Ok(None);
    }
    if projection.events.len() > 4 {
        return Err(invalid());
    }
    let mut events = vec![];
    for event in &projection.events {
        if event.actor.account_id != review.manifest.admin.account_id
            || event.actor.user_id == 0
            || event.at < review.reviewed_at
            || event.revision == 0
            || event.revision > review.revision
            || !event.removed_rows.is_subset(&review.removed)
        {
            return Err(invalid());
        }
        let mut metadata =
            serde_json::json!({"batch_id":review.manifest.batch_id,"revision":event.revision});
        match event.kind {
            EventType::BulkBatchCreated
                if event.actor == review.manifest.admin
                    && event.revision == 1
                    && event.removed_rows.is_empty() =>
            {
                metadata["rows"] = serde_json::json!(review.manifest.entries.len());
            }
            EventType::BulkBatchRowsRemoved if !event.removed_rows.is_empty() => {
                metadata["removed_rows"] = serde_json::json!(event.removed_rows);
            }
            EventType::BulkBatchSubmitted if event.removed_rows.is_empty() => {
                let submitted = review.submitted.as_ref().ok_or_else(invalid)?;
                if submitted.submitted_by != event.actor.user_id
                    || submitted.submitted_at != event.at
                {
                    return Err(invalid());
                }
                metadata["requests"] = serde_json::json!(submitted.requests.len());
                metadata["email_deliveries"] = serde_json::json!(submitted.emails.len());
                metadata["personal_links"] = serde_json::json!(submitted.personal_links.len());
            }
            EventType::BulkBatchCancelled
                if review.submitted.is_some() && event.removed_rows.is_empty() => {}
            _ => return Err(invalid()),
        }
        let digest = Sha256::digest(format!(
            "ghinvite/bulk/audit/v1/{}/{}/{}",
            review.manifest.batch_id,
            String::from(event.operation_id.clone()),
            event.kind.as_str()
        ));
        events.push(AuditEvent {
            id: crate::AuditEventId::from_ulid(ulid::Ulid::from_bytes(
                digest[..16].try_into().unwrap(),
            )),
            account_id: event.actor.account_id,
            occurred_at: event.at,
            event_type: event.kind,
            actor_kind: ActorKind::User,
            actor_id: Some(event.actor.user_id),
            target_kind: TargetKind::BulkBatch,
            target_id: review.manifest.batch_id.to_string(),
            metadata,
            request_id: None,
        });
    }
    let base = encoded.unwrap_or_else(|| serde_json::json!({"review":review,"primary_app":if review.manifest.targets.repos.is_empty(){"org"}else{"repo"},"expected":previous.map(|r|r.snapshot.as_str())}).to_string());
    let mut value: serde_json::Value = serde_json::from_str(&base).map_err(|_| invalid())?;
    value["events"] = serde_json::to_value(events).map_err(|_| invalid())?;
    super::integer::validate(&value)?;
    Ok(Some(value.to_string()))
}
pub fn statements() -> Vec<String> {
    let mut statements: Vec<String> = STATEMENTS.iter().map(|s| (*s).to_owned()).collect();
    statements.insert(statements.len() - 1, super::audit_write::retained_events());
    statements
}
pub fn invalid() -> Error {
    Error::ProjectionInvariant("invalid bulk review projection".into())
}
pub fn unavailable() -> Error {
    Error::Database("bulk review storage unavailable".into())
}
pub fn classify(message: String) -> Error {
    if message.contains("audit_events.account_id") || super::unique_violation(&message) {
        invalid()
    } else {
        super::projection::sql::classify(message)
    }
}

#[derive(serde::Deserialize)]
pub struct Row {
    pub id: String,
    pub snapshot: String,
}
impl Row {
    pub fn decode(&self) -> Result<Review> {
        let review: Review = serde_json::from_str(&self.snapshot).map_err(|_| invalid())?;
        review.validate().map_err(|_| invalid())?;
        super::integer::validate(&review)?;
        if review.manifest.batch_id.to_string() != self.id {
            return Err(invalid());
        }
        Ok(review)
    }
}
pub fn encode(review: &Review, previous: Option<&Row>) -> Result<Option<String>> {
    review.validate().map_err(|_| invalid())?;
    super::integer::validate(review)?;
    if let Some(row) = previous {
        let old = row.decode()?;
        if old.manifest != review.manifest
            || old.reviewed_at != review.reviewed_at
            || old.rows != review.rows
            || matches!((&old.submitted, &review.submitted), (Some(a), Some(b)) if a != b)
        {
            return Err(invalid());
        }
        if old.revision == review.revision {
            return if old == *review {
                Ok(None)
            } else {
                Err(invalid())
            };
        }
        if old.revision > review.revision {
            return Ok(None);
        }
        if !old.removed.is_subset(&review.removed)
            || (old.submitted.is_some()
                && (old.submitted != review.submitted
                    || old.removed != review.removed
                    || old.personal_link_rows != review.personal_link_rows))
        {
            return Err(invalid());
        }
    }
    let primary_app = if review.manifest.targets.repos.is_empty() {
        "org"
    } else {
        "repo"
    };
    Ok(Some(serde_json::json!({"review":review,"primary_app":primary_app,"expected":previous.map(|r|r.snapshot.as_str())}).to_string()))
}
pub const GET: &str = "SELECT id,snapshot FROM bulk_batches WHERE id=?1";
pub const SCOPED_GET: &str = "SELECT id,snapshot FROM bulk_batches WHERE id=?1 AND account_id=?2";

/// A batch delivery must be derived from an issued row and the complete retained
/// target grant. The request projection alone cannot authorize a different write.
pub(crate) fn delivery_identity(command: &str, organization: bool) -> String {
    let target = if organization {
        format!(
            r#"
AND json_extract(?1,'{command}.installation_id')=COALESCE(json_extract(b.manifest,'$.organization_installation_id'),json_extract(b.manifest,'$.installation_id'))
AND json_extract(?1,'{command}.organization')=json_extract(b.manifest,'$.targets.organization')
AND json_extract(?1,'{command}.teams')=json_extract(b.manifest,'$.targets.teams')"#
        )
    } else {
        format!(
            r#"
AND json_extract(?1,'{command}.installation_id')=json_extract(b.manifest,'$.installation_id')
AND EXISTS(SELECT 1 FROM json_each(b.manifest,'$.targets.repos') t
 WHERE json_extract(t.value,'$.repo_id')=json_extract(?1,'{command}.repo_id')
 AND json_extract(t.value,'$.repo_full_name')=json_extract(?1,'{command}.repo_full_name')
 AND CASE json_extract(t.value,'$.grant') WHEN 'read' THEN 'pull' WHEN 'write' THEN 'push' ELSE json_extract(t.value,'$.grant') END=json_extract(?1,'{command}.permission'))"#
        )
    };
    format!(
        r#"EXISTS(SELECT 1 FROM invitation_requests r
JOIN bulk_batch_requests br ON br.request_id=r.id AND br.batch_id=r.batch_id AND br.row_number=r.batch_row AND br.requester_id=r.requester_id
JOIN bulk_batches b ON b.id=br.batch_id
WHERE r.id=json_extract(?1,'{command}.request_id') AND r.source='batch' AND r.invitation_link_id IS NULL
AND json_extract(?1,'{command}.link_id') IS NULL
AND br.batch_id=json_extract(?1,'{command}.batch.batch_id') AND br.row_number=json_extract(?1,'{command}.batch.row')
AND r.requester_id=json_extract(?1,'{command}.requester_id') AND b.account_id=json_extract(?1,'{command}.account_id')
AND json_extract(?1,'{command}.approval_id')='request.approved/'||r.id
AND json_extract(?1,'{command}.approved_at')=json_extract(b.snapshot,'$.submitted.submitted_at')
AND json_extract(?1,'{command}.access_lifetime_seconds') IS json_extract(b.manifest,'$.access_lifetime_seconds')
{target})"#
    )
}
pub const STATEMENTS: &[&str] = &[
    r#"INSERT INTO projection_assertions(dependency) SELECT json_extract(?1,'$.review.submitted') IS NULL OR EXISTS(SELECT 1 FROM users WHERE user_id=json_extract(?1,'$.review.submitted.submitted_by'))"#,
    r#"INSERT INTO projection_assertions(dependency) SELECT EXISTS(SELECT 1 FROM users WHERE user_id=json_extract(?1,'$.review.manifest.admin.user_id')) AND EXISTS(SELECT 1 FROM installations WHERE app=json_extract(?1,'$.primary_app') AND installation_id=json_extract(?1,'$.review.manifest.installation_id') AND account_id=json_extract(?1,'$.review.manifest.admin.account_id')) AND (json_extract(?1,'$.review.manifest.organization_installation_id') IS NULL OR EXISTS(SELECT 1 FROM installations WHERE app='org' AND installation_id=json_extract(?1,'$.review.manifest.organization_installation_id') AND account_id=json_extract(?1,'$.review.manifest.admin.account_id')))"#,
    r#"INSERT INTO projection_assertions(dependency) SELECT NOT EXISTS(SELECT 1 FROM bulk_batches WHERE id=json_extract(?1,'$.review.manifest.batch_id') AND snapshot IS NOT json_extract(?1,'$.expected'))"#,
    r#"INSERT INTO bulk_batches(id,account_id,created_by,revision,manifest,snapshot) VALUES(json_extract(?1,'$.review.manifest.batch_id'),json_extract(?1,'$.review.manifest.admin.account_id'),json_extract(?1,'$.review.manifest.admin.user_id'),json_extract(?1,'$.review.revision'),json_extract(?1,'$.review.manifest'),json_extract(?1,'$.review')) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,snapshot=excluded.snapshot WHERE bulk_batches.revision<excluded.revision"#,
    r#"INSERT INTO bulk_batch_requests(batch_id,row_number,request_id,requester_id) SELECT json_extract(?1,'$.review.manifest.batch_id'),json_extract(value,'$.source.row'),json_extract(value,'$.request_id'),json_extract(value,'$.requester_id') FROM json_each(?1,'$.review.submitted.requests') WHERE true ON CONFLICT(batch_id,row_number) DO NOTHING"#,
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];
