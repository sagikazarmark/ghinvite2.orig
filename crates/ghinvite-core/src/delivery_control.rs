//! Pure M1 control policy. Effects are durable intents for workflow adapters,
//! never GitHub calls. Confirmed create and request-decision receipts are inputs
//! owned elsewhere and cannot be rewritten by this reducer.
use crate::{InvitationLinkId, RequestId, link_lifecycle::LifecycleOperationId};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Repository,
    Organization,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<InvitationLinkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<crate::bulk::BatchRef>,
    pub request_id: RequestId,
    pub kind: DeliveryKind,
    pub target_id: u64,
    pub delivery_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HoldWindow {
    /// The positive link control revision that began this auto-pause.
    pub epoch: u64,
    pub window_start: DateTime<Utc>,
    pub paused_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkControl {
    Active,
    ManualPause,
    AutoPause {
        window: HoldWindow,
    },
    Revoked {
        cancel_pending_invitations: bool,
        decline_pending_requests: bool,
        /// The still-active auto-pause at revocation. This must be carried even
        /// when the earlier hold has not reached the request/delivery yet.
        abort_hold: Option<HoldWindow>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlEnvelope {
    pub identity: Identity,
    pub operation_id: LifecycleOperationId,
    pub revision: u64,
    /// Greatest auto-pause epoch, including released epochs. Full snapshots
    /// prevent a delayed hold from reviving a released hold.
    pub hold_epoch: u64,
    pub at: DateTime<Utc>,
    pub control: LinkControl,
}

/// NoWriteProven is an authority assertion, not inferred from an empty list or
/// merely a Planned receipt. A claimed fence/lost run result is MayHaveWritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    NoWriteProven,
    MayHaveWritten,
    OwnedInvitation {
        upstream_id: u64,
        created_at: DateTime<Utc>,
    },
    ForeignInvitation {
        upstream_id: u64,
    },
    Final,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetDisposition {
    None,
    Unused { slot_id: String },
    ChargedOrUncertain,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub phase: Phase,
    pub delivery_revision: u64,
    pub last_observed_at: Option<DateTime<Utc>>,
    pub budget: BudgetDisposition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    LinkRevoked,
    OwnExpiry,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Disposition {
    Ready,
    Held {
        epoch: u64,
    },
    Aborted,
    RecoverUnknown,
    CancelOwned {
        upstream_id: u64,
        reason: CancelReason,
    },
    StopTrackingForeign,
    Final,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TimerStage {
    Scheduled,
    AwaitingEvidence,
    Complete,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnExpiryTimer {
    pub generation: u64,
    pub upstream_id: u64,
    pub due_at: DateTime<Utc>,
    scheduled_for: DateTime<Utc>,
    stage: TimerStage,
    requested_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct State {
    identity: Identity,
    admitted_at: DateTime<Utc>,
    invitation_lifetime_seconds: u32,
    accepted: Option<ControlEnvelope>,
    disposition: Disposition,
    wake_generation: u64,
    own_expiry: Option<OwnExpiryTimer>,
}
impl State {
    pub fn own_expiry_due(&self, generation: u64, upstream_id: u64, now: DateTime<Utc>) -> bool {
        self.own_expiry.as_ref().is_some_and(|t| {
            t.generation == generation
                && t.upstream_id == upstream_id
                && t.stage == TimerStage::Scheduled
                && now >= t.scheduled_for
        })
    }

    pub fn new(
        identity: Identity,
        admitted_at: DateTime<Utc>,
        invitation_lifetime_seconds: u32,
    ) -> Result<Self, Error> {
        validate_identity(&identity)?;
        if !(3600..=604800).contains(&invitation_lifetime_seconds) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            identity,
            admitted_at,
            invitation_lifetime_seconds,
            accepted: None,
            disposition: Disposition::Ready,
            wake_generation: 0,
            own_expiry: None,
        })
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn disposition(&self) -> &Disposition {
        &self.disposition
    }
    pub fn cancel_on_resolve(&self) -> bool {
        matches!(
            self.accepted.as_ref().map(|envelope| &envelope.control),
            Some(LinkControl::Revoked {
                cancel_pending_invitations: true,
                ..
            })
        )
    }
    pub fn own_expiry(&self) -> Option<&OwnExpiryTimer> {
        self.own_expiry.as_ref()
    }
    pub fn wake_generation(&self) -> u64 {
        self.wake_generation
    }
    pub fn accepts_wake(&self, generation: u64) -> bool {
        generation > 0
            && generation == self.wake_generation
            && matches!(
                self.disposition,
                Disposition::Ready | Disposition::RecoverUnknown
            )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpiryEvidence {
    StillPending,
    Accepted,
    Absent,
    Unavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Apply(ControlEnvelope),
    /// Re-evaluate after a durable attempt result or reconciliation. This never
    /// turns an uncertain phase into NoWriteProven on its own.
    Advance,
    TickOwnExpiry {
        generation: u64,
        upstream_id: u64,
    },
    OwnExpiryObserved {
        generation: u64,
        upstream_id: u64,
        expected_revision: u64,
        observed_at: DateTime<Utc>,
        evidence: ExpiryEvidence,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    RecordHeld {
        epoch: u64,
    },
    RecordAborted,
    ReleaseUnusedSlot {
        slot_id: String,
    },
    Wake {
        generation: u64,
    },
    RecoverReadOnly,
    /// A later, separately fenced/budgeted executor must validate live evidence.
    /// This enum is not permission to issue a DELETE directly.
    RequestCancellation {
        upstream_id: u64,
        reason: CancelReason,
    },
    /// Local final Unfulfillable disposition; preserve foreign create history
    /// and do not manufacture an upstream cancellation settlement.
    StopTrackingForeign,
    ScheduleOwnExpiry {
        generation: u64,
        upstream_id: u64,
        at: DateTime<Utc>,
    },
    ObserveOwnExpiry {
        generation: u64,
        upstream_id: u64,
    },
    RecordAccepted,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub state: State,
    pub effects: Vec<Effect>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid delivery control")]
    Invalid,
    #[error("delivery control identity conflict")]
    IdentityConflict,
    #[error("delivery control revision conflict")]
    RevisionConflict,
    #[error("delivery revocation is irreversible")]
    Revoked,
    #[error("inconsistent delivery facts")]
    InconsistentFacts,
    #[error("delivery control generation exhausted")]
    Exhausted,
}
fn validate_identity(id: &Identity) -> Result<(), Error> {
    crate::bulk::validate_origin(id.link_id, id.batch).map_err(|_| Error::Invalid)?;
    if [id.target_id, id.delivery_generation]
        .into_iter()
        .any(|v| v == 0 || v > crate::storage::integer::MAX_EXACT)
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn increment(value: u64) -> Result<u64, Error> {
    value
        .checked_add(1)
        .filter(|n| *n <= crate::storage::integer::MAX_EXACT)
        .ok_or(Error::Exhausted)
}
fn validate_envelope(envelope: &ControlEnvelope) -> Result<(), Error> {
    validate_identity(&envelope.identity)?;
    validate_link_control(
        envelope.revision,
        envelope.hold_epoch,
        envelope.at,
        &envelope.control,
    )
}
/// Validate the complete link snapshot before binding it to a request or delivery.
pub fn validate_link_control(
    revision: u64,
    hold_epoch: u64,
    at: DateTime<Utc>,
    control: &LinkControl,
) -> Result<(), Error> {
    if revision == 0 || revision > crate::storage::integer::MAX_EXACT || hold_epoch > revision {
        return Err(Error::Invalid);
    }
    let window = match control {
        LinkControl::AutoPause { window } => Some(window),
        LinkControl::Revoked { abort_hold, .. } => abort_hold.as_ref(),
        _ => None,
    };
    if let Some(window) = window
        && (window.epoch == 0
            || window.epoch != hold_epoch
            || window.window_start > window.paused_at
            || window.paused_at > at)
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn retry_expiry(
    timer: &mut OwnExpiryTimer,
    effects: &mut Vec<Effect>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    timer.generation = increment(timer.generation)?;
    timer.stage = TimerStage::Scheduled;
    timer.requested_at = None;
    timer.scheduled_for = now
        .checked_add_signed(Duration::seconds(60))
        .ok_or(Error::Invalid)?;
    effects.push(Effect::ScheduleOwnExpiry {
        generation: timer.generation,
        upstream_id: timer.upstream_id,
        at: now
            .checked_add_signed(Duration::seconds(60))
            .ok_or(Error::Invalid)?,
    });
    Ok(())
}
fn qualifies(state: &State, window: &HoldWindow) -> bool {
    state.admitted_at >= window.window_start && state.admitted_at <= window.paused_at
}
fn desired(state: &State, facts: &Facts) -> Disposition {
    if matches!(facts.phase, Phase::Final) {
        return Disposition::Final;
    }
    let (hold, abort, cancel) = match state.accepted.as_ref().map(|s| &s.control) {
        Some(LinkControl::AutoPause { window }) => (
            qualifies(state, window).then_some(window.epoch),
            false,
            false,
        ),
        Some(LinkControl::Revoked {
            cancel_pending_invitations,
            abort_hold,
            ..
        }) => (
            None,
            *cancel_pending_invitations || abort_hold.as_ref().is_some_and(|w| qualifies(state, w)),
            *cancel_pending_invitations,
        ),
        _ => (None, false, false),
    };
    match facts.phase {
        Phase::NoWriteProven if abort => Disposition::Aborted,
        Phase::NoWriteProven if hold.is_some() => Disposition::Held {
            epoch: hold.unwrap(),
        },
        Phase::NoWriteProven => Disposition::Ready,
        Phase::MayHaveWritten => Disposition::RecoverUnknown,
        Phase::OwnedInvitation { upstream_id, .. } if cancel => Disposition::CancelOwned {
            upstream_id,
            reason: CancelReason::LinkRevoked,
        },
        Phase::ForeignInvitation { .. } if cancel => Disposition::StopTrackingForeign,
        Phase::OwnedInvitation { .. } | Phase::ForeignInvitation { .. } => Disposition::Ready,
        Phase::Final => Disposition::Final,
    }
}
/// `now` must be the handler's journaled clock. The caller atomically persists
/// this state and arranges effects before acknowledging an operation receipt.
/// Equal-revision duplicates emit nothing; old envelopes cannot undo controls.
pub fn reduce(
    state: &State,
    facts: &Facts,
    event: Event,
    now: DateTime<Utc>,
) -> Result<Transition, Error> {
    validate_identity(&state.identity)?;
    if facts.delivery_revision == 0
        || facts.delivery_revision > crate::storage::integer::MAX_EXACT
        || !(3600..=604800).contains(&state.invitation_lifetime_seconds)
    {
        return Err(Error::Invalid);
    }
    if matches!(
        facts.phase,
        Phase::OwnedInvitation { upstream_id, .. } | Phase::ForeignInvitation { upstream_id }
            if upstream_id == 0 || upstream_id > crate::storage::integer::MAX_EXACT
    ) {
        return Err(Error::Invalid);
    }
    let mut next = state.clone();
    let mut effects = vec![];
    if let Event::Apply(envelope) = &event {
        validate_envelope(envelope)?;
        if envelope.identity != state.identity {
            return Err(Error::IdentityConflict);
        }
        if let Some(old) = &state.accepted {
            if envelope.revision == old.revision {
                return if envelope == old {
                    Ok(Transition {
                        state: next,
                        effects,
                    })
                } else {
                    Err(Error::RevisionConflict)
                };
            }
            if envelope.revision < old.revision {
                return Ok(Transition {
                    state: next,
                    effects,
                });
            }
            if matches!(old.control, LinkControl::Revoked { .. }) {
                return Err(Error::Revoked);
            }
            if envelope.hold_epoch < old.hold_epoch {
                return Err(Error::RevisionConflict);
            }
            // A released epoch cannot be reused by a later auto-pause.
            if let LinkControl::AutoPause { window }
            | LinkControl::Revoked {
                abort_hold: Some(window),
                ..
            } = &envelope.control
            {
                let continuing = matches!(&old.control,LinkControl::AutoPause{window:previous} if previous==window);
                if window.epoch <= old.hold_epoch && !continuing {
                    return Err(Error::RevisionConflict);
                }
            }
        }
        next.accepted = Some(envelope.clone());
    }
    if matches!(
        state.disposition,
        Disposition::Aborted | Disposition::StopTrackingForeign | Disposition::Final
    ) {
        if matches!(state.disposition, Disposition::Aborted)
            && !matches!(facts.phase, Phase::NoWriteProven | Phase::Final)
        {
            return Err(Error::InconsistentFacts);
        }
        return Ok(Transition {
            state: next,
            effects,
        });
    }
    let mut disposition = desired(&next, facts);
    // A retained cancellation intent cannot be cleared by an ordinary fact
    // refresh. Terminal evidence remains the only completion authority.
    if let Disposition::CancelOwned { .. } = state.disposition
        && !matches!(facts.phase, Phase::Final)
    {
        disposition = state.disposition.clone();
    }
    if disposition != state.disposition {
        next.wake_generation = increment(state.wake_generation)?;
        match &disposition {
            Disposition::Held { epoch } => effects.push(Effect::RecordHeld { epoch: *epoch }),
            Disposition::Aborted => {
                effects.push(Effect::RecordAborted);
                if let BudgetDisposition::Unused { slot_id } = &facts.budget {
                    effects.push(Effect::ReleaseUnusedSlot {
                        slot_id: slot_id.clone(),
                    });
                }
            }
            Disposition::Ready
                if matches!(state.disposition, Disposition::Held { .. })
                    && matches!(facts.phase, Phase::NoWriteProven) =>
            {
                effects.push(Effect::Wake {
                    generation: next.wake_generation,
                })
            }
            Disposition::RecoverUnknown => effects.push(Effect::RecoverReadOnly),
            Disposition::CancelOwned {
                upstream_id,
                reason,
            } => effects.push(Effect::RequestCancellation {
                upstream_id: *upstream_id,
                reason: *reason,
            }),
            Disposition::StopTrackingForeign => effects.push(Effect::StopTrackingForeign),
            _ => {}
        }
        next.disposition = disposition;
    } else if matches!(event, Event::Apply(_))
        && matches!(next.disposition, Disposition::RecoverUnknown)
        && matches!(
            next.accepted.as_ref().map(|e| &e.control),
            Some(LinkControl::Revoked {
                cancel_pending_invitations: true,
                ..
            })
        )
    {
        effects.push(Effect::RecoverReadOnly);
    }
    if matches!(
        next.disposition,
        Disposition::CancelOwned { .. }
            | Disposition::Aborted
            | Disposition::StopTrackingForeign
            | Disposition::Final
    ) {
        return Ok(Transition {
            state: next,
            effects,
        });
    }
    if let Phase::OwnedInvitation {
        upstream_id,
        created_at,
    } = facts.phase
    {
        // GitHub creation timestamps have whole-second precision.
        if created_at.timestamp() < state.admitted_at.timestamp() || created_at > now {
            return Err(Error::InconsistentFacts);
        }
        if state.invitation_lifetime_seconds < 604800 && next.own_expiry.is_none() {
            let due_at = created_at
                .checked_add_signed(Duration::seconds(i64::from(
                    state.invitation_lifetime_seconds,
                )))
                .ok_or(Error::Invalid)?;
            next.own_expiry = Some(OwnExpiryTimer {
                generation: 1,
                upstream_id,
                due_at,
                scheduled_for: due_at,
                stage: TimerStage::Scheduled,
                requested_at: None,
            });
            effects.push(Effect::ScheduleOwnExpiry {
                generation: 1,
                upstream_id,
                at: due_at,
            });
        }
        if let Some(timer) = next.own_expiry.as_mut() {
            if timer.upstream_id != upstream_id
                || created_at.checked_add_signed(Duration::seconds(i64::from(
                    state.invitation_lifetime_seconds,
                ))) != Some(timer.due_at)
            {
                return Err(Error::InconsistentFacts);
            }
            match event {
                Event::TickOwnExpiry {
                    generation,
                    upstream_id: id,
                } if generation == timer.generation
                    && id == upstream_id
                    && now >= timer.scheduled_for
                    && timer.stage == TimerStage::Scheduled =>
                {
                    timer.stage = TimerStage::AwaitingEvidence;
                    timer.requested_at = Some(now);
                    effects.push(Effect::ObserveOwnExpiry {
                        generation,
                        upstream_id,
                    });
                }
                Event::OwnExpiryObserved {
                    generation,
                    upstream_id: id,
                    expected_revision,
                    observed_at,
                    evidence,
                } if generation == timer.generation
                    && id == upstream_id
                    && timer.stage == TimerStage::AwaitingEvidence
                    && expected_revision == facts.delivery_revision
                    && timer.requested_at.is_some_and(|at| observed_at >= at)
                    && observed_at >= timer.due_at
                    && observed_at >= created_at
                    && observed_at <= now
                    && facts.last_observed_at.is_none_or(|at| observed_at >= at) =>
                {
                    match evidence {
                        ExpiryEvidence::StillPending => {
                            timer.stage = TimerStage::Complete;
                            next.disposition = Disposition::CancelOwned {
                                upstream_id,
                                reason: CancelReason::OwnExpiry,
                            };
                            effects.push(Effect::RequestCancellation {
                                upstream_id,
                                reason: CancelReason::OwnExpiry,
                            });
                        }
                        ExpiryEvidence::Accepted => {
                            timer.stage = TimerStage::Complete;
                            effects.push(Effect::RecordAccepted);
                        }
                        ExpiryEvidence::Absent | ExpiryEvidence::Unavailable => {
                            if evidence == ExpiryEvidence::Absent {
                                effects.push(Effect::RecoverReadOnly);
                            }
                            retry_expiry(timer, &mut effects, now)?;
                        }
                    }
                }
                Event::OwnExpiryObserved {
                    generation,
                    upstream_id: id,
                    ..
                } if generation == timer.generation
                    && id == upstream_id
                    && timer.stage == TimerStage::AwaitingEvidence =>
                {
                    // A stale response cannot settle, but must not strand the
                    // only effective timer in AwaitingEvidence indefinitely.
                    retry_expiry(timer, &mut effects, now)?;
                }
                _ => {}
            }
        }
    }
    Ok(Transition {
        state: next,
        effects,
    })
}
