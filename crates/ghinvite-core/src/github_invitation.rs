use crate::ids::{GithubInvitationId, RequestId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationState {
    Sending,
    Sent,
    Accepted,
    Declined,
    Expired,
    Cancelled,
    Failed,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown invitation state: {0}")]
pub struct UnknownInvitationState(pub String);

impl FromStr for InvitationState {
    type Err = UnknownInvitationState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sending" => Ok(Self::Sending),
            "sent" => Ok(Self::Sent),
            "accepted" => Ok(Self::Accepted),
            "declined" => Ok(Self::Declined),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            other => Err(UnknownInvitationState(other.to_string())),
        }
    }
}

impl fmt::Display for InvitationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        })
    }
}

impl InvitationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Declined | Self::Expired | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GithubInvitation {
    pub id: GithubInvitationId,
    pub invitation_request_id: RequestId,
    pub repo_id: u64,
    /// Set after `PUT /repos/.../collaborators` returns 201 with an invitation id.
    /// Stays `None` if GitHub returned 204 (recipient already a member) or 4xx.
    pub github_invitation_id: Option<u64>,
    pub state: InvitationState,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        for s in [
            InvitationState::Sending,
            InvitationState::Sent,
            InvitationState::Accepted,
            InvitationState::Declined,
            InvitationState::Expired,
            InvitationState::Cancelled,
            InvitationState::Failed,
        ] {
            assert_eq!(s.to_string().parse::<InvitationState>().unwrap(), s);
        }
    }

    #[test]
    fn terminal_states() {
        assert!(!InvitationState::Sending.is_terminal());
        assert!(!InvitationState::Sent.is_terminal());
        assert!(InvitationState::Accepted.is_terminal());
        assert!(InvitationState::Declined.is_terminal());
        assert!(InvitationState::Expired.is_terminal());
        assert!(InvitationState::Cancelled.is_terminal());
        assert!(InvitationState::Failed.is_terminal());
    }
}
