//! Evidence decisions for one retained repository delivery. No I/O or mutation.
use chrono::{DateTime, Utc};
use ghinvite_core::{
    InvitationState,
    delivery::{CreateOutcome, DeliverySnapshot},
    targets::{EffectiveRole, RepoRole},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Keep the direct-collaborator requirement off until SP-07 step 8 is verified.
/// Even while off, the role that justified acceptance is retained as provenance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcceptancePolicy {
    pub require_direct_collaborator: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    Listed {
        upstream_id: u64,
        expired: bool,
        /// GitHub's creation time from this same listing. Older evidence
        /// without it can still settle on GitHub's own `expired` flag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_at: Option<DateTime<Utc>>,
    },
    FoundByInvitee {
        upstream_id: u64,
        permission: EffectiveRole,
    },
    Absent {
        availability_verified: bool,
        role_name: Option<EffectiveRole>,
        direct_collaborator: Option<bool>,
        membership: Option<MembershipState>,
    },
    FailedInvitation {
        upstream_id: u64,
        reason: FailedReason,
    },
}

// Organization observations are represented without applying organization rules
// to this repository reducer. Its acceptance rule never consults membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Active,
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailedReason {
    Expired,
    Other,
}

/// An observation cannot be replayed onto a newer revision or delivery generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_timing: Option<ghinvite_core::delivery::InvitationTiming>,
    pub delivery_key: String,
    pub expected_revision: u64,
    pub at: DateTime<Utc>,
    pub evidence: Evidence,
}
impl Observation {
    pub fn new(snapshot: &DeliverySnapshot, at: DateTime<Utc>, evidence: Evidence) -> Self {
        Self {
            invitation_timing: None,
            delivery_key: snapshot.create.command.delivery_key(),
            expected_revision: snapshot.revision,
            at,
            evidence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    ListedExpired,
    /// Still listed by an observation taken after GitHub's 7-day lifetime.
    ListedPastLifetime {
        created_at: DateTime<Utc>,
    },
    FailedInvitationExpired,
    EffectiveRole {
        role_name: EffectiveRole,
    },
    DirectCollaborator {
        role_name: EffectiveRole,
    },
    NoLongerPending {
        role_name: EffectiveRole,
        direct_collaborator: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileDecision {
    NoChange,
    ResolveCreated {
        upstream_id: u64,
    },
    Settle {
        state: InvitationState,
        provenance: Provenance,
    },
}

/// The owner applies the returned decision atomically with its revision/winner.
/// Absence is never a create receipt, and settlement never rewrites create history.
pub fn evaluate(
    snapshot: &DeliverySnapshot,
    observation: &Observation,
    policy: AcceptancePolicy,
) -> ReconcileDecision {
    use ReconcileDecision::{NoChange, ResolveCreated, Settle};
    if snapshot.tracking_stopped.is_some()
        || snapshot.settlement.is_some()
        || observation.delivery_key != snapshot.create.command.delivery_key()
        || observation.expected_revision != snapshot.revision
        || observation.at < snapshot.create.command.approved_at
    {
        return NoChange;
    }
    let upstream_id = match snapshot.create.outcome {
        CreateOutcome::OutcomeUnknown => {
            return match observation.evidence {
                Evidence::FoundByInvitee { upstream_id, .. } if upstream_id > 0 => {
                    ResolveCreated { upstream_id }
                }
                _ => NoChange,
            };
        }
        CreateOutcome::Created { upstream_id }
        | CreateOutcome::AlreadyPending { upstream_id, .. }
            if upstream_id > 0 =>
        {
            upstream_id
        }
        _ => return NoChange,
    };
    match &observation.evidence {
        Evidence::Listed {
            upstream_id: found,
            expired: true,
            ..
        } if *found == upstream_id => Settle {
            state: InvitationState::Expired,
            provenance: Provenance::ListedExpired,
        },
        // A fresh complete listing still shows the known invitation more than
        // GitHub's 7-day lifetime after creation: it can no longer be accepted,
        // whatever GitHub's own flag says. Absence is handled below and is
        // never Expired.
        Evidence::Listed {
            upstream_id: found,
            expired: false,
            created_at: Some(created_at),
        } if *found == upstream_id
            && ghinvite_core::delivery::past_github_lifetime(*created_at, observation.at) =>
        {
            Settle {
                state: InvitationState::Expired,
                provenance: Provenance::ListedPastLifetime {
                    created_at: *created_at,
                },
            }
        }
        Evidence::FailedInvitation {
            upstream_id: found,
            reason: FailedReason::Expired,
        } if *found == upstream_id => Settle {
            state: InvitationState::Expired,
            provenance: Provenance::FailedInvitationExpired,
        },
        Evidence::Absent {
            availability_verified: true,
            role_name: Some(role),
            direct_collaborator,
            ..
        } => {
            // A missing role or required direct observation is not negative evidence.
            if policy.require_direct_collaborator && direct_collaborator.is_none() {
                return NoChange;
            }
            if role.satisfies(RepoRole::from(snapshot.create.command.permission))
                && (!policy.require_direct_collaborator || *direct_collaborator == Some(true))
            {
                Settle {
                    state: InvitationState::Accepted,
                    provenance: if policy.require_direct_collaborator {
                        Provenance::DirectCollaborator {
                            role_name: role.clone(),
                        }
                    } else {
                        Provenance::EffectiveRole {
                            role_name: role.clone(),
                        }
                    },
                }
            } else {
                Settle {
                    state: InvitationState::Declined,
                    provenance: Provenance::NoLongerPending {
                        role_name: role.clone(),
                        direct_collaborator: *direct_collaborator,
                    },
                }
            }
        }
        _ => NoChange,
    }
}

/// Foreign pending invitations participate in reconciliation but never DELETE.
/// Cancellation of such a delivery stops tracking without touching GitHub.
pub fn can_withdraw(snapshot: &DeliverySnapshot) -> bool {
    snapshot.tracking_stopped.is_none()
        && snapshot.settlement.is_none()
        && matches!(snapshot.create.outcome, CreateOutcome::Created { upstream_id } if upstream_id > 0)
}

#[cfg(test)]
pub(crate) mod tests;
