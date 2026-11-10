//! Repository-owned semantic transition cursor and bounded durable audit outbox.
use ghinvite_core::{
    delivery::{BlockedReason, CreateOutcome, DeliverySnapshot},
    delivery_audit::{
        DeliveryAuditFact, DeliveryAuditKind, DeliveryAuditStep, DeliveryBlockedReason,
        RepositoryDeliveryProjection,
    },
    delivery_control::Disposition,
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};

const STATE: &str = "repository_audit";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum SemanticState {
    Other,
    Blocked(BlockedReason),
    Unknown,
    Held { epoch: u64 },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cursor {
    revision: u64,
    state: SemanticState,
}

/// Cursor and outbox share one state write: killing an invocation cannot leave
/// a fact retained without the semantic identity that prevents re-dating it.
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Retained {
    #[serde(default)]
    snapshot: Option<DeliverySnapshot>,
    cursor: Option<Cursor>,
    pending: Vec<DeliveryAuditFact>,
}
async fn retained(ctx: &ObjectContext<'_>) -> Result<Retained, TerminalError> {
    Ok(ctx
        .get::<Json<Retained>>(STATE)
        .await?
        .map(|state| state.0)
        .unwrap_or_default())
}

fn invalid() -> TerminalError {
    TerminalError::new_with_code(409, "invalid retained repository audit")
}

pub(crate) async fn held_epoch_changed(
    ctx: &ObjectContext<'_>,
    epoch: u64,
) -> Result<bool, TerminalError> {
    Ok(retained(ctx)
        .await?
        .cursor
        .is_none_or(|old| old.state != SemanticState::Held { epoch }))
}

async fn semantic(
    ctx: &ObjectContext<'_>,
    snapshot: &DeliverySnapshot,
) -> Result<SemanticState, TerminalError> {
    Ok(match snapshot.create.outcome {
        CreateOutcome::Blocked { reason } => SemanticState::Blocked(reason),
        CreateOutcome::OutcomeUnknown => SemanticState::Unknown,
        CreateOutcome::Held { .. } => {
            let retained = ctx
                .get::<Json<crate::delivery_control::Retained>>(crate::delivery_control::STATE)
                .await?
                .ok_or_else(invalid)?;
            let Disposition::Held { epoch } = retained.0.state.disposition() else {
                return Err(invalid());
            };
            SemanticState::Held { epoch: *epoch }
        }
        _ => SemanticState::Other,
    })
}

/// Older snapshots remain readable until this owner first writes its complete
/// atomic record. New code never publishes an independently stored receipt.
pub(crate) async fn snapshot(
    ctx: &ObjectContext<'_>,
) -> Result<Option<DeliverySnapshot>, TerminalError> {
    let retained = retained(ctx).await?;
    if retained.snapshot.is_some() {
        return Ok(retained.snapshot);
    }
    Ok(ctx
        .get::<Json<DeliverySnapshot>>("snapshot")
        .await?
        .map(|v| v.0))
}

/// Shared handlers use a concrete context too; the SDK's context lifetime
/// cannot be abstracted by an elided ContextReadState bound in Send handlers.
pub(crate) async fn shared_snapshot(
    ctx: &SharedObjectContext<'_>,
) -> Result<Option<DeliverySnapshot>, TerminalError> {
    if let Some(Json(state)) = ctx.get::<Json<Retained>>(STATE).await?
        && state.snapshot.is_some()
    {
        return Ok(state.snapshot);
    }
    Ok(ctx
        .get::<Json<DeliverySnapshot>>("snapshot")
        .await?
        .map(|v| v.0))
}

pub(crate) async fn receipt(
    ctx: &ObjectContext<'_>,
) -> Result<Option<ghinvite_core::delivery::CreateReceipt>, TerminalError> {
    if let Some(snapshot) = snapshot(ctx).await? {
        return Ok(Some(snapshot.create));
    }
    Ok(ctx
        .get::<Json<ghinvite_core::delivery::CreateReceipt>>("receipt")
        .await?
        .map(|v| v.0))
}

pub(crate) async fn projection(
    ctx: &ObjectContext<'_>,
    snapshot: &DeliverySnapshot,
) -> Result<RepositoryDeliveryProjection, TerminalError> {
    crate::delivery::validate_notice_snapshot(ctx.key(), snapshot)?;
    let retained = retained(ctx).await?;
    if retained
        .pending
        .iter()
        .any(|fact| fact.transition_revision > snapshot.revision)
    {
        return Err(invalid());
    }
    Ok(RepositoryDeliveryProjection {
        snapshot: snapshot.clone(),
        audit_events: retained.pending,
    })
}

fn transition(
    previous: Option<SemanticState>,
    next: SemanticState,
    aborted: bool,
) -> Vec<DeliveryAuditKind> {
    if previous == Some(next) {
        return vec![];
    }
    let mut events = Vec::with_capacity(2);
    if let Some(SemanticState::Held { epoch }) = previous
        && !aborted
    {
        // A new hold epoch does not release the previous hold. Only returning
        // to runnable work releases it; revocation instead records Aborted.
        if !matches!(next, SemanticState::Held { .. }) {
            events.push(DeliveryAuditKind::Released { hold_epoch: epoch });
        }
    }
    let kind = match next {
        SemanticState::Blocked(reason) => Some(DeliveryAuditKind::Blocked {
            reason: DeliveryBlockedReason::Repository(reason),
        }),
        SemanticState::Unknown => Some(DeliveryAuditKind::OutcomeUnknown),
        SemanticState::Held { epoch } => Some(DeliveryAuditKind::Held { hold_epoch: epoch }),
        SemanticState::Other => None,
    };
    events.extend(kind);
    events
}

/// Snapshot, semantic cursor and pending facts are one authoritative state
/// write. A kill before it exposes none of the new transition; a kill after it
/// preserves the exact snapshot and original fact time for later recovery.
pub(crate) async fn save(
    ctx: &ObjectContext<'_>,
    snapshot: &DeliverySnapshot,
) -> Result<(), TerminalError> {
    crate::delivery::validate_notice_snapshot(ctx.key(), snapshot)?;
    let mut retained = retained(ctx).await?;
    let previous_snapshot = match retained.snapshot.as_ref() {
        Some(previous) => Some(previous.clone()),
        None => ctx
            .get::<Json<DeliverySnapshot>>("snapshot")
            .await?
            .map(|v| v.0),
    };
    if previous_snapshot.as_ref().is_some_and(|old| {
        old.create.command != snapshot.create.command
            || old.create.audit_version != snapshot.create.audit_version
            || old.revision > snapshot.revision
            || (old.revision == snapshot.revision && old != snapshot)
    }) {
        return Err(invalid());
    }
    // Drain one older bounded batch before changing the owner snapshot. SQL
    // completion stays with the independent projector, never this owner.
    if !retained.pending.is_empty() {
        let old = previous_snapshot.as_ref().ok_or_else(invalid)?;
        ctx.object_client::<crate::delivery::DeliveryProjectionClient>(ctx.key())
            .apply(Json(RepositoryDeliveryProjection {
                snapshot: old.clone(),
                audit_events: retained.pending.clone(),
            }))
            .send()
            .await?;
        retained.pending.clear();
        ctx.set(STATE, Json(retained.clone()));
    }
    let previous = retained.cursor.as_ref();
    let next = semantic(ctx, snapshot).await?;
    if previous.is_some_and(|old| {
        old.revision > snapshot.revision || (old.revision == snapshot.revision && old.state != next)
    }) {
        return Err(invalid());
    }
    let kinds = transition(
        previous.map(|old| old.state),
        next,
        matches!(snapshot.create.outcome, CreateOutcome::Aborted { .. }),
    );
    if !kinds.is_empty() {
        let at = crate::clock::now(ctx, "delivery_audit_time").await?;
        if at < snapshot.create.command.approved_at {
            return Err(TerminalError::new_with_code(
                503,
                "delivery audit clock precedes approval",
            ));
        }
        retained.pending = kinds
            .into_iter()
            .map(|kind| DeliveryAuditFact {
                step: DeliveryAuditStep::Repository,
                transition_revision: snapshot.revision,
                at,
                kind,
            })
            .collect();
    }
    retained.cursor = Some(Cursor {
        revision: snapshot.revision,
        state: next,
    });
    retained.snapshot = Some(snapshot.clone());
    ctx.set(STATE, Json(retained));
    Ok(())
}

/// Acknowledges the durable one-way enqueue, independent of projector SQL retries.
pub(crate) async fn acknowledge(ctx: &ObjectContext<'_>) -> Result<(), TerminalError> {
    let mut retained = retained(ctx).await?;
    if !retained.pending.is_empty() {
        retained.pending.clear();
        ctx.set(STATE, Json(retained));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_retries_do_not_duplicate_facts_but_reentry_does() {
        let blocked = SemanticState::Blocked(BlockedReason::RequesterIdentity);
        assert_eq!(transition(None, blocked, false).len(), 1);
        assert!(transition(Some(blocked), blocked, false).is_empty());
        assert!(transition(Some(blocked), SemanticState::Other, false).is_empty());
        assert_eq!(
            transition(Some(SemanticState::Other), blocked, false).len(),
            1
        );
        assert!(transition(Some(SemanticState::Unknown), SemanticState::Unknown, false).is_empty());
    }

    #[test]
    fn release_uses_original_epoch_and_abort_never_claims_release() {
        let held = SemanticState::Held { epoch: 4 };
        assert_eq!(
            transition(Some(held), SemanticState::Other, false),
            vec![DeliveryAuditKind::Released { hold_epoch: 4 }]
        );
        assert!(transition(Some(held), SemanticState::Other, true).is_empty());
        assert_eq!(
            transition(Some(held), SemanticState::Held { epoch: 7 }, false),
            vec![DeliveryAuditKind::Held { hold_epoch: 7 }]
        );
        assert_eq!(
            transition(Some(held), SemanticState::Unknown, false),
            vec![
                DeliveryAuditKind::Released { hold_epoch: 4 },
                DeliveryAuditKind::OutcomeUnknown
            ]
        );
    }
}
