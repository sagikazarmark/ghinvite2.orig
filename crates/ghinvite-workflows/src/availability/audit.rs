//! One retained installation snapshot and bounded durable projection outbox.
use super::*;
const STATE: &str = "installation_audit";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SourceWatermark {
    pub installation_id: u64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub sender_id: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retained {
    status: InstallationStatus,
    pending: Option<InstallationChange>,
    source: Option<SourceWatermark>,
    #[serde(default)]
    delegation: Option<AccountSettings>,
    #[serde(default)]
    pending_receipt: Option<(String, serde_json::Value)>,
}
async fn retained(ctx: &ObjectContext<'_>) -> Result<Option<Retained>, TerminalError> {
    Ok(ctx.get::<Json<Retained>>(STATE).await?.map(|v| v.0))
}
pub(super) async fn status(
    ctx: &ObjectContext<'_>,
) -> Result<Option<Json<InstallationStatus>>, TerminalError> {
    if let Some(value) = retained(ctx).await? {
        return Ok(Some(Json(value.status)));
    }
    ctx.get::<Json<InstallationStatus>>("installation").await
}
pub(super) async fn shared_status(
    ctx: &SharedObjectContext<'_>,
) -> Result<Option<Json<InstallationStatus>>, TerminalError> {
    if let Some(Json(value)) = ctx.get::<Json<Retained>>(STATE).await? {
        return Ok(Some(Json(value.status)));
    }
    ctx.get::<Json<InstallationStatus>>("installation").await
}
async fn value(ctx: &ObjectContext<'_>) -> Result<Retained, TerminalError> {
    match retained(ctx).await? {
        Some(value) => Ok(value),
        None => Ok(Retained {
            status: status(ctx)
                .await?
                .map(|v| v.0)
                .unwrap_or_else(InstallationStatus::empty),
            pending: None,
            source: None,
            delegation: None,
            pending_receipt: None,
        }),
    }
}
pub(super) async fn delegation(
    ctx: &ObjectContext<'_>,
) -> Result<Option<AccountSettings>, TerminalError> {
    if let Some(value) = value(ctx).await?.delegation {
        return Ok(Some(value));
    }
    Ok(ctx
        .get::<Json<AccountSettings>>("delegation")
        .await?
        .map(|v| v.0))
}
pub(super) async fn source(
    ctx: &ObjectContext<'_>,
    id: u64,
) -> Result<Option<SourceWatermark>, TerminalError> {
    let value = value(ctx).await?;
    if let Some(source) = value.source.filter(|s| s.installation_id == id) {
        return Ok(Some(source));
    }
    let key = format!("owner_updated_at/{id}");
    Ok(ctx
        .get::<Json<chrono::DateTime<chrono::Utc>>>(&key)
        .await?
        .map(|at| SourceWatermark {
            installation_id: id,
            updated_at: at.0,
            sender_id: value
                .status
                .installation
                .as_ref()
                .and_then(|v| v.installed_by_user_id)
                .unwrap_or(0),
        }))
}
pub(super) async fn flush(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    let mut value = value(ctx).await?;
    if let Some(input) = value.pending.clone() {
        ctx.object_client::<InstallationProjectionClient>(ctx.key())
            .apply(Json(input))
            .send()
            .await?;
        if let Some((key, receipt)) = value.pending_receipt.take() {
            ctx.set(&key, Json(receipt));
        }
        value.pending = None;
        ctx.set(STATE, Json(value));
    }
    Ok(())
}
pub(super) async fn save_status(
    ctx: &ObjectContext<'_>,
    status: InstallationStatus,
) -> Result<(), TerminalError> {
    let mut value = value(ctx).await?;
    value.status = status;
    ctx.set(STATE, Json(value));
    Ok(())
}
pub(super) async fn save_source(
    ctx: &ObjectContext<'_>,
    status: InstallationStatus,
    source: SourceWatermark,
) -> Result<(), TerminalError> {
    flush(ctx).await?;
    let mut value = value(ctx).await?;
    value.status = status;
    value.source = Some(source);
    ctx.set(STATE, Json(value));
    Ok(())
}
pub(super) async fn commit(
    ctx: &ObjectContext<'_>,
    _state: &AppState,
    status: InstallationStatus,
    input: InstallationChange,
    source: Option<SourceWatermark>,
    receipt: Option<(String, serde_json::Value)>,
) -> Result<(), TerminalError> {
    input
        .events(ctx.key().parse().map_err(|_| invalid())?)
        .map_err(|_| invalid())?;
    flush(ctx).await?;
    let mut value = value(ctx).await?;
    value.status = status;
    if let Some(settings) = &input.settings {
        value.delegation = Some(settings.clone());
    }
    value.pending = Some(input);
    value.pending_receipt = receipt;
    if let Some(source) = source {
        value.source = Some(source);
    }
    ctx.set(STATE, Json(value));
    #[cfg(feature = "integration")]
    if let Some(fault) = &_state.installation_audit_fault {
        let fault = fault.clone();
        ctx.run(move || {
            let fault = fault.clone();
            async move {
                if fault.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    fault
                        .paused
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::future::pending::<()>().await;
                }
                Ok::<_, HandlerError>(())
            }
        })
        .name("installation_audit_after_atomic_save")
        .await?;
    }
    flush(ctx).await
}
