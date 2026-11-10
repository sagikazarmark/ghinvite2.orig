//! Per-step semantic transition cursors and a bounded durable projection outbox.
use super::*;
use ghinvite_core::delivery_audit::{
    DeliveryAuditFact, DeliveryAuditKind, DeliveryAuditStep, DeliveryBlockedReason,
    MAX_AUDIT_FACTS, OrganizationDeliveryProjection,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Semantic {
    Other,
    Blocked(OrganizationBlockedReason),
    Unknown,
    Held { epoch: u64 },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cursor {
    revision: u64,
    steps: BTreeMap<String, Semantic>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct AuditState {
    cursor: Cursor,
    pub(super) pending: Vec<DeliveryAuditFact>,
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(409, "invalid retained organization audit")
}
fn held(value: &Retained, step: &str) -> Result<Semantic, TerminalError> {
    match value
        .steps
        .get(step)
        .and_then(|s| s.control.as_ref())
        .map(|s| s.disposition())
    {
        Some(model::Disposition::Held { epoch }) => Ok(Semantic::Held { epoch: *epoch }),
        _ => Err(invalid()),
    }
}
fn semantics(value: &Retained) -> Result<BTreeMap<String, Semantic>, TerminalError> {
    let org = match value.snapshot.create.outcome {
        OrganizationCreateOutcome::Blocked { reason } => Semantic::Blocked(reason),
        OrganizationCreateOutcome::OutcomeUnknown => Semantic::Unknown,
        OrganizationCreateOutcome::Held { .. } => held(value, "org")?,
        _ => Semantic::Other,
    };
    let mut states = BTreeMap::from([("org".into(), org)]);
    for (id, team) in &value.snapshot.teams {
        let step = format!("team:{id}");
        let state = match team.create.outcome {
            TeamCreateOutcome::Blocked { reason } => Semantic::Blocked(reason),
            TeamCreateOutcome::OutcomeUnknown => Semantic::Unknown,
            TeamCreateOutcome::Held { .. } => held(value, &step)?,
            _ => Semantic::Other,
        };
        states.insert(step, state);
    }
    Ok(states)
}
fn step_kind(step: &str) -> Result<DeliveryAuditStep, TerminalError> {
    if step == "org" {
        return Ok(DeliveryAuditStep::Organization);
    }
    step.strip_prefix("team:")
        .and_then(|id| id.parse().ok())
        .map(|team_id| DeliveryAuditStep::Team { team_id })
        .ok_or_else(invalid)
}
fn aborted(value: &Retained, step: &str) -> bool {
    if step == "org" {
        matches!(
            value.snapshot.create.outcome,
            OrganizationCreateOutcome::Aborted { .. }
        )
    } else {
        step.strip_prefix("team:")
            .and_then(|id| id.parse::<u64>().ok())
            .and_then(|id| value.snapshot.teams.get(&id))
            .is_some_and(|t| matches!(t.create.outcome, TeamCreateOutcome::Aborted { .. }))
    }
}
fn transition(previous: Option<Semantic>, next: Semantic, aborted: bool) -> Vec<DeliveryAuditKind> {
    if previous == Some(next) {
        return vec![];
    }
    let mut events = Vec::with_capacity(2);
    if let Some(Semantic::Held { epoch }) = previous
        && !aborted
        && !matches!(next, Semantic::Held { .. })
    {
        events.push(DeliveryAuditKind::Released { hold_epoch: epoch });
    }
    events.extend(match next {
        Semantic::Blocked(reason) => Some(DeliveryAuditKind::Blocked {
            reason: DeliveryBlockedReason::Organization(reason),
        }),
        Semantic::Unknown => Some(DeliveryAuditKind::OutcomeUnknown),
        Semantic::Held { epoch } => Some(DeliveryAuditKind::Held { hold_epoch: epoch }),
        Semantic::Other => None,
    });
    events
}
pub(super) fn held_epoch_changed(value: &Retained, step: &str, epoch: u64) -> bool {
    value
        .audit
        .as_ref()
        .is_none_or(|old| old.cursor.steps.get(step) != Some(&Semantic::Held { epoch }))
}
pub(super) fn envelope(value: &Retained) -> Result<OrganizationDeliveryProjection, TerminalError> {
    let projection = OrganizationDeliveryProjection {
        snapshot: value.snapshot.clone(),
        audit_events: value
            .audit
            .as_ref()
            .map(|a| a.pending.clone())
            .unwrap_or_default(),
    };
    projection.events().map_err(|_| invalid())?;
    Ok(projection)
}
pub(super) async fn prepare(
    ctx: &ObjectContext<'_>,
    value: &mut Retained,
) -> Result<(), TerminalError> {
    if value.audit.as_ref().is_some_and(|a| !a.pending.is_empty()) {
        return Err(invalid());
    }
    let previous = value.audit.as_ref().map(|v| &v.cursor);
    let next = semantics(value)?;
    if previous.as_ref().is_some_and(|old| {
        old.revision > value.snapshot.revision
            || (old.revision == value.snapshot.revision && old.steps != next)
    }) {
        return Err(invalid());
    }
    let mut changes = Vec::new();
    for (step, state) in &next {
        changes.extend(
            transition(
                previous
                    .as_ref()
                    .and_then(|old| old.steps.get(step).copied()),
                *state,
                aborted(value, step),
            )
            .into_iter()
            .map(|kind| (step.clone(), kind)),
        );
    }
    if changes.len() > MAX_AUDIT_FACTS {
        return Err(invalid());
    }
    let mut audit_events = Vec::with_capacity(changes.len());
    if !changes.is_empty() {
        let at = crate::clock::now(ctx, "organization_audit_time").await?;
        if at < value.snapshot.create.command.approved_at {
            return Err(TerminalError::new_with_code(
                503,
                "organization audit clock precedes approval",
            ));
        }
        for (step, kind) in changes {
            audit_events.push(DeliveryAuditFact {
                step: step_kind(&step)?,
                transition_revision: value.snapshot.revision,
                at,
                kind,
            });
        }
    }
    value.audit = Some(AuditState {
        cursor: Cursor {
            revision: value.snapshot.revision,
            steps: next,
        },
        pending: audit_events,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_dedupe_but_reason_changes_and_reentry_are_distinct() {
        let seats = Semantic::Blocked(OrganizationBlockedReason::NoSeat);
        assert_eq!(transition(None, seats, false).len(), 1);
        assert!(transition(Some(seats), seats, false).is_empty());
        assert!(transition(Some(seats), Semantic::Other, false).is_empty());
        assert_eq!(transition(Some(Semantic::Other), seats, false).len(), 1);
        assert_eq!(
            transition(
                Some(seats),
                Semantic::Blocked(OrganizationBlockedReason::IdpSyncedTeam),
                false
            )
            .len(),
            1
        );
        assert!(transition(Some(Semantic::Unknown), Semantic::Unknown, false).is_empty());
    }
    #[test]
    fn new_hold_epoch_is_not_a_release_and_abort_preserves_aborted_meaning() {
        let held = Semantic::Held { epoch: 3 };
        assert_eq!(
            transition(Some(held), Semantic::Other, false),
            vec![DeliveryAuditKind::Released { hold_epoch: 3 }]
        );
        assert!(transition(Some(held), Semantic::Other, true).is_empty());
        assert_eq!(
            transition(Some(held), Semantic::Held { epoch: 5 }, false),
            vec![DeliveryAuditKind::Held { hold_epoch: 5 }]
        );
    }
}
