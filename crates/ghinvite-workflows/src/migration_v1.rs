//! Private maintenance-only import protocol. Never bootstraps from SQL on reads.
use crate::admission_v1::{Admit, RequestSnapshot};
use crate::admission_v1::{InvitationCodeV1Client, LinkSnapshot};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HandoffImport {
    pub migration_id: String,
    pub manifest_checksum: String,
    pub index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct VerifyImport {
    pub link: LinkSnapshot,
    pub request: Option<RequestSnapshot>,
    pub blocker: Option<ghinvite_core::RequestId>,
    pub plan: Option<crate::request_lifecycle_v1::ApprovedDispatch>,
    pub operation: Option<serde_json::Value>,
}

pub async fn verify(
    ctx: &ObjectContext<'_>,
    input: HandoffImport,
) -> Result<VerifyImport, TerminalError> {
    let Json(source) = ctx
        .get::<Json<BeginImport>>("migration/v1/source")
        .await?
        .ok_or_else(conflict)?;
    if source.migration_id != input.migration_id
        || source.manifest_checksum != input.manifest_checksum
    {
        return Err(conflict());
    }
    let Json(link) = ctx
        .get::<Json<LinkSnapshot>>("v1/link")
        .await?
        .ok_or_else(conflict)?;
    let record = ctx
        .get::<Json<ImportedRequest>>(&format!("migration/v1/item/{}", input.index))
        .await?;
    let (request, blocker, plan, operation) = if let Some(Json(record)) = record {
        let r = record.request;
        let request = ctx
            .get::<Json<RequestSnapshot>>(&format!("v1/request/{}", r.request_id))
            .await?
            .map(|v| v.0);
        let blocker = ctx
            .get::<Json<ghinvite_core::RequestId>>(&format!("v1/blocker/{}", r.requester_id))
            .await?
            .map(|v| v.0);
        let plan = ctx
            .get::<Json<crate::request_lifecycle_v1::ApprovedDispatch>>(&format!(
                "v1/dispatch/{}",
                r.request_id
            ))
            .await?
            .map(|v| v.0);
        let operation = ctx
            .get::<Json<serde_json::Value>>(&format!("v1/op/{}", r.request_id))
            .await?
            .map(|v| v.0);
        (request, blocker, plan, operation)
    } else {
        (None, None, None, None)
    };
    Ok(VerifyImport {
        link,
        request,
        blocker,
        plan,
        operation,
    })
}

pub async fn handoff(ctx: &ObjectContext<'_>, input: HandoffImport) -> Result<(), TerminalError> {
    use restate_sdk::context::InvocationHandle;
    ensure_open(ctx).await?;
    let Json(source) = ctx
        .get::<Json<BeginImport>>("migration/v1/source")
        .await?
        .ok_or_else(conflict)?;
    if source.migration_id != input.migration_id
        || source.manifest_checksum != input.manifest_checksum
    {
        return Err(conflict());
    }
    let key = format!("migration/v1/handed-off/{}", input.index);
    if ctx.get::<bool>(&key).await? == Some(true) {
        return Ok(());
    }
    let Json(record) = ctx
        .get::<Json<ImportedRequest>>(&format!("migration/v1/item/{}", input.index))
        .await?
        .ok_or_else(conflict)?;
    if record.request.state == ghinvite_core::RequestState::Pending
        || record.request.state == ghinvite_core::RequestState::Approved
            && record.receipts.iter().any(|r| !r.outcome.confirmed())
    {
        ctx.workflow_client::<crate::admission_v1::InvitationRequestV1Client>(
            record.request.request_id.to_string(),
        )
        .run(Json(crate::admission_v1::WorkflowEnvelope::from_authority(
            &source.link,
            record.request,
        )))
        .send()
        .invocation_id()
        .await?;
    }
    ctx.set(&key, true);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportedRequest {
    pub request: RequestSnapshot,
    /// Version 0 is the evidence-bound legacy accepted-attempt namespace.
    pub legacy_input: Admit,
    pub plan: Option<crate::request_lifecycle_v1::ApprovedDispatch>,
    pub receipts: Vec<ghinvite_core::delivery::CreateReceipt>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImportRequest {
    pub migration_id: String,
    pub manifest_checksum: String,
    pub index: usize,
    pub record: serde_json::Value,
}

pub async fn import_request(
    ctx: &ObjectContext<'_>,
    input: ImportRequest,
) -> Result<ImportStatus, TerminalError> {
    use sha2::{Digest, Sha256};
    let Json(source) = ctx
        .get::<Json<BeginImport>>("migration/v1/source")
        .await?
        .ok_or_else(conflict)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&input.record).map_err(|_| conflict())?)
    );
    if source.migration_id != input.migration_id
        || source.manifest_checksum != input.manifest_checksum
        || source.request_checksums.get(input.index) != Some(&digest)
    {
        return Err(conflict());
    }
    let record: ImportedRequest = serde_json::from_value(input.record).map_err(|_| conflict())?;
    let key = format!("migration/v1/item/{}", input.index);
    if let Some(Json(old)) = ctx.get::<Json<ImportedRequest>>(&key).await? {
        if old != record {
            return Err(conflict());
        }
        return status(ctx).await;
    }
    let count = ctx.get::<u64>("migration/v1/count").await?.unwrap_or(0);
    if count != input.index as u64 || ctx.get::<bool>("migration/v1/active").await? == Some(true) {
        return Err(conflict());
    }
    let request = &record.request;
    let mut normalized = record.legacy_input.clone();
    normalized.normalize();
    if normalized != record.legacy_input
        || normalized.version != 0
        || normalized.link_id != source.link.link_id
        || normalized.requester_id != request.requester_id
        || normalized.justification != request.justification
        || String::from(normalized.operation_id.clone()) != request.request_id.to_string()
        || (request.state == ghinvite_core::RequestState::Pending && request.decision.is_some())
        || (request.state == ghinvite_core::RequestState::Approved && request.decision.is_none())
    {
        return Err(conflict());
    }
    ghinvite_core::storage::projection::sql::encode(
        &ghinvite_core::storage::projection::ProjectionEnvelope {
            version: 1,
            transition_id: "migration-validation".into(),
            link: source.link.clone(),
            requests: vec![request.clone()],
            events: vec![],
        },
    )
    .map_err(|_| conflict())?;
    let request_key = format!("v1/request/{}", request.request_id);
    let op_key = format!("v1/op/{}", String::from(normalized.operation_id.clone()));
    if ctx
        .get::<Json<RequestSnapshot>>(&request_key)
        .await?
        .is_some()
        || ctx.get::<Json<serde_json::Value>>(&op_key).await?.is_some()
    {
        return Err(conflict());
    }
    let blocking = matches!(
        request.state,
        ghinvite_core::RequestState::Pending | ghinvite_core::RequestState::Approved
    );
    let blocker_key = format!("v1/blocker/{}", request.requester_id);
    if blocking
        && ctx
            .get::<Json<ghinvite_core::RequestId>>(&blocker_key)
            .await?
            .is_some()
    {
        return Err(conflict());
    }
    if request.state == ghinvite_core::RequestState::Approved {
        let plan = record.plan.as_ref().ok_or_else(conflict)?;
        crate::admission_v1::validate_import_plan(&source.link, request, plan)?;
        if record.receipts.len() != plan.commands.len()
            || record
                .receipts
                .iter()
                .zip(&plan.commands)
                .any(|(r, c)| r.command != *c || r.revision == 0)
        {
            return Err(conflict());
        }
        // Receiving state is imported before this link can authorize dispatch.
        for receipt in &record.receipts {
            ctx.object_client::<crate::delivery_v1::GithubCreateV1Client>(
                receipt.command.invitation_id.to_string(),
            )
            .import_receipt(Json(crate::delivery_v1::ImportReceipt {
                migration_id: source.migration_id.clone(),
                manifest_checksum: source.manifest_checksum.clone(),
                receipt: receipt.clone(),
            }))
            .call()
            .await?;
        }
        ctx.set(
            &format!("v1/dispatch/{}", request.request_id),
            Json(plan.clone()),
        );
    } else if record.plan.is_some() || !record.receipts.is_empty() {
        return Err(conflict());
    }
    ctx.set(&request_key, Json(request.clone()));
    if blocking {
        ctx.set(&blocker_key, Json(request.request_id));
    }
    ctx.set(&op_key, Json(serde_json::json!({"input": normalized, "receipt": {
        "decided_at": request.admitted_at,
        "result": {"kind":"accepted", "request_id":request.request_id,
            "state": if source.link.creation.approval_required { "pending" } else { "approved" },
            "decision_deadline":request.decision_deadline}
    }})));
    ctx.set(&key, Json(record));
    ctx.set("migration/v1/count", count + 1);
    Ok(ImportStatus {
        phase: "importing".into(),
        imported_requests: count as usize + 1,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BeginImport {
    pub version: u32,
    pub migration_id: String,
    pub manifest_checksum: String,
    /// Identity of the coordinated, fenced source checkpoint (operator assertion).
    pub checkpoint: String,
    pub link: LinkSnapshot,
    pub request_checksums: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ImportStatus {
    pub phase: String,
    pub imported_requests: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ActivateImport {
    pub migration_id: String,
    pub manifest_checksum: String,
    /// Assert only after the operator verifies the identity-checked SQL adoption.
    pub projection_verified: bool,
}

fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "migration identity/state conflict")
}

pub async fn ensure_open(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    if ctx.get::<bool>("migration/v1/present").await? == Some(true)
        && ctx.get::<bool>("migration/v1/active").await? != Some(true)
    {
        return Err(TerminalError::new_with_code(
            503,
            "link migration incomplete",
        ));
    }
    Ok(())
}

pub async fn status(ctx: &ObjectContext<'_>) -> Result<ImportStatus, TerminalError> {
    Ok(ImportStatus {
        phase: if ctx.get::<bool>("migration/v1/active").await? == Some(true) {
            "active"
        } else {
            "importing"
        }
        .into(),
        imported_requests: ctx.get::<u64>("migration/v1/count").await?.unwrap_or(0) as usize,
    })
}

pub async fn begin(
    ctx: &ObjectContext<'_>,
    input: BeginImport,
) -> Result<ImportStatus, TerminalError> {
    if ctx.key() != input.link.link_id.to_string()
        || input.version != 1
        || input.migration_id.is_empty()
        || input.checkpoint.is_empty()
        || input.manifest_checksum.len() != 64
        || !input
            .manifest_checksum
            .bytes()
            .all(|c| c.is_ascii_hexdigit())
        || input.link.uses != input.request_checksums.len() as u64
    {
        return Err(conflict());
    }
    ghinvite_core::storage::projection::sql::encode(
        &ghinvite_core::storage::projection::ProjectionEnvelope {
            version: 1,
            transition_id: "migration-validation".into(),
            link: input.link.clone(),
            requests: vec![],
            events: vec![],
        },
    )
    .map_err(|_| conflict())?;
    if let Some(Json(old)) = ctx.get::<Json<BeginImport>>("migration/v1/source").await? {
        if old != input {
            return Err(conflict());
        }
        return status(ctx).await;
    }
    if !ctx.get_keys().await?.is_empty() {
        return Err(conflict());
    }
    ctx.set("migration/v1/present", true);
    ctx.set("migration/v1/source", Json(input.clone()));
    ctx.set("v1/link", Json(input.link.clone()));
    // Historical creation replay is the checkpoint snapshot, explicitly scoped
    // to this migration; there was no v1 creation command before cutover.
    ctx.set("v1/creation", Json(input.link));
    Ok(ImportStatus {
        phase: "importing".into(),
        imported_requests: 0,
    })
}

pub async fn activate(
    ctx: &ObjectContext<'_>,
    input: ActivateImport,
) -> Result<ImportStatus, TerminalError> {
    let Json(source) = ctx
        .get::<Json<BeginImport>>("migration/v1/source")
        .await?
        .ok_or_else(conflict)?;
    if source.migration_id != input.migration_id
        || source.manifest_checksum != input.manifest_checksum
        || !input.projection_verified
        || ctx.get::<u64>("migration/v1/count").await?.unwrap_or(0)
            != source.request_checksums.len() as u64
    {
        return Err(conflict());
    }
    if ctx.get::<bool>("migration/v1/active").await? == Some(true) {
        return status(ctx).await;
    }
    ctx.object_client::<InvitationCodeV1Client>(&source.link.invitation_code)
        .register(Json(source.link.link_id))
        .call()
        .await?;
    ctx.set("migration/v1/active", true);
    status(ctx).await
}
