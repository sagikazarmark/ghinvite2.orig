//! Owner-retained monitoring facts, separate from create and settlement receipts.
use crate::settlement_policy::{Evidence, Observation};
use chrono::{DateTime, Utc};
use ghinvite_core::{
    delivery::{CreateOutcome, CreateReceipt, DeliverySnapshot},
    storage::operational::RepositoryTelemetry,
};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};

/// Only a newly observed transition establishes the age of uncertainty. Existing
/// unknown owners without this instrumentation remain explicitly undated.
pub(crate) async fn record_unknown(
    ctx: &ObjectContext<'_>,
    previous: Option<&CreateReceipt>,
    receipt: &CreateReceipt,
) -> Result<(), TerminalError> {
    if matches!(receipt.outcome, CreateOutcome::OutcomeUnknown)
        && !previous.is_some_and(|p| matches!(p.outcome, CreateOutcome::OutcomeUnknown))
        && ctx
            .get::<Json<DateTime<Utc>>>("unknown_since")
            .await?
            .is_none()
    {
        let at = crate::clock::now(ctx, "outcome_unknown_since").await?;
        ctx.set("unknown_since", Json(at));
    }
    Ok(())
}

/// A healthy observation can leave lifecycle unchanged. Failed/incomplete reads,
/// another invitation, and stale owner revisions must not refresh its watermark.
fn current_observation(snapshot: &DeliverySnapshot, observation: &Observation) -> bool {
    if snapshot.tracking_stopped.is_some()
        || snapshot.settlement.is_some()
        || observation.delivery_key != snapshot.create.command.delivery_key()
        || observation.expected_revision != snapshot.revision
        || observation.at
            < snapshot
                .create
                .confirmed_at
                .unwrap_or(snapshot.create.command.approved_at)
    {
        return false;
    }
    match (&snapshot.create.outcome, &observation.evidence) {
        (CreateOutcome::OutcomeUnknown, Evidence::FoundByInvitee { upstream_id, .. }) => {
            *upstream_id > 0
        }
        (
            CreateOutcome::OutcomeUnknown,
            Evidence::Absent {
                availability_verified: true,
                ..
            },
        ) => true,
        (
            CreateOutcome::Created { upstream_id }
            | CreateOutcome::AlreadyPending { upstream_id, .. },
            Evidence::Listed {
                upstream_id: found, ..
            }
            | Evidence::FailedInvitation {
                upstream_id: found, ..
            },
        ) => *upstream_id > 0 && upstream_id == found,
        (
            CreateOutcome::Created { upstream_id }
            | CreateOutcome::AlreadyPending { upstream_id, .. },
            Evidence::Absent {
                availability_verified: true,
                role_name: Some(_),
                ..
            },
        ) => *upstream_id > 0,
        _ => false,
    }
}

pub(crate) enum ObservationProgress {
    Rejected,
    Current,
    Advanced,
}

pub(crate) async fn record_observation(
    ctx: &ObjectContext<'_>,
    snapshot: &DeliverySnapshot,
    observation: &Observation,
) -> Result<ObservationProgress, TerminalError> {
    if !current_observation(snapshot, observation) {
        return Ok(ObservationProgress::Rejected);
    }
    let previous = ctx.get::<Json<DateTime<Utc>>>("last_observed_at").await?;
    if let Some(Json(previous)) = previous {
        if observation.at < previous {
            return Ok(ObservationProgress::Rejected);
        }
        if observation.at == previous {
            return Ok(ObservationProgress::Current);
        }
    }
    ctx.set("last_observed_at", Json(observation.at));
    Ok(ObservationProgress::Advanced)
}

/// Sent after the snapshot to the same exclusive projection object. Its FIFO
/// queue preserves the parent dependency without making the owner wait for SQL.
pub(crate) async fn send_projection(
    ctx: &ObjectContext<'_>,
    snapshot: &DeliverySnapshot,
) -> Result<(), TerminalError> {
    let unknown_since = ctx
        .get::<Json<DateTime<Utc>>>("unknown_since")
        .await?
        .map(|v| v.0);
    let last_observed_at = ctx
        .get::<Json<DateTime<Utc>>>("last_observed_at")
        .await?
        .map(|v| v.0);
    if unknown_since.is_none() && last_observed_at.is_none() {
        return Ok(());
    }
    let command = &snapshot.create.command;
    let telemetry = RepositoryTelemetry {
        request_id: command.request_id,
        target_id: command.repo_id,
        delivery_generation: command.delivery_generation,
        account_id: command.account_id,
        unknown_since,
        last_observed_at,
    };
    ctx.object_client::<crate::delivery::DeliveryProjectionClient>(command.delivery_key())
        .telemetry(Json(telemetry))
        .send()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settlement_policy::tests::snapshot;
    use ghinvite_core::{delivery::SettlementReason, targets::EffectiveRole};

    #[test]
    fn only_complete_current_evidence_refreshes_observation_age() {
        let mut snapshot = snapshot(CreateOutcome::Created { upstream_id: 99 });
        let at = snapshot.create.command.approved_at;
        let evidence = Evidence::Listed {
            upstream_id: 99,
            expired: false,
            created_at: None,
        };
        let mut observation = Observation::new(&snapshot, at, evidence);
        assert!(current_observation(&snapshot, &observation));
        observation.expected_revision += 1;
        assert!(!current_observation(&snapshot, &observation));
        observation.expected_revision = snapshot.revision;
        observation.delivery_key.push('x');
        assert!(!current_observation(&snapshot, &observation));
        observation.delivery_key = snapshot.create.command.delivery_key();
        observation.at = at - chrono::Duration::nanoseconds(1);
        assert!(!current_observation(&snapshot, &observation));
        observation.at = at;
        observation.evidence = Evidence::Listed {
            upstream_id: 100,
            expired: false,
            created_at: None,
        };
        assert!(!current_observation(&snapshot, &observation));
        for (available, role, expected) in [
            (false, Some(EffectiveRole::Write), false),
            (true, None, false),
            (true, Some(EffectiveRole::None), true),
        ] {
            observation.evidence = Evidence::Absent {
                availability_verified: available,
                role_name: role,
                direct_collaborator: None,
                membership: None,
            };
            assert_eq!(current_observation(&snapshot, &observation), expected);
        }
        snapshot.create.confirmed_at = Some(at + chrono::Duration::nanoseconds(1));
        assert!(
            !current_observation(&snapshot, &observation),
            "evidence predating confirmation cannot refresh or settle the delivery"
        );
        snapshot.create.confirmed_at = None;
        snapshot.tracking_stopped = Some(SettlementReason::LinkRevoked);
        assert!(!current_observation(&snapshot, &observation));
    }

    #[test]
    fn unknown_absence_dates_observation_but_does_not_resolve_creation() {
        let snapshot = snapshot(CreateOutcome::OutcomeUnknown);
        let observation = Observation::new(
            &snapshot,
            snapshot.create.command.approved_at,
            Evidence::Absent {
                availability_verified: true,
                role_name: None,
                direct_collaborator: None,
                membership: None,
            },
        );
        assert!(current_observation(&snapshot, &observation));
        assert_eq!(
            crate::settlement_policy::evaluate(&snapshot, &observation, Default::default()),
            crate::settlement_policy::ReconcileDecision::NoChange
        );
    }
}
