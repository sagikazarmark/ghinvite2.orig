use crate::ids::{InvitationLinkId, RequestId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Pending,
    Approved,
    Declined,
    Expired,
    /// Reserved for v2 (link revoke cascading); not produced by v1 code paths.
    Cancelled,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown request state: {0}")]
pub struct UnknownRequestState(pub String);

impl FromStr for RequestState {
    type Err = UnknownRequestState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "declined" => Ok(Self::Declined),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(UnknownRequestState(other.to_string())),
        }
    }
}

impl fmt::Display for RequestState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        })
    }
}

impl RequestState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationRequest {
    pub id: RequestId,
    pub invitation_link_id: InvitationLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub state: RequestState,
    pub decided_by: Option<u64>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Persisted admission deadline, independent of invitation-link expiration.
    /// None means no recorded deadline (auto-approval or missing historical data).
    pub decision_deadline: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_string() {
        for s in [
            RequestState::Pending,
            RequestState::Approved,
            RequestState::Declined,
            RequestState::Expired,
            RequestState::Cancelled,
        ] {
            assert_eq!(s.to_string().parse::<RequestState>().unwrap(), s);
        }
    }

    #[test]
    fn pending_is_only_non_terminal() {
        assert!(!RequestState::Pending.is_terminal());
        assert!(RequestState::Approved.is_terminal());
        assert!(RequestState::Declined.is_terminal());
        assert!(RequestState::Expired.is_terminal());
        assert!(RequestState::Cancelled.is_terminal());
    }

    #[test]
    fn rejects_unknown_state() {
        assert_eq!(
            "approving".parse::<RequestState>(),
            Err(UnknownRequestState("approving".into()))
        );
    }
}
